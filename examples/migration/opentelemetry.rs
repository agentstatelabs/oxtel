//! Before moving to oxtel: traces and logs sent over OTLP with the
//! OpenTelemetry SDK, through tracing-opentelemetry and the tracing
//! appender, with log lines from tracing-subscriber's `fmt`, and the trace
//! carried across a boundary by hand. The tracing calls are the same as in
//! `examples/logs.rs` and `examples/propagation.rs`.
//!
//! It isn't one of the workspace's examples, since it needs crates they
//! don't. `cargo run -p xtask -- check-examples` checks it builds with:
//!
//! ```toml
//! [dependencies]
//! opentelemetry = "0.33"
//! opentelemetry_sdk = "0.33"
//! opentelemetry-otlp = "0.33"
//! opentelemetry-appender-tracing = "0.33"
//! tracing = "0.1"
//! tracing-opentelemetry = "0.34"
//! tracing-subscriber = { version = "0.3", features = ["env-filter"] }
//! ```

use std::collections::HashMap;

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, SpanExporter};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ANCHOR: setup
    // The service's name comes from OTEL_SERVICE_NAME, and the endpoint
    // from OTEL_EXPORTER_OTLP_ENDPOINT.
    let traces = SdkTracerProvider::builder()
        .with_batch_exporter(SpanExporter::builder().with_http().build()?)
        .build();
    let logs = SdkLoggerProvider::builder()
        .with_batch_exporter(LogExporter::builder().with_http().build()?)
        .build();
    global::set_text_map_propagator(TraceContextPropagator::new());
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_opentelemetry::layer().with_tracer(traces.tracer("shop")))
        .with(OpenTelemetryTracingBridge::new(&logs))
        .init();
    // ANCHOR_END: setup

    tracing::info!(user.id = 42, plan = "team", "signed in");

    let mut headers = HashMap::new();
    tracing::info_span!("send order").in_scope(|| inject(&mut headers));
    handle(&headers);

    // ANCHOR: shutdown
    // Send what's still waiting, before the program exits.
    traces.shutdown()?;
    logs.shutdown()?;
    // ANCHOR_END: shutdown
    Ok(())
}

// ANCHOR: propagation
/// A client: write the current trace into the outgoing headers.
fn inject(outgoing: &mut HashMap<String, String>) {
    let context = tracing::Span::current().context();
    global::get_text_map_propagator(|propagator| propagator.inject_context(&context, outgoing));
}

/// A server: continue the trace the incoming headers carry.
fn handle(incoming: &HashMap<String, String>) {
    let parent = global::get_text_map_propagator(|propagator| propagator.extract(incoming));
    let span = tracing::info_span!("handle order");
    let _ = span.set_parent(parent);
    let _entered = span.enter();
    tracing::info!("handling the order");
}
// ANCHOR_END: propagation
