//! A snapshot of every metric, for readers such as Prometheus exposition
//! and OTLP export. Values are cumulative from the provider's start time.

use std::sync::Arc;
use std::time::SystemTime;

use crate::id::{SpanId, TraceId};
use crate::trace::Scope;

/// What a metric measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MetricKind {
    /// A monotonic sum, such as requests served.
    Counter,
    /// The latest value, such as items in a queue.
    Gauge,
    /// A distribution in explicit buckets, such as request durations.
    Histogram,
}

/// A measurement kept as an example of a point's values, linking it to the
/// trace in which it was recorded.
#[derive(Clone, Debug, PartialEq)]
pub struct Exemplar {
    /// The measured value.
    pub value: f64,
    /// When it was recorded.
    pub time: SystemTime,
    /// The trace it was recorded in, if any.
    pub trace_id: Option<TraceId>,
    /// The span it was recorded in, if any.
    pub span_id: Option<SpanId>,
}

/// A histogram's state.
#[derive(Clone, Debug, PartialEq)]
pub struct HistogramPoint {
    /// Upper bounds of the buckets, ascending. A value `v` falls in the
    /// first bucket whose bound is `>= v`; past the last bound, in the
    /// overflow bucket.
    pub bounds: Arc<[f64]>,
    /// Counts per bucket, not cumulative; one more than `bounds`.
    pub bucket_counts: Vec<u64>,
    /// The sum of all recorded values.
    pub sum: f64,
    /// The smallest recorded value, if any.
    pub min: Option<f64>,
    /// The largest recorded value, if any.
    pub max: Option<f64>,
    /// The latest exemplar per bucket.
    pub exemplars: Vec<Option<Exemplar>>,
}

impl HistogramPoint {
    /// The number of recorded values.
    pub fn count(&self) -> u64 {
        self.bucket_counts.iter().sum()
    }
}

/// A point's value.
#[derive(Clone, Debug, PartialEq)]
pub enum PointValue {
    /// A counter's total.
    Counter(u64),
    /// A gauge's latest value.
    Gauge(f64),
    /// A histogram's state.
    Histogram(HistogramPoint),
}

/// One series: a metric's value for one set of attributes.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    /// The attributes, sorted by key.
    pub attributes: Vec<(String, String)>,
    /// The value.
    pub value: PointValue,
    /// For counters, the latest exemplar. Histograms keep theirs per
    /// bucket.
    pub exemplar: Option<Exemplar>,
}

/// A metric and all of its series.
#[derive(Clone, Debug, PartialEq)]
pub struct MetricFamily {
    /// The metric's name, as recorded, such as
    /// `http.server.request.duration`.
    pub name: String,
    /// Its description, if one was given.
    pub description: Option<String>,
    /// Its unit in UCUM, such as `s` or `By`, if one was given.
    pub unit: Option<String>,
    /// What it measures.
    pub kind: MetricKind,
    /// What recorded it: the crate at the root of its first callsite's
    /// target.
    pub scope: Scope,
    /// Its series, sorted by attributes.
    pub points: Vec<Point>,
    /// When cumulative values started.
    pub start_time: SystemTime,
    /// When the snapshot was taken.
    pub time: SystemTime,
}
