//! oxtel in one call: traces, metrics and logs, configured by the standard
//! `OTEL_*` variables.
//!
//! With an OpenTelemetry Collector on localhost:4318 (or wherever
//! `OTEL_EXPORTER_OTLP_ENDPOINT` points), everything arrives there:
//!
//! ```bash
//! cargo run -p oxtel-examples --example quickstart
//! ```
//!
//! Without one, print it instead:
//!
//! ```bash
//! OTEL_TRACES_EXPORTER=console OTEL_LOGS_EXPORTER=console OTEL_METRICS_EXPORTER=console \
//!   cargo run -p oxtel-examples --example quickstart
//! ```

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The service name applies unless OTEL_SERVICE_NAME sets another. Keep
    // the guard alive: dropping it exports what's pending.
    let _telemetry = oxtel::builder()
        .with_default_service_name("quickstart")
        .install()?;

    metrics::describe_histogram!(
        "http.server.request.duration",
        metrics::Unit::Seconds,
        "Duration of HTTP server requests."
    );

    // Your code, instrumented as usual.
    for route in ["/items", "/cart"] {
        let span = tracing::info_span!("request", otel.kind = "server", http.route = route);
        let _entered = span.enter();
        tracing::info!(items = 3, "listed");
        metrics::histogram!("http.server.request.duration", "http.route" => route).record(0.042);
    }
    Ok(())
}
