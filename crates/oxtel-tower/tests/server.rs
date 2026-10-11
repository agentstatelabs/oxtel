//! The server layer around a real axum router.

use axum::Router;
use axum::body::Body;
use axum::extract::MatchedPath;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use oxtel_sdk::Resource;
use oxtel_sdk::context::TraceFlags;
use oxtel_sdk::logs::{InMemoryLogExporter, LoggerProvider, Severity};
use oxtel_sdk::metrics::{MeterProvider, MetricKind, PointValue};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, SpanKind, Status, TracerProvider};
use oxtel_sdk::{SpanContext, SpanId, TraceId, Value};
use oxtel_tower::{HttpServerLayer, MetricsService, server_span};
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;

struct Harness {
    spans: InMemorySpanExporter,
    logs: InMemoryLogExporter,
    meters: MeterProvider,
    _guards: (
        tracing::subscriber::DefaultGuard,
        metrics::LocalRecorderGuard<'static>,
    ),
    _providers: (TracerProvider, LoggerProvider),
}

/// Pipelines recording into memory, installed for this thread.
fn harness() -> Harness {
    let spans = InMemorySpanExporter::default();
    let logs = InMemoryLogExporter::default();
    let resource = Resource::builder().without_env().build();
    let traces = TracerProvider::builder()
        .with_resource(resource.clone())
        .with_sampler(Sampler::AlwaysOn)
        .with_span_events(false)
        .with_simple_exporter(spans.clone())
        .build();
    let log_provider = LoggerProvider::builder()
        .with_resource(resource)
        .with_simple_exporter(logs.clone())
        .build();
    let meters = MeterProvider::builder().build();
    let subscriber = tracing_subscriber::registry()
        .with(traces.layer())
        .with(log_provider.layer());
    let recorder: &'static _ = Box::leak(Box::new(meters.recorder()));
    Harness {
        spans,
        logs,
        meters,
        _guards: (
            tracing::subscriber::set_default(subscriber),
            metrics::set_default_local_recorder(recorder),
        ),
        _providers: (traces, log_provider),
    }
}

fn matched(ext: &axum::http::Extensions) -> Option<&str> {
    ext.get::<MatchedPath>().map(MatchedPath::as_str)
}

fn app(layer: HttpServerLayer) -> Router {
    Router::new()
        .route("/items/{id}", get(|| async { "item" }))
        .route(
            "/fail",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "broken") }),
        )
        .route(
            "/whoami",
            get(|| async {
                tracing::Span::current().record("app.user", "u-1");
                "you"
            }),
        )
        .layer(layer)
}

async fn send(app: &Router, req: Request<Body>) -> StatusCode {
    app.clone().oneshot(req).await.unwrap().status()
}

fn get_req(uri: &str) -> Request<Body> {
    Request::get(uri)
        .header("host", "api.example:8080")
        .header("user-agent", "probe/1.0")
        .body(Body::empty())
        .unwrap()
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| &kv.value)
}

#[tokio::test(flavor = "current_thread")]
async fn requests_become_server_spans_named_by_route() {
    let h = harness();
    let app = app(HttpServerLayer::new().with_route(matched));
    assert_eq!(
        send(&app, get_req("/items/7?secret=x")).await,
        StatusCode::OK
    );
    let span = &h.spans.finished_spans()[0];
    assert_eq!(span.name, "GET /items/{id}");
    assert_eq!(span.kind, SpanKind::Server);
    assert_eq!(span.status, Status::Unset);
    for (key, value) in [
        ("http.request.method", Value::from("GET")),
        ("url.path", Value::from("/items/7")),
        ("url.scheme", Value::from("http")),
        ("http.route", Value::from("/items/{id}")),
        ("server.address", Value::from("api.example")),
        ("server.port", Value::I64(8080)),
        ("network.protocol.version", Value::from("1.1")),
        ("user_agent.original", Value::from("probe/1.0")),
        ("http.response.status_code", Value::I64(200)),
    ] {
        assert_eq!(attr(span, key), Some(&value), "{key}");
    }
    assert!(attr(span, "error.type").is_none());
    assert!(
        span.attributes
            .iter()
            .all(|kv| !matches!(&kv.value, Value::String(s) if s.contains("secret"))),
        "the query isn't recorded"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn server_errors_and_unmatched_requests() {
    let h = harness();
    let app = app(HttpServerLayer::new().with_route(matched));
    send(&app, get_req("/fail")).await;
    send(&app, get_req("/nowhere")).await;
    let purge = Request::builder()
        .method("PURGE")
        .uri("/items/1")
        .body(Body::empty())
        .unwrap();
    send(&app, purge).await;
    let spans = h.spans.finished_spans();

    let fail = &spans[0];
    assert_eq!(fail.name, "GET /fail");
    assert_eq!(attr(fail, "error.type"), Some(&Value::from("500")));
    assert!(matches!(fail.status, Status::Error { .. }));

    // The fallback isn't wrapped by `Router::layer`, so 404s for unknown
    // paths leave no span; that keeps scanners out of the traces.
    let purge = spans.last().unwrap();
    assert_eq!(purge.name, "HTTP /items/{id}", "an unknown method is HTTP");
    assert_eq!(
        attr(purge, "http.request.method"),
        Some(&Value::from("_OTHER"))
    );
    assert_eq!(
        attr(purge, "http.request.method_original"),
        Some(&Value::from("PURGE"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn callers_traces_are_continued_unless_told_not_to() {
    let h = harness();
    let caller = SpanContext {
        trace_id: TraceId::random(),
        span_id: SpanId::random(),
        flags: TraceFlags::SAMPLED,
        is_remote: true,
        ..Default::default()
    };
    let traced = || {
        let mut req = get_req("/items/1");
        req.headers_mut().insert(
            "traceparent",
            format!("00-{}-{}-01", caller.trace_id, caller.span_id)
                .parse()
                .unwrap(),
        );
        req
    };
    send(&app(HttpServerLayer::new()), traced()).await;
    send(
        &app(HttpServerLayer::new().with_propagation(false)),
        traced(),
    )
    .await;
    let spans = h.spans.finished_spans();
    assert_eq!(spans[0].context.trace_id, caller.trace_id);
    assert_eq!(spans[0].parent_span_id, Some(caller.span_id));
    assert!(spans[0].parent_is_remote);
    assert_ne!(spans[1].context.trace_id, caller.trace_id);
    assert_eq!(spans[1].parent_span_id, None);
}

#[tokio::test(flavor = "current_thread")]
async fn metrics_follow_the_conventions() {
    let h = harness();
    let app = app(HttpServerLayer::new().with_route(matched));
    send(&app, get_req("/items/1")).await;
    send(&app, get_req("/items/2")).await;
    send(&app, get_req("/fail")).await;
    let metrics = h.meters.collect();

    let duration = metrics
        .iter()
        .find(|m| m.name == "http.server.request.duration")
        .unwrap();
    assert_eq!(duration.kind, MetricKind::Histogram);
    assert_eq!(duration.unit.as_deref(), Some("s"));
    let series = |route: &str| {
        duration
            .points
            .iter()
            .find(|p| {
                p.attributes
                    .iter()
                    .any(|(k, v)| k == "http.route" && v == route)
            })
            .unwrap()
    };
    let items = series("/items/{id}");
    let PointValue::Histogram(h1) = &items.value else {
        panic!()
    };
    assert_eq!(
        h1.count(),
        2,
        "one series for every id: the route, not the path"
    );
    assert_eq!(h1.bounds.len(), 14, "the conventions' duration buckets");
    let labels: Vec<&str> = items.attributes.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        labels,
        [
            "http.request.method",
            "http.response.status_code",
            "http.route",
            "network.protocol.version",
            "url.scheme"
        ]
    );
    assert!(
        series("/fail")
            .attributes
            .contains(&("error.type".into(), "500".into()))
    );

    let active = metrics
        .iter()
        .find(|m| m.name == "http.server.active_requests")
        .unwrap();
    assert_eq!(
        active.points[0].value,
        PointValue::Gauge(0.0),
        "all finished"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_access_log_is_on_unless_turned_off() {
    let h = harness();
    let quiet = app(HttpServerLayer::new().with_access_log(false));
    send(&quiet, get_req("/items/1")).await;
    assert!(h.logs.records().is_empty());
    let logged = app(HttpServerLayer::new());
    send(&logged, get_req("/items/1")).await;
    send(&logged, get_req("/fail")).await;
    let records = h.logs.records();
    let spans = h.spans.finished_spans();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].body, Some(Value::from("request")));
    assert_eq!(records[0].severity, Severity::INFO);
    assert_eq!(records[0].span_id, Some(spans[1].context.span_id));
    assert_eq!(records[1].severity, Severity::ERROR, "5xx logs at error");
}

#[tokio::test(flavor = "current_thread")]
async fn spans_of_your_own_keep_the_standard_fields() {
    let h = harness();
    let layer = HttpServerLayer::new()
        .with_route(matched)
        .with_span(|info| server_span!(info, app.user = tracing::field::Empty));
    send(&app(layer), get_req("/whoami")).await;
    let span = &h.spans.finished_spans()[0];
    assert_eq!(attr(span, "app.user"), Some(&Value::from("u-1")));
    assert_eq!(span.name, "GET /whoami");
    assert_eq!(
        attr(span, "http.response.status_code"),
        Some(&Value::I64(200))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_metrics_service_serves_scrapes() {
    let h = harness();
    metrics::counter!("jobs.done").increment(4);
    let exporter = oxtel_prometheus::PrometheusExporter::new(
        h.meters.clone(),
        Resource::builder().without_env().build(),
    );
    let app = Router::new().route_service("/metrics", MetricsService::new(exporter));
    let response = app
        .oneshot(
            Request::get("/metrics")
                .header("accept", "application/openmetrics-text")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response.headers()["content-type"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(content_type.starts_with("application/openmetrics-text"));
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("jobs_done_total"), "{body}");
}

#[cfg(feature = "axum")]
#[tokio::test(flavor = "current_thread")]
async fn with_the_axum_feature_routes_need_no_setup() {
    let h = harness();
    send(&app(HttpServerLayer::new()), get_req("/items/9")).await;
    assert_eq!(h.spans.finished_spans()[0].name, "GET /items/{id}");
}
