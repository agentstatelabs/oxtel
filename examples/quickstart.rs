//! oxtel in one program: traces and logs to an OTLP collector, metrics for
//! a Prometheus scrape, exemplars linking metrics to traces.
//!
//! `cargo run -p oxtel-examples --example quickstart`
//!
//! With a collector listening on localhost:4318 (or wherever
//! `OTEL_EXPORTER_OTLP_ENDPOINT` points), the spans and log records arrive
//! there. The Prometheus exposition is printed at the end.

use oxtel_otlp::OtlpHttpExporter;
use oxtel_prometheus::PrometheusExporter;
use oxtel_sdk::Resource;
use oxtel_sdk::logs::LoggerProvider;
use oxtel_sdk::metrics::MeterProvider;
use oxtel_sdk::trace::TracerProvider;
use tracing_subscriber::layer::SubscriberExt;

fn main() {
    // Service name, resource attributes and sampler come from the standard
    // OTEL_* variables; this name applies unless OTEL_SERVICE_NAME is set.
    let resource = Resource::builder()
        .with_default_service_name("quickstart")
        .build();

    // One OTLP exporter serves every signal. Endpoint, headers, compression
    // and timeout come from the OTEL_EXPORTER_OTLP_* variables.
    let otlp = OtlpHttpExporter::builder().build();

    // Traces: tracing spans, batched and exported over OTLP/HTTP. Events
    // go out as log records rather than span events.
    let traces = TracerProvider::builder()
        .with_resource(resource.clone())
        .with_span_events(false)
        .with_batch_exporter(otlp.clone())
        .build();

    // Logs: tracing events, carrying the ids of the span they happen in.
    let logs = LoggerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(otlp)
        .build();

    tracing::subscriber::set_global_default(
        tracing_subscriber::registry()
            .with(traces.layer())
            .with(logs.layer())
            .with(tracing_subscriber::fmt::layer()),
    )
    .expect("set the subscriber");

    // Metrics: the `metrics` macros, recorded by oxtel.
    let meters = MeterProvider::builder().build();
    metrics::set_global_recorder(meters.recorder()).expect("set the recorder");
    metrics::describe_histogram!(
        "http.server.request.duration",
        metrics::Unit::Seconds,
        "Duration of HTTP server requests."
    );

    // Your code, instrumented as usual.
    for route in ["/items", "/cart"] {
        let span = tracing::info_span!("request", otel.kind = "server", http.route = route);
        let _entered = span.enter();
        tracing::info!("handling");
        metrics::histogram!("http.server.request.duration", "http.route" => route).record(0.042);
    }

    // What a Prometheus scrape gets: OpenMetrics, with exemplars that link
    // each bucket to the trace that recorded it.
    let prometheus = PrometheusExporter::new(meters, resource);
    let (_content_type, body) = prometheus.render(Some("application/openmetrics-text"));
    println!("{body}");

    // Flush the spans and log records before exiting.
    let _ = logs.shutdown();
    let _ = traces.shutdown();
}
