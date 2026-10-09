//! Traces: `tracing` spans in, OpenTelemetry spans out.
//!
//! ```
//! use oxtel_sdk::trace::{InMemorySpanExporter, TracerProvider};
//! use tracing_subscriber::layer::SubscriberExt;
//!
//! let exporter = InMemorySpanExporter::default();
//! let provider = TracerProvider::builder()
//!     .with_simple_exporter(exporter.clone())
//!     .build();
//! let subscriber = tracing_subscriber::registry().with(provider.layer());
//! tracing::subscriber::with_default(subscriber, || {
//!     tracing::info_span!("work", items = 3).in_scope(|| {});
//! });
//! assert_eq!(exporter.finished_spans()[0].name, "work");
//! ```

mod data;
mod export;
mod layer;
mod processor;
mod provider;
mod sampler;

pub use data::{Event, Link, Scope, SpanData, SpanKind, Status};
pub use export::{ExportError, InMemorySpanExporter, SpanExporter};
pub use layer::{
    RemoteParentGuard, SpanLimits, TraceLayer, attach_remote_parent, current_span_context,
    span_context,
};
pub use processor::{
    BatchConfig, BatchSpanProcessor, NoopSpanProcessor, ProcessorStats, SimpleSpanProcessor,
    SpanProcessor,
};
pub use provider::{TracerProvider, TracerProviderBuilder};
pub use sampler::Sampler;
