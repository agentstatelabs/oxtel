//! Logs: `tracing` events in, OpenTelemetry log records out.
//!
//! ```
//! use oxtel_sdk::logs::{InMemoryLogExporter, LoggerProvider, Severity};
//! use tracing_subscriber::layer::SubscriberExt;
//!
//! let exporter = InMemoryLogExporter::default();
//! let provider = LoggerProvider::builder()
//!     .with_simple_exporter(exporter.clone())
//!     .build();
//! let subscriber = tracing_subscriber::registry().with(provider.layer());
//! tracing::subscriber::with_default(subscriber, || {
//!     tracing::warn!(retries = 3, "write conflict");
//! });
//! let record = &exporter.records()[0];
//! assert_eq!(record.severity, Severity::WARN);
//! assert_eq!(record.body, Some("write conflict".into()));
//! ```
//!
//! With a [`TracerProvider`](crate::trace::TracerProvider)'s layer in the
//! same subscriber, records made inside a span carry its trace and span ids.
//! Turn its span events off
//! ([`with_span_events(false)`](crate::trace::TracerProviderBuilder::with_span_events))
//! to have events exported only as log records, as OpenTelemetry now
//! recommends.

mod data;
mod export;
mod layer;
mod processor;
mod provider;

pub use crate::export::{BatchConfig, ExportError, ProcessorStats};
pub use data::{LogRecord, Severity};
pub use export::{InMemoryLogExporter, LogExporter};
pub use layer::{LogLayer, LogLimits};
pub use processor::{BatchLogProcessor, LogProcessor, NoopLogProcessor, SimpleLogProcessor};
pub use provider::{LoggerProvider, LoggerProviderBuilder};
