//! The client layer around a fake client that records what it was sent.

use std::convert::Infallible;
use std::fmt;
use std::sync::{Arc, Mutex};

use http::{HeaderMap, Request, Response};
use oxtel_sdk::Resource;
use oxtel_sdk::metrics::{MeterProvider, PointValue};
use oxtel_sdk::propagation::extract_trace_context;
use oxtel_sdk::trace::{
    InMemorySpanExporter, Sampler, SpanData, SpanKind, Status, TracerProvider, span_context,
};
use oxtel_tower::HttpClientLayer;
use tower::{Layer, ServiceExt, service_fn};
use tracing_subscriber::layer::SubscriberExt;

type Sent = Arc<Mutex<Vec<HeaderMap>>>;

/// Answers 404 for `/missing`, 200 otherwise, keeping each request's
/// headers.
fn fake_client(
    sent: Sent,
) -> impl tower::Service<Request<()>, Response = Response<()>, Error = Infallible, Future = impl Send>
+ Clone {
    service_fn(move |req: Request<()>| {
        let sent = Arc::clone(&sent);
        async move {
            sent.lock().unwrap().push(req.headers().clone());
            let status = if req.uri().path() == "/missing" {
                404
            } else {
                200
            };
            Ok::<_, Infallible>(Response::builder().status(status).body(()).unwrap())
        }
    })
}

fn setup() -> (
    InMemorySpanExporter,
    MeterProvider,
    TracerProvider,
    tracing::subscriber::DefaultGuard,
    metrics::LocalRecorderGuard<'static>,
) {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(spans.clone())
        .build();
    let meters = MeterProvider::builder().build();
    let recorder: &'static _ = Box::leak(Box::new(meters.recorder()));
    let metrics_guard = metrics::set_default_local_recorder(recorder);
    let guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(traces.layer()));
    (spans, meters, traces, guard, metrics_guard)
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a oxtel_sdk::Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| &kv.value)
}

#[tokio::test(flavor = "current_thread")]
async fn requests_become_client_spans_and_carry_the_trace() {
    let (spans, _meters, _traces, _g, _m) = setup();
    let sent = Sent::default();
    let client = HttpClientLayer::new().layer(fake_client(Arc::clone(&sent)));
    let parent = tracing::info_span!("job");
    let response = {
        let _entered = parent.enter();
        client
            .clone()
            .oneshot(
                Request::get("https://user:pw@api.example/items?token=secret")
                    .body(())
                    .unwrap(),
            )
            .await
            .unwrap()
    };
    assert_eq!(response.status(), 200);
    drop(parent);
    let spans = spans.finished_spans();
    let span = spans.iter().find(|s| s.kind == SpanKind::Client).unwrap();
    let job = spans.iter().find(|s| s.name == "job").unwrap();
    assert_eq!(span.name, "GET");
    assert_eq!(span.parent_span_id, Some(job.context.span_id));
    assert_eq!(
        attr(span, "url.full"),
        Some(&"https://api.example:443/items".into()),
        "no credentials or query"
    );
    assert_eq!(attr(span, "server.address"), Some(&"api.example".into()));
    assert_eq!(attr(span, "server.port"), Some(&oxtel_sdk::Value::I64(443)));
    assert_eq!(
        attr(span, "http.response.status_code"),
        Some(&oxtel_sdk::Value::I64(200))
    );
    assert_eq!(span.status, Status::Unset);

    let headers = &sent.lock().unwrap()[0];
    let carried = extract_trace_context(headers).expect("traceparent sent");
    assert_eq!(carried.trace_id, span.context.trace_id);
    assert_eq!(
        carried.span_id, span.context.span_id,
        "the client span is the server's parent"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn failures_are_errors_and_counted() {
    #[derive(Debug)]
    struct Refused;
    impl fmt::Display for Refused {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("refused")
        }
    }
    impl std::error::Error for Refused {}

    let (spans, meters, _traces, _g, _m) = setup();
    let sent = Sent::default();
    let client = HttpClientLayer::new().layer(fake_client(Arc::clone(&sent)));
    client
        .clone()
        .oneshot(Request::get("http://api.example/missing").body(()).unwrap())
        .await
        .unwrap();
    let broken = HttpClientLayer::new().layer(service_fn(|_: Request<()>| async {
        Err::<Response<()>, _>(Refused)
    }));
    assert!(
        broken
            .oneshot(Request::get("http://api.example/x").body(()).unwrap())
            .await
            .is_err()
    );

    let spans = spans.finished_spans();
    assert_eq!(attr(&spans[0], "error.type"), Some(&"404".into()));
    assert!(
        matches!(spans[0].status, Status::Error { .. }),
        "4xx is a client error"
    );
    assert!(matches!(
        attr(&spans[1], "error.type"),
        Some(oxtel_sdk::Value::String(t)) if t.ends_with("Refused")
    ));

    let duration = meters
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
    assert!(
        missing
            .attributes
            .contains(&("server.address".into(), "api.example".into()))
    );
    assert!(
        missing
            .attributes
            .contains(&("server.port".into(), "80".into()))
    );
    assert!(matches!(&missing.value, PointValue::Histogram(h) if h.count() == 1));
}

#[tokio::test(flavor = "current_thread")]
async fn propagation_can_be_turned_off() {
    let (_spans, _meters, _traces, _g, _m) = setup();
    let sent = Sent::default();
    let client = HttpClientLayer::new()
        .with_propagation(false)
        .layer(fake_client(Arc::clone(&sent)));
    let span = tracing::info_span!("job");
    let _entered = span.enter();
    assert!(span_context(&span).is_some());
    client
        .oneshot(Request::get("https://partner.example/").body(()).unwrap())
        .await
        .unwrap();
    assert!(sent.lock().unwrap()[0].get("traceparent").is_none());
}
