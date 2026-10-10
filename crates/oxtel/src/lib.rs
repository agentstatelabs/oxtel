//! OpenTelemetry-compatible telemetry for Rust, native to [`tracing`] and
//! [`metrics`]: structured logs with one line of setup, and traces, metrics
//! and logs sent to a collector when you want them, with no code changes.
//!
//! ```no_run
//! fn main() -> Result<(), oxtel::Error> {
//!     // Keep the guard alive for as long as the program runs.
//!     let _telemetry = oxtel::init()?;
//!
//!     // Instrument as usual: events and spans with `tracing`, metrics
//!     // with `metrics`.
//!     tracing::info!(user.id = 42, "signed in");
//!     let span = tracing::info_span!("request", order.id = 7);
//!     let _entered = span.enter();
//!     tracing::warn!(attempt = 2, "payment provider slow; retrying");
//!     metrics::counter!("orders.placed").increment(1);
//!     Ok(())
//! }
//! ```
//!
//! With nothing configured, that writes log lines to standard error, as
//! text on a terminal and one JSON object per line anywhere else, and sends
//! nothing anywhere. Set `OTEL_EXPORTER_OTLP_ENDPOINT` (for example
//! `http://localhost:4318`) and traces, metrics and logs go there over OTLP.
//! On startup, oxtel logs one line saying what it set up. The
//! [guide](https://github.com/agentstatelabs/oxtel/blob/main/docs/guide.md)
//! covers the rest in plain words.
//!
//! What gets set up:
//!
//! | Signal | From | To (`OTEL_*_EXPORTER`) |
//! |---|---|---|
//! | Traces | `tracing` spans | `otlp`, `console`, `none` |
//! | Logs | `tracing` events, and `log` records with the `log` feature | `otlp`, `console`, `none` |
//! | Metrics | the `metrics` facade | `otlp`, `prometheus`, `console`, `none` |
//!
//! - Nothing is exported until it's configured. Setting
//!   `OTEL_EXPORTER_OTLP_ENDPOINT`, or a signal's own endpoint, makes `otlp`
//!   the default for those signals. The specification's default is `otlp`
//!   to `localhost:4318` even then; naming `otlp` still gets that.
//! - `OTEL_CONFIG_FILE`, with the `config` feature (on by default), names a
//!   declarative configuration file to build everything from instead: see
//!   [`oxtel_config`](https://docs.rs/oxtel-config).
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
//! - Events are also written locally, to standard error: readable text
//!   when that's a terminal, flat JSON with OpenTelemetry field names
//!   otherwise, for log shippers. `OXTEL_LOG_FORMAT` (`auto`, `text`,
//!   `json`, `ecs` or `none`) and `OXTEL_LOG_OUTPUT` (`stderr` or `stdout`)
//!   change that; so do [`Builder::with_local_logs`] and its neighbours.
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
mod plan;
pub mod propagation;
mod summary;

pub use builder::{Builder, Error, Telemetry};
pub use config::{Exporter, LogOutput};
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
