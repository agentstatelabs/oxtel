//! Requests through a real reqwest client to a local server.

use std::net::SocketAddr;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{MeterProvider, MetricKind, PointValue};
use oxtel_sdk::propagation::extract_trace_context;
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, SpanKind, Status, TracerProvider};
use oxtel_semconv::{Instrument, attribute, metric};
use tracing_subscriber::layer::SubscriberExt;

struct Telemetry {
    spans: InMemorySpanExporter,
    meters: MeterProvider,
    _traces: TracerProvider,
    _tracing: tracing::subscriber::DefaultGuard,
    _metrics: metrics::LocalRecorderGuard<'static>,
}

/// Spans and metrics recorded in memory, on this thread. The tests run on a
/// current-thread runtime, so the server and client tasks are on it too.
fn telemetry() -> Telemetry {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(spans.clone())
        .build();
    let meters = MeterProvider::builder().build();
    let recorder: &'static _ = Box::leak(Box::new(meters.recorder()));
    Telemetry {
        spans,
        meters,
        _metrics: metrics::set_default_local_recorder(recorder),
        _tracing: tracing::subscriber::set_default(
            tracing_subscriber::registry().with(traces.layer()),
        ),
        _traces: traces,
    }
}

/// A server that answers with the `traceparent` it was sent, if any, and
/// 404 for `/missing`.
async fn server() -> SocketAddr {
    let app = Router::new()
        .route(
            "/items",
            get(|headers: HeaderMap| async move {
                headers
                    .get("traceparent")
                    .map(|v| v.to_str().unwrap().to_string())
                    .unwrap_or_default()
            }),
        )
        .route("/missing", get(|| async { StatusCode::NOT_FOUND }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a oxtel_sdk::Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| &kv.value)
}

#[tokio::test(flavor = "current_thread")]
async fn requests_become_client_spans_and_carry_the_trace() {
    let telemetry = telemetry();
    let addr = server().await;
    let client = oxtel_reqwest::client(reqwest::Client::new());
    let job = tracing::info_span!("job");
    let traceparent = client
        .get(format!("http://user:secret@{addr}/items?token=secret"))
        .send()
        .instrument_in(&job)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    drop(job);

    let spans = telemetry.spans.finished_spans();
    let span = spans.iter().find(|s| s.kind == SpanKind::Client).unwrap();
    let job = spans.iter().find(|s| s.name == "job").unwrap();
    assert_eq!(span.name, "GET");
    assert_eq!(span.parent_span_id, Some(job.context.span_id));
    assert_eq!(
        attr(span, "url.full"),
        Some(&format!("http://127.0.0.1:{}/items", addr.port()).into()),
        "no credentials or query"
    );
    assert_eq!(attr(span, "server.address"), Some(&"127.0.0.1".into()));
    assert_eq!(
        attr(span, "server.port"),
        Some(&oxtel_sdk::Value::I64(addr.port().into()))
    );
    assert_eq!(attr(span, "http.request.method"), Some(&"GET".into()));
    assert_eq!(attr(span, "network.protocol.version"), Some(&"1.1".into()));
    assert_eq!(
        attr(span, "http.response.status_code"),
        Some(&oxtel_sdk::Value::I64(200))
    );
    assert_eq!(span.status, Status::Unset);

    let mut headers = HeaderMap::new();
    headers.insert(
        "traceparent",
        traceparent.parse().expect("traceparent sent"),
    );
    let carried = extract_trace_context(&headers).unwrap();
    assert_eq!(carried.trace_id, span.context.trace_id);
    assert_eq!(
        carried.span_id, span.context.span_id,
        "the client span is the server's parent"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn failures_are_errors_and_counted() {
    let telemetry = telemetry();
    let addr = server().await;
    // A port nothing listens on.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let client = oxtel_reqwest::client(reqwest::Client::new());
    let response = client
        .get(format!("http://{addr}/missing"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    assert!(
        client
            .get(format!("http://{closed}/"))
            .send()
            .await
            .is_err()
    );

    let spans = telemetry.spans.finished_spans();
    assert_eq!(attr(&spans[0], "error.type"), Some(&"404".into()));
    assert!(
        matches!(spans[0].status, Status::Error { .. }),
        "4xx is a client error"
    );
    assert_eq!(attr(&spans[1], "error.type"), Some(&"connect".into()));
    assert!(matches!(spans[1].status, Status::Error { .. }));
    assert_eq!(attr(&spans[1], "http.response.status_code"), None);

    let duration = telemetry
        .meters
        .collect()
        .into_iter()
        .find(|m| m.name == "http.client.request.duration")
        .unwrap();
    assert_eq!(duration.unit.as_deref(), Some("s"));
    let missing = duration
        .points
        .iter()
        .find(|p| {
            p.attributes
                .contains(&("http.response.status_code".into(), "404".into()))
        })
        .unwrap();
    for label in [
        ("http.request.method", "GET".to_string()),
        ("url.scheme", "http".to_string()),
        ("server.address", "127.0.0.1".to_string()),
        ("server.port", addr.port().to_string()),
        ("error.type", "404".to_string()),
    ] {
        assert!(
            missing
                .attributes
                .contains(&(label.0.into(), label.1.clone())),
            "{label:?}"
        );
    }
    assert!(matches!(&missing.value, PointValue::Histogram(h) if h.count() == 1));
    assert!(duration.points.iter().any(|p| {
        p.attributes
            .contains(&("error.type".into(), "connect".into()))
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn propagation_and_metrics_can_be_turned_off() {
    let telemetry = telemetry();
    let addr = server().await;
    let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
        .with(
            oxtel_reqwest::HttpClientMiddleware::new()
                .with_propagation(false)
                .with_metrics(false),
        )
        .build();
    let traceparent = client
        .get(format!("http://{addr}/items"))
        .send()
        .instrument_in(&tracing::info_span!("job"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(traceparent, "", "no trace context sent");
    assert_eq!(telemetry.spans.finished_spans().len(), 2);
    assert!(
        telemetry
            .meters
            .collect()
            .iter()
            .all(|m| m.name != "http.client.request.duration")
    );
}

/// Everything recorded is named as the conventions define it.
#[tokio::test(flavor = "current_thread")]
async fn spans_and_metrics_use_only_defined_names() {
    let telemetry = telemetry();
    let addr = server().await;
    let client = oxtel_reqwest::client(reqwest::Client::new());
    for path in ["/items", "/missing"] {
        client
            .get(format!("http://{addr}{path}"))
            .send()
            .await
            .unwrap();
    }
    let spans = telemetry.spans.finished_spans();
    assert_eq!(spans.len(), 2);
    for span in &spans {
        for kv in &span.attributes {
            assert!(
                kv.key.starts_with("otel.") || attribute::is_defined(&kv.key),
                "span {:?} has attribute {:?}, which the conventions don't define",
                span.name,
                kv.key
            );
        }
    }
    let families = telemetry.meters.collect();
    assert!(!families.is_empty());
    for family in &families {
        let definition = metric::by_name(&family.name)
            .unwrap_or_else(|| panic!("{} isn't a defined metric", family.name));
        assert_eq!(family.kind, MetricKind::Histogram, "{}", family.name);
        assert_eq!(definition.instrument, Instrument::Histogram);
        assert_eq!(family.unit.as_deref(), Some(definition.unit));
        for point in &family.points {
            for (key, _) in &point.attributes {
                assert!(
                    attribute::is_defined(key),
                    "{} has label {key:?}, which the conventions don't define",
                    family.name
                );
            }
        }
    }
}

/// Runs a future inside a span, as a caller's own work would.
trait InstrumentIn: std::future::Future + Sized {
    fn instrument_in(self, span: &tracing::Span) -> tracing::instrument::Instrumented<Self> {
        tracing::Instrument::instrument(self, span.clone())
    }
}

impl<F: std::future::Future> InstrumentIn for F {}
