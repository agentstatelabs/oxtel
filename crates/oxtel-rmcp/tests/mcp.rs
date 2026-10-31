//! A real rmcp client and an instrumented rmcp server, talking over an
//! in-memory pipe.

use oxtel_rmcp::Instrumented;
use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{MeterProvider, MetricFamily, PointValue};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, SpanKind, Status, TracerProvider};
use oxtel_sdk::{SpanId, TraceId, Value};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    RequestMetaObject,
};
use rmcp::service::{RequestContext, RoleClient, RoleServer, RunningService};
use rmcp::{ServerHandler, ServiceExt};
use tracing_subscriber::layer::SubscriberExt;

/// Tools that succeed, fail as tools, fail the server, or don't exist.
struct Tools;

impl ServerHandler for Tools {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match request.name.as_ref() {
            "echo" => Ok(CallToolResult::success(vec![ContentBlock::text("hello")]).into()),
            "broken" => Ok(CallToolResult::error(vec![ContentBlock::text("disk full")]).into()),
            "crash" => Err(ErrorData::internal_error("store unavailable", None)),
            _ => Err(ErrorData::invalid_params("no such tool", None)),
        }
    }
}

struct Harness {
    spans: InMemorySpanExporter,
    meters: MeterProvider,
    _traces: TracerProvider,
    _guards: (
        tracing::subscriber::DefaultGuard,
        metrics::LocalRecorderGuard<'static>,
    ),
}

fn harness() -> Harness {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(spans.clone())
        .build();
    let meters = MeterProvider::builder().build();
    let recorder: &'static _ = Box::leak(Box::new(meters.recorder()));
    let guards = (
        tracing::subscriber::set_default(tracing_subscriber::registry().with(traces.layer())),
        metrics::set_default_local_recorder(recorder),
    );
    Harness {
        spans,
        meters,
        _traces: traces,
        _guards: guards,
    }
}

/// Connect a plain rmcp client to `server` over an in-memory pipe.
async fn connect(
    server: Instrumented<Tools>,
) -> (RunningService<RoleClient, ()>, tokio::task::JoinHandle<()>) {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("server starts");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client connects");
    (client, server)
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| &kv.value)
}

fn named<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
    spans.iter().find(|s| s.name == name).unwrap_or_else(|| {
        panic!(
            "no span {name:?} in {:?}",
            spans.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    })
}

fn call(name: &'static str) -> CallToolRequestParams {
    CallToolRequestParams::new(name)
}

#[tokio::test(flavor = "current_thread")]
async fn tool_calls_become_server_spans_that_continue_the_callers_trace() {
    let h = harness();
    let (client, server) = connect(Instrumented::new(Tools).with_transport("pipe")).await;
    let (trace_id, span_id) = (TraceId::random(), SpanId::random());
    let mut params = call("echo");
    let mut meta = RequestMetaObject::new();
    meta.0
        .set_traceparent(format!("00-{trace_id}-{span_id}-01"));
    params.meta = Some(meta);
    client.call_tool(params).await.unwrap();
    client.cancel().await.unwrap();
    server.await.unwrap();

    let spans = h.spans.finished_spans();
    let echo = named(&spans, "tools/call echo");
    assert_eq!(echo.kind, SpanKind::Server);
    assert_eq!(echo.context.trace_id, trace_id);
    assert_eq!(echo.parent_span_id, Some(span_id));
    for (key, value) in [
        ("mcp.method.name", "tools/call"),
        ("gen_ai.tool.name", "echo"),
        ("gen_ai.operation.name", "execute_tool"),
        ("network.transport", "pipe"),
    ] {
        assert_eq!(attr(echo, key), Some(&Value::from(value)), "{key}");
    }
    assert!(attr(echo, "jsonrpc.request.id").is_some());
    assert!(
        matches!(attr(echo, "mcp.protocol.version"), Some(Value::String(v)) if v.starts_with("20")),
        "{:?}",
        attr(echo, "mcp.protocol.version")
    );
    assert!(
        attr(echo, "gen_ai.tool.call.arguments").is_none(),
        "payloads are opt-in"
    );
    assert_eq!(echo.status, Status::Unset);
    named(&spans, "initialize");
    named(&spans, "notifications/initialized");
}

#[tokio::test(flavor = "current_thread")]
async fn failures_follow_the_conventions() {
    let h = harness();
    let (client, server) = connect(Instrumented::new(Tools)).await;
    client.call_tool(call("broken")).await.unwrap();
    assert!(client.call_tool(call("crash")).await.is_err());
    assert!(client.call_tool(call("nope")).await.is_err());
    client.cancel().await.unwrap();
    server.await.unwrap();
    let spans = h.spans.finished_spans();

    let broken = named(&spans, "tools/call broken");
    assert_eq!(attr(broken, "error.type"), Some(&Value::from("tool_error")));
    assert!(matches!(broken.status, Status::Error { .. }));

    let crash = named(&spans, "tools/call crash");
    assert_eq!(
        attr(crash, "rpc.response.status_code"),
        Some(&Value::from("-32603"))
    );
    assert_eq!(attr(crash, "error.type"), Some(&Value::from("-32603")));
    assert_eq!(
        crash.status,
        Status::Error {
            description: "store unavailable".into()
        }
    );

    let nope = named(&spans, "tools/call nope");
    assert_eq!(
        attr(nope, "rpc.response.status_code"),
        Some(&Value::from("-32602"))
    );
    assert!(
        attr(nope, "error.type").is_none(),
        "the caller's mistake, not the server's"
    );
    assert_eq!(nope.status, Status::Unset);
}

#[tokio::test(flavor = "current_thread")]
async fn payloads_are_recorded_only_when_asked() {
    let h = harness();
    let (client, server) = connect(Instrumented::new(Tools).with_payloads(true)).await;
    let mut params = call("echo");
    params.arguments = Some(
        serde_json::json!({"text": "hi"})
            .as_object()
            .unwrap()
            .clone(),
    );
    client.call_tool(params).await.unwrap();
    client.cancel().await.unwrap();
    server.await.unwrap();
    let spans = h.spans.finished_spans();
    let echo = named(&spans, "tools/call echo");
    assert_eq!(
        attr(echo, "gen_ai.tool.call.arguments"),
        Some(&Value::from(r#"{"text":"hi"}"#))
    );
    assert!(
        matches!(attr(echo, "gen_ai.tool.call.result"), Some(Value::String(r)) if r.contains("hello"))
    );
}

fn histogram<'a>(metrics: &'a [MetricFamily], name: &str) -> &'a MetricFamily {
    metrics
        .iter()
        .find(|m| m.name == name)
        .unwrap_or_else(|| panic!("no {name}"))
}

#[tokio::test(flavor = "current_thread")]
async fn operations_and_sessions_are_measured() {
    let h = harness();
    let (client, server) = connect(Instrumented::new(Tools).with_transport("pipe")).await;
    client.call_tool(call("echo")).await.unwrap();
    client.call_tool(call("echo")).await.unwrap();
    assert!(client.call_tool(call("crash")).await.is_err());
    client.cancel().await.unwrap();
    server.await.unwrap();
    let metrics = h.meters.collect();

    let operations = histogram(&metrics, "mcp.server.operation.duration");
    assert_eq!(operations.unit.as_deref(), Some("s"));
    let echo = operations
        .points
        .iter()
        .find(|p| {
            p.attributes
                .contains(&("gen_ai.tool.name".into(), "echo".into()))
        })
        .unwrap();
    assert!(matches!(&echo.value, PointValue::Histogram(h) if h.count() == 2));
    let keys: Vec<&str> = echo.attributes.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "gen_ai.operation.name",
            "gen_ai.tool.name",
            "mcp.method.name",
            "mcp.protocol.version",
            "network.transport"
        ]
    );
    assert!(operations.points.iter().any(|p| {
        p.attributes
            .contains(&("error.type".into(), "-32603".into()))
    }));

    let sessions = histogram(&metrics, "mcp.server.session.duration");
    assert!(matches!(&sessions.points[0].value, PointValue::Histogram(h) if h.count() == 1));
}

#[cfg(feature = "client")]
mod client_side {
    use super::*;
    use oxtel_rmcp::client::{ClientOptions, call_tool};

    #[tokio::test(flavor = "current_thread")]
    async fn client_spans_carry_the_trace_to_the_server() {
        let h = harness();
        let (client, server) = connect(Instrumented::new(Tools)).await;
        let parent = tracing::info_span!("agent step");
        {
            let _entered = parent.enter();
            call_tool(client.peer(), call("echo"), ClientOptions::default())
                .await
                .unwrap();
            assert!(
                call_tool(client.peer(), call("crash"), ClientOptions::default())
                    .await
                    .is_err()
            );
        }
        drop(parent);
        client.cancel().await.unwrap();
        server.await.unwrap();

        let spans = h.spans.finished_spans();
        let step = named(&spans, "agent step");
        let clients: Vec<&SpanData> = spans
            .iter()
            .filter(|s| s.kind == SpanKind::Client)
            .collect();
        let echo_client = clients
            .iter()
            .find(|s| s.name == "tools/call echo")
            .unwrap();
        assert_eq!(echo_client.parent_span_id, Some(step.context.span_id));
        let echo_server = spans
            .iter()
            .find(|s| s.kind == SpanKind::Server && s.name == "tools/call echo")
            .unwrap();
        assert_eq!(
            echo_server.parent_span_id,
            Some(echo_client.context.span_id),
            "the server continues the client's span through params._meta"
        );
        assert_eq!(echo_server.context.trace_id, step.context.trace_id);

        let crash_client = clients
            .iter()
            .find(|s| s.name == "tools/call crash")
            .unwrap();
        assert_eq!(
            attr(crash_client, "error.type"),
            Some(&Value::from("-32603"))
        );

        let metrics = h.meters.collect();
        let operations = histogram(&metrics, "mcp.client.operation.duration");
        assert_eq!(operations.unit.as_deref(), Some("s"));
        assert_eq!(
            operations.points.len(),
            2,
            "one series each for echo and crash"
        );
    }
}
