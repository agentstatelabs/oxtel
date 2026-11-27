//! Everything oxtel-rmcp records is named as the OpenTelemetry MCP
//! conventions define it (in the genai registry, which builds on the main
//! one): every span attribute and metric label is a defined attribute, and
//! every metric matches its definition.

use std::future::Future;

use oxtel_rmcp::Instrumented;
use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{MeterProvider, MetricFamily, MetricKind};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, TracerProvider};
use oxtel_semconv::{Instrument, attribute, genai};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
};
use rmcp::service::{RequestContext, RoleClient, RoleServer, RunningService};
use rmcp::{ServerHandler, ServiceExt};
use tracing_subscriber::layer::SubscriberExt;

struct Tools;

impl ServerHandler for Tools {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match request.name.as_ref() {
            "echo" => Ok(CallToolResult::success(vec![ContentBlock::text("hi")]).into()),
            "broken" => Ok(CallToolResult::error(vec![ContentBlock::text("no")]).into()),
            _ => Err(ErrorData::internal_error("down", None)),
        }
    }
}

fn defined(key: &str) -> bool {
    key.starts_with("otel.") || genai::attribute::is_defined(key) || attribute::is_defined(key)
}

/// Every span attribute and metric label is a defined attribute, and every
/// metric matches its definition. The names recorded are kept in
/// tests/recorded/`recorded`.txt for the site's reference:
/// OXTEL_REWRITE_RECORDED=1 rewrites the file when a change is meant.
fn check_names(spans: &[SpanData], families: &[MetricFamily], recorded: &str) {
    oxtel_sdk::testing::assert_recorded_names(
        format!(
            "{}/tests/recorded/{recorded}.txt",
            env!("CARGO_MANIFEST_DIR")
        ),
        spans,
        families,
        std::env::var_os("OXTEL_REWRITE_RECORDED").is_some(),
    );
    for span in spans {
        for kv in &span.attributes {
            assert!(
                defined(&kv.key),
                "span {:?} has attribute {:?}, which the conventions don't define",
                span.name,
                kv.key
            );
        }
    }
    for family in families {
        let definition = genai::metric::by_name(&family.name)
            .unwrap_or_else(|| panic!("{} isn't a defined metric", family.name));
        assert_eq!(
            match family.kind {
                MetricKind::Histogram => Instrument::Histogram,
                MetricKind::Counter => Instrument::Counter,
                MetricKind::Gauge => Instrument::Gauge,
            },
            definition.instrument,
            "{}",
            family.name
        );
        assert_eq!(
            family.unit.as_deref(),
            Some(definition.unit),
            "{}",
            family.name
        );
        for point in &family.points {
            for (key, _) in &point.attributes {
                assert!(defined(key), "{} has label {key:?}", family.name);
            }
        }
    }
}

/// A session with an instrumented server over a pipe, recorded: `calls`
/// makes the client's calls.
async fn session<F>(
    calls: impl FnOnce(RunningService<RoleClient, ()>) -> F,
) -> (Vec<SpanData>, Vec<MetricFamily>)
where
    F: Future<Output = ()>,
{
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(spans.clone())
        .build();
    let meters = MeterProvider::builder().build();
    let recorder: &'static _ = Box::leak(Box::new(meters.recorder()));
    let _metrics = metrics::set_default_local_recorder(recorder);
    let _tracing =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(traces.layer()));

    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let running = Instrumented::new(Tools)
            .with_transport("pipe")
            .with_payloads(true)
            .serve(server_io)
            .await
            .unwrap();
        let _ = running.waiting().await;
    });
    calls(().serve(client_io).await.unwrap()).await;
    server.await.unwrap();
    (spans.finished_spans(), meters.collect())
}

#[tokio::test(flavor = "current_thread")]
async fn spans_and_metrics_use_only_defined_names() {
    let (spans, families) = session(|client| async move {
        for tool in ["echo", "broken", "crash"] {
            let _ = client.call_tool(CallToolRequestParams::new(tool)).await;
        }
        client.cancel().await.unwrap();
    })
    .await;
    assert!(
        families
            .iter()
            .any(|f| f.name == "mcp.server.session.duration")
    );
    check_names(&spans, &families, "conventions");
}

/// The client side's spans and metric, through oxtel-rmcp's client.
#[cfg(feature = "client")]
#[tokio::test(flavor = "current_thread")]
async fn client_calls_use_only_defined_names() {
    let (spans, families) = session(|client| async move {
        for tool in ["echo", "broken", "crash"] {
            let _ = oxtel_rmcp::client::call_tool(
                client.peer(),
                CallToolRequestParams::new(tool),
                Default::default(),
            )
            .await;
        }
        client.cancel().await.unwrap();
    })
    .await;
    assert!(
        families
            .iter()
            .any(|f| f.name == "mcp.client.operation.duration")
    );
    check_names(&spans, &families, "client");
}
