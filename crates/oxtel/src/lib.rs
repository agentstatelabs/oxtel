//! OpenTelemetry-compatible telemetry for Rust, native to [`tracing`] and
//! [`metrics`].
//!
//! **Under development.** This release reserves the name; it has no API yet.
//! See the [design](https://github.com/agentstatelabs/oxtel/blob/main/DESIGN.md).
//!
//! oxtel turns the spans, events and metrics your code already records with
//! `tracing` and `metrics` into standard OpenTelemetry output: OTLP traces,
//! metrics and logs, and Prometheus/OpenMetrics, configured through the
//! standard `OTEL_*` variables or an OpenTelemetry declarative config file.
//!
//! [`tracing`]: https://docs.rs/tracing
//! [`metrics`]: https://docs.rs/metrics
