//! Metrics: the `metrics` facade in, OpenTelemetry metrics out.
//!
//! Install the provider's recorder, record with the `metrics` macros, and
//! read a snapshot for export:
//!
//! ```
//! use oxtel_sdk::metrics::{MeterProvider, PointValue};
//!
//! let provider = MeterProvider::builder().build();
//! metrics::with_local_recorder(&provider.recorder(), || {
//!     metrics::counter!("jobs.done", "queue" => "email").increment(2);
//! });
//! let families = provider.collect();
//! assert_eq!(families[0].points[0].value, PointValue::Counter(2));
//! ```
//!
//! In a program, install it once with `metrics::set_global_recorder`.
//!
//! To push metrics, to OTLP for instance, give the builder a
//! [`MetricExporter`] with
//! [`with_periodic_exporter`](MeterProviderBuilder::with_periodic_exporter):
//! a background thread exports every `OTEL_METRIC_EXPORT_INTERVAL` (60 s by
//! default), and once more when the provider shuts down.

mod adder;
mod data;
mod export;
mod provider;
mod reader;
mod store;
mod units;

pub use crate::export::{ExportError, ProcessorStats};
pub use data::{Exemplar, HistogramPoint, MetricFamily, MetricKind, Point, PointValue};
pub use export::{InMemoryMetricExporter, MetricExporter};
pub use provider::{MeterProvider, MeterProviderBuilder};
pub use reader::PeriodicReaderConfig;
pub use store::{
    DEFAULT_BUCKETS, ExemplarFilter, MetricsRecorder, OVERFLOW_ATTRIBUTE, SECONDS_BUCKETS,
};
pub use units::ucum;
