//! An instrumented rmcp server behind rmcp's streamable HTTP server, and a
//! real rmcp client calling it over HTTP.

use std::sync::Arc;

use oxtel_rmcp::InstrumentedHandler;
use oxtel_sdk::Resource;
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, SpanKind, TracerProvider};
use oxtel_sdk::{SpanId, TraceId, Value};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientConfig, ContentBlock, ErrorData,
    ProtocolVersion, RequestMetaObject,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpClientTransport, StreamableHttpService};
use rmcp::{ServerHandler, ServiceExt};
use tracing_subscriber::layer::SubscriberExt;

#[derive(Clone)]
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
            _ => Err(ErrorData::invalid_params("no such tool", None)),
        }
    }
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| &kv.value)
}

fn text(span: &SpanData, key: &str) -> Option<String> {
    attr(span, key).map(|v| match v {
        Value::String(s) => s.to_string(),
        other => format!("{other:?}"),
    })
}

fn named<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
    spans.iter().find(|s| s.name == name).unwrap_or_else(|| {
        panic!(
            "no span {name:?} in {:?}",
            spans.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    })
}

/// Serve `Tools`, instrumented, at /mcp on a port of its own, and call
/// `echo` and `broken` through an rmcp client, `echo` with a trace of the
/// caller's. The client asks for `protocol`, or rmcp's latest. Returns the
/// server's spans, the caller's ids, and the protocol version the client
/// and server agreed on.
async fn call_over_http(
    protocol: Option<ProtocolVersion>,
) -> (Vec<SpanData>, TraceId, SpanId, String) {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(spans.clone())
        .build();
    let _subscriber =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(traces.layer()));

    let mcp = StreamableHttpService::new(
        || Ok(InstrumentedHandler::new(Tools)),
        Arc::new(LocalSessionManager::default()),
        Default::default(),
    );
    let app = axum::Router::new().nest_service("/mcp", mcp);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let transport = StreamableHttpClientTransport::from_uri(format!("http://{address}/mcp"));
    let mut config = ClientConfig::default();
    if let Some(protocol) = protocol {
        config.protocol_version = protocol;
    }
    let client = config.serve(transport).await.expect("client connects");
    let agreed = client
        .peer_info()
        .expect("initialized")
        .protocol_version
        .as_str()
        .to_string();
    let (trace_id, span_id) = (TraceId::random(), SpanId::random());
    let mut echo = CallToolRequestParams::new("echo");
    let mut meta = RequestMetaObject::new();
    meta.0
        .set_traceparent(format!("00-{trace_id}-{span_id}-01"));
    echo.meta = Some(meta);
    client.call_tool(echo).await.unwrap();
    client
        .call_tool(CallToolRequestParams::new("broken"))
        .await
        .unwrap();
    client.cancel().await.unwrap();
    server.abort();
    (spans.finished_spans(), trace_id, span_id, agreed)
}

#[tokio::test(flavor = "current_thread")]
async fn tool_calls_over_http_continue_the_callers_trace() {
    let (spans, trace_id, span_id, agreed) = call_over_http(None).await;
    let echo = named(&spans, "tools/call echo");
    assert_eq!(echo.kind, SpanKind::Server);
    assert_eq!(echo.context.trace_id, trace_id);
    assert_eq!(echo.parent_span_id, Some(span_id));
    for (key, value) in [
        ("mcp.method.name", "tools/call"),
        ("gen_ai.tool.name", "echo"),
        ("gen_ai.operation.name", "execute_tool"),
        ("network.transport", "tcp"),
        ("network.protocol.name", "http"),
        ("network.protocol.version", "1.1"),
    ] {
        assert_eq!(text(echo, key).as_deref(), Some(value), "{key}");
    }
    assert!(attr(echo, "jsonrpc.request.id").is_some());
    // The version the session agreed on, not the one the client asked for.
    for span in [named(&spans, "initialize"), echo] {
        assert_eq!(
            text(span, "mcp.protocol.version").as_deref(),
            Some(agreed.as_str()),
            "{}",
            span.name
        );
    }
    assert!(attr(echo, "error.type").is_none());

    let broken = named(&spans, "tools/call broken");
    assert_eq!(text(broken, "error.type").as_deref(), Some("tool_error"));
    // No trace of the caller's: a trace of its own, not a child of
    // whatever span rmcp's HTTP server runs it in.
    assert_ne!(broken.context.trace_id, trace_id);
    assert_eq!(broken.parent_span_id, None);
}

#[tokio::test(flavor = "current_thread")]
async fn sessions_over_http_are_recorded() {
    // Before 2026-07-28, a client starts a session with `initialize`, and
    // the server names it in the Mcp-Session-Id header.
    let (spans, _, _, _) = call_over_http(Some(ProtocolVersion::V_2025_11_25)).await;
    let initialize = named(&spans, "initialize");
    assert_eq!(initialize.parent_span_id, None);
    let echo = named(&spans, "tools/call echo");
    assert_eq!(
        text(echo, "mcp.protocol.version").as_deref(),
        Some("2025-11-25")
    );
    let session = text(echo, "mcp.session.id").expect("a session id");
    assert!(!session.is_empty());
    assert_eq!(
        text(named(&spans, "tools/call broken"), "mcp.session.id"),
        Some(session),
        "one session"
    );
}
