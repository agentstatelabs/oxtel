//! OpenTelemetry-compatible telemetry for Rust, native to [`tracing`] and
//! [`metrics`].
//!
//! One call sets everything up from the standard `OTEL_*` configuration:
//!
//! ```no_run
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let _telemetry = oxtel::builder()
//!         .with_default_service_name("checkout")
//!         .install()?;
//!
//!     // Instrument as usual: spans and events with `tracing`,
//!     // metrics with `metrics`.
//!     let span = tracing::info_span!("request", otel.kind = "server");
//!     let _entered = span.enter();
//!     tracing::info!(items = 3, "listed");
//!     metrics::counter!("requests.handled").increment(1);
//!     Ok(())
//! } // Dropping the guard exports what's pending.
//! ```
//!
//! What gets set up:
//!
//! | Signal | From | To (`OTEL_*_EXPORTER`) |
//! |---|---|---|
//! | Traces | `tracing` spans | `otlp` (default), `console`, `none` |
//! | Logs | `tracing` events, and `log` records with the `log` feature | `otlp` (default), `console`, `none` |
//! | Metrics | the `metrics` facade | `otlp` (default), `prometheus`, `console`, `none` |
//!
//! - `OTEL_SDK_DISABLED=true` turns all of it off. Local logs and layers
//!   added with [`Builder::with_layer`] still run.
//! - The resource comes from `OTEL_SERVICE_NAME` and
//!   `OTEL_RESOURCE_ATTRIBUTES`, the sampler from `OTEL_TRACES_SAMPLER`,
//!   OTLP's endpoint and headers from `OTEL_EXPORTER_OTLP_*`, the Prometheus
//!   endpoint from `OTEL_EXPORTER_PROMETHEUS_HOST` and `_PORT`, and batching
//!   and limits from their standard variables.
//! - Spans and log records follow `RUST_LOG`-style directives, `info` by
//!   default; see [`Builder::with_traces_filter`].
//! - With a log pipeline, events become log records carrying their span's
//!   ids. Without one, they become span events instead.
//! - Events can also be written locally, to standard error, as text, flat
//!   JSON or ECS JSON: see [`Builder::with_local_logs`] and
//!   `OXTEL_LOG_FORMAT`.
//! - Settings oxtel can't honour, such as `OTEL_TRACES_EXPORTER=zipkin`, are
//!   logged as warnings with the target `oxtel`, rather than failing.
//!
//! [`Builder`] adds to this: resource attributes, filters, your own layers,
//! and hooks into each pipeline for extra exporters and processors.
//!
//! [`tracing`]: https://docs.rs/tracing
//! [`metrics`]: https://docs.rs/metrics

mod builder;
mod config;
mod filter;
mod guard;
pub mod propagation;

pub use builder::{Builder, Error, Telemetry};
pub use config::Exporter;
pub use guard::Guard;

#[cfg(feature = "otlp")]
pub use oxtel_otlp as otlp;
pub use oxtel_prometheus as prometheus;
pub use oxtel_sdk as sdk;
pub use oxtel_sdk::{KeyValue, Resource, Value};

/// Set up traces, metrics and logs from the standard `OTEL_*`
/// configuration, and install them globally. Keep the guard alive for as
/// long as the program runs.
///
/// Fails only if another `tracing` subscriber or `metrics` recorder is
/// already installed.
pub fn init() -> Result<Guard, Error> {
    builder().install()
}

/// A [`Builder`], for configuration beyond the environment's.
pub fn builder() -> Builder {
    Builder::default()
}
