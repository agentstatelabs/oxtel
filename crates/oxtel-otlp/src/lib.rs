//! OTLP exporters for [oxtel](https://github.com/agentstatelabs/oxtel).
//!
//! [`OtlpHttpExporter`] sends spans, metrics and log records over OTLP/HTTP,
//! with protobuf or JSON bodies, to an OpenTelemetry Collector or any backend that
//! accepts OTLP. One exporter, cloned, serves every pipeline:
//!
//! ```no_run
//! use oxtel_otlp::OtlpHttpExporter;
//! use oxtel_sdk::logs::LoggerProvider;
//! use oxtel_sdk::metrics::MeterProvider;
//! use oxtel_sdk::trace::TracerProvider;
//! use tracing_subscriber::layer::SubscriberExt;
//!
//! // Each signal's endpoint, headers, compression and timeout come from the
//! // standard OTEL_EXPORTER_OTLP_* variables.
//! let otlp = OtlpHttpExporter::builder().build();
//! let traces = TracerProvider::builder()
//!     .with_batch_exporter(otlp.clone())
//!     .build();
//! let logs = LoggerProvider::builder()
//!     .with_batch_exporter(otlp.clone())
//!     .build();
//! let meters = MeterProvider::builder().with_periodic_exporter(otlp).build();
//! metrics::set_global_recorder(meters.recorder()).unwrap();
//! tracing::subscriber::set_global_default(
//!     tracing_subscriber::registry()
//!         .with(traces.layer())
//!         .with(logs.layer()),
//! )
//! .unwrap();
//! ```
//!
//! The protocol types are in [`proto`], generated from the official
//! definitions; [`transform`] converts oxtel's data model to them.

#[cfg(feature = "http-proto")]
mod http;
pub mod json;
pub mod proto;
pub mod transform;

#[cfg(feature = "http-proto")]
pub use http::{
    OtlpHttpExporter, OtlpHttpExporterBuilder, Protocol, Signal, parse_headers, resolve_url,
};
