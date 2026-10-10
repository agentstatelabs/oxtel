//! Everything oxtel-tower records is named as the OpenTelemetry semantic
//! conventions define it: every span attribute and metric label is a
//! defined attribute, and every metric matches its definition's instrument
//! and unit.

use axum::Router;
use axum::body::Body;
use axum::extract::MatchedPath;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{MeterProvider, MetricKind};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, TracerProvider};
use oxtel_semconv::{Instrument, attribute, metric};
use oxtel_tower::{HttpClientLayer, HttpServerLayer};
use tower::{Layer, ServiceExt, service_fn};
use tracing_subscriber::layer::SubscriberExt;

/// `otel.*` fields steer the span itself; they aren't attributes.
fn is_instruction(key: &str) -> bool {
    key.starts_with("otel.")
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

    let server = HttpServerLayer::new()
        .with_route(|ext| ext.get::<MatchedPath>().map(MatchedPath::as_str))
        .with_access_log(true);
    let app = Router::new()
        .route("/items/{id}", get(|| async { "item" }))
        .route("/fail", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .layer(server);
    for uri in ["/items/1", "/fail"] {
        let req = Request::get(uri)
            .header("host", "api.example:8080")
            .header("user-agent", "probe/1.0")
            .body(Body::empty())
            .unwrap();
        app.clone().oneshot(req).await.unwrap();
    }
    let client = HttpClientLayer::new().layer(service_fn(|_: Request<()>| async {
        Ok::<_, std::convert::Infallible>(
            axum::http::Response::builder()
                .status(404)
                .body(())
                .unwrap(),
        )
    }));
    client
        .oneshot(Request::get("https://api.example/x").body(()).unwrap())
        .await
        .unwrap();

    let spans = spans.finished_spans();
    assert!(spans.len() >= 3);
    for span in &spans {
        for kv in &span.attributes {
            assert!(
                is_instruction(&kv.key) || attribute::is_defined(&kv.key),
                "span {:?} has attribute {:?}, which the conventions don't define",
                span.name,
                kv.key
            );
        }
    }

    let families = meters.collect();
    assert!(families.len() >= 3);
    for family in &families {
        let definition = metric::by_name(&family.name)
            .unwrap_or_else(|| panic!("{} isn't a defined metric", family.name));
        let instrument = match family.kind {
            MetricKind::Counter => Instrument::Counter,
            MetricKind::Histogram => Instrument::Histogram,
            // A `metrics` gauge moved up and down is an up-down counter.
            MetricKind::Gauge => definition.instrument,
        };
        assert_eq!(instrument, definition.instrument, "{}", family.name);
        if let Some(unit) = &family.unit {
            assert_eq!(unit, definition.unit, "{}", family.name);
        } else {
            assert!(
                definition.unit.starts_with('{'),
                "{} needs a unit",
                family.name
            );
        }
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
