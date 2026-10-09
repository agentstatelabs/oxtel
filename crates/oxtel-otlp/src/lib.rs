//! OTLP exporters for [oxtel](https://github.com/agentstatelabs/oxtel).
//!
//! [`OtlpHttpExporter`] sends spans over OTLP/HTTP with protobuf bodies, to
//! an OpenTelemetry Collector or any backend that accepts OTLP:
//!
//! ```no_run
//! use oxtel_otlp::OtlpHttpExporter;
//! use oxtel_sdk::trace::TracerProvider;
//! use tracing_subscriber::layer::SubscriberExt;
//!
//! // Endpoint, headers, compression and timeout come from the standard
//! // OTEL_EXPORTER_OTLP_* variables.
//! let provider = TracerProvider::builder()
//!     .with_batch_exporter(OtlpHttpExporter::builder().build())
//!     .build();
//! tracing::subscriber::set_global_default(
//!     tracing_subscriber::registry().with(provider.layer()),
//! )
//! .unwrap();
//! ```
//!
//! The protocol types are in [`proto`], generated from the official
//! definitions; [`transform`] converts oxtel's data model to them.

#[cfg(feature = "http-proto")]
mod http;
pub mod proto;
pub mod transform;

#[cfg(feature = "http-proto")]
pub use http::{OtlpHttpExporter, OtlpHttpExporterBuilder, parse_headers, resolve_url};
