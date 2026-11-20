//! A snapshot of every metric, for readers such as Prometheus exposition
//! and OTLP export. Values are cumulative from the provider's start time,
//! unless a family's [`Temporality`] says they're the change since the
//! reader's previous collection.

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

/// Whether a sum's or a histogram's values add up from a fixed start, or
/// count only what happened since the previous collection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Temporality {
    /// Totals since the start time, which stays the same.
    #[default]
    Cumulative,
    /// The change since the previous collection, which is the start time.
    Delta,
}

/// Which temporality an exporter asks for, as
/// `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE` names it. oxtel's
/// instruments are the `metrics` facade's synchronous ones, so `Delta` and
/// `LowMemory` ask for the same: delta counters and histograms. Gauges have
/// no temporality.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TemporalityPreference {
    /// Everything cumulative, the default.
    #[default]
    Cumulative,
    /// Counters and histograms as deltas; up-down sums cumulative.
    Delta,
    /// Synchronous counters and histograms as deltas, everything else
    /// cumulative.
    LowMemory,
}

impl TemporalityPreference {
    /// The preference named `cumulative`, `delta`, or `lowmemory` (or
    /// `low_memory`, as configuration files spell it), in any case.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "cumulative" => Some(TemporalityPreference::Cumulative),
            "delta" => Some(TemporalityPreference::Delta),
            "lowmemory" | "low_memory" => Some(TemporalityPreference::LowMemory),
            _ => None,
        }
    }

    /// The temporality it gives metrics of `kind`.
    pub fn temporality(self, kind: MetricKind) -> Temporality {
        match (self, kind) {
            (TemporalityPreference::Cumulative, _) | (_, MetricKind::Gauge) => {
                Temporality::Cumulative
            }
            (_, MetricKind::Counter | MetricKind::Histogram) => Temporality::Delta,
        }
    }
}

/// How histograms are aggregated, as
/// `OTEL_EXPORTER_OTLP_METRICS_DEFAULT_HISTOGRAM_AGGREGATION` names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HistogramAggregation {
    /// Explicit bucket boundaries (`explicit_bucket_histogram`), the
    /// default: the conventions' advice for the metric, or the
    /// specification's defaults.
    #[default]
    Explicit,
    /// Base-2 exponential buckets (`base2_exponential_bucket_histogram`),
    /// which adapt to the values' range with no boundaries to choose.
    Exponential {
        /// The most buckets for each sign; 160 by default.
        max_size: u32,
        /// The finest scale; 20 by default.
        max_scale: i8,
    },
}

impl HistogramAggregation {
    /// Exponential buckets with the specification's defaults.
    pub const EXPONENTIAL: HistogramAggregation = HistogramAggregation::Exponential {
        max_size: crate::metrics::exponential::DEFAULT_MAX_SIZE,
        max_scale: crate::metrics::exponential::DEFAULT_MAX_SCALE,
    };

    /// The aggregation named `explicit_bucket_histogram` or
    /// `base2_exponential_bucket_histogram`, in any case.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "explicit_bucket_histogram" => Some(HistogramAggregation::Explicit),
            "base2_exponential_bucket_histogram" => Some(HistogramAggregation::EXPONENTIAL),
            _ => None,
        }
    }
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

/// The buckets of one sign of an exponential histogram: `counts[i]` is the
/// bucket at index `offset + i`, which holds values in
/// `(base^(offset+i), base^(offset+i+1)]`, where `base` is
/// `2^(2^-scale)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExponentialBuckets {
    /// The first bucket's index.
    pub offset: i32,
    /// Counts per bucket, not cumulative.
    pub counts: Vec<u64>,
}

/// An exponential histogram's state.
#[derive(Clone, Debug, PartialEq)]
pub struct ExponentialHistogramPoint {
    /// The resolution: bucket boundaries are powers of `2^(2^-scale)`.
    pub scale: i32,
    /// Values that were exactly zero.
    pub zero_count: u64,
    /// Buckets for positive values.
    pub positive: ExponentialBuckets,
    /// Buckets for negative values, by magnitude.
    pub negative: ExponentialBuckets,
    /// The number of recorded values.
    pub count: u64,
    /// The sum of all recorded values.
    pub sum: f64,
    /// The smallest recorded value, if any.
    pub min: Option<f64>,
    /// The largest recorded value, if any.
    pub max: Option<f64>,
    /// Exemplars of recorded values.
    pub exemplars: Vec<Exemplar>,
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
    /// A histogram's state, in exponential buckets.
    ExponentialHistogram(ExponentialHistogramPoint),
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
    /// Whether the values are totals or changes. Gauges are always
    /// [`Temporality::Cumulative`].
    pub temporality: Temporality,
    /// When cumulative values started, or, for deltas, the previous
    /// collection.
    pub start_time: SystemTime,
    /// When the snapshot was taken.
    pub time: SystemTime,
}
