//! Everything oxtel-rmcp records is named as the OpenTelemetry MCP
//! conventions define it (in the genai registry, which builds on the main
//! one): every span attribute and metric label is a defined attribute, and
//! every metric matches its definition.

use oxtel_rmcp::Instrumented;
use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{MeterProvider, MetricKind};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, TracerProvider};
use oxtel_semconv::{Instrument, attribute, genai};
use rmcp::model::{CallToolRequestParams, CallToolResult, Content, ErrorData};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ServerHandler, ServiceExt};
use tracing_subscriber::layer::SubscriberExt;

struct Tools;

impl ServerHandler for Tools {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match request.name.as_ref() {
            "echo" => Ok(CallToolResult::success(vec![Content::text("hi")])),
            "broken" => Ok(CallToolResult::error(vec![Content::text("no")])),
            _ => Err(ErrorData::internal_error("down", None)),
        }
    }
}

fn defined(key: &str) -> bool {
    key.starts_with("otel.") || genai::attribute::is_defined(key) || attribute::is_defined(key)
}

#[tokio::test(flavor = "current_thread")]
async fn spans_and_metrics_use_only_defined_names() {
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
    let client = ().serve(client_io).await.unwrap();
    for tool in ["echo", "broken", "crash"] {
        let _ = client.call_tool(CallToolRequestParams::new(tool)).await;
    }
    client.cancel().await.unwrap();
    server.await.unwrap();

    for span in spans.finished_spans() {
        for kv in &span.attributes {
            assert!(
                defined(&kv.key),
                "span {:?} has attribute {:?}, which the conventions don't define",
                span.name,
                kv.key
            );
        }
    }
    let families = meters.collect();
    assert!(
        families
            .iter()
            .any(|f| f.name == "mcp.server.session.duration")
    );
    for family in &families {
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
