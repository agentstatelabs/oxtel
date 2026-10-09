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

mod adder;
mod data;
mod provider;
mod store;
mod units;

pub use data::{Exemplar, HistogramPoint, MetricFamily, MetricKind, Point, PointValue};
pub use provider::{MeterProvider, MeterProviderBuilder};
pub use store::{
    DEFAULT_BUCKETS, ExemplarFilter, MetricsRecorder, OVERFLOW_ATTRIBUTE, SECONDS_BUCKETS,
};
pub use units::ucum;
