//! Where metric values live, and the `metrics` facade recorder that fills
//! it.
//!
//! Registration looks the key up in one of 16 shards, keyed by the facade's
//! precomputed key hash. Recording through a handle is an atomic operation,
//! plus, inside a sampled span, an exemplar.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use metrics::{Counter, CounterFn, Gauge, GaugeFn, Histogram, HistogramFn, Key, KeyName};
use metrics::{Metadata, Recorder, SharedString, Unit};

use crate::metrics::data::{Exemplar, HistogramPoint, MetricFamily, MetricKind, Point, PointValue};
use crate::metrics::units::ucum;
use crate::trace::{Scope, current_span_context};

/// Which measurements may become exemplars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExemplarFilter {
    /// Measurements made inside a sampled span. The specification's default.
    #[default]
    TraceBased,
    /// Every measurement, with trace ids when there is a span.
    AlwaysOn,
    /// None.
    AlwaysOff,
}

impl ExemplarFilter {
    /// `OTEL_METRICS_EXEMPLAR_FILTER`: `trace_based` (the default),
    /// `always_on` or `always_off`.
    pub fn from_env() -> Self {
        match std::env::var("OTEL_METRICS_EXEMPLAR_FILTER")
            .unwrap_or_default()
            .trim()
        {
            "always_on" => ExemplarFilter::AlwaysOn,
            "always_off" => ExemplarFilter::AlwaysOff,
            _ => ExemplarFilter::TraceBased,
        }
    }

    fn capture(self, value: f64) -> Option<Exemplar> {
        let (trace_id, span_id) = match self {
            ExemplarFilter::AlwaysOff => return None,
            ExemplarFilter::TraceBased => {
                let cx = current_span_context().filter(|cx| cx.is_sampled())?;
                (Some(cx.trace_id), Some(cx.span_id))
            }
            ExemplarFilter::AlwaysOn => match current_span_context() {
                Some(cx) => (Some(cx.trace_id), Some(cx.span_id)),
                None => (None, None),
            },
        };
        Some(Exemplar {
            value,
            time: SystemTime::now(),
            trace_id,
            span_id,
        })
    }
}

/// Bucket bounds the semantic conventions advise for durations in seconds.
pub const SECONDS_BUCKETS: [f64; 14] = [
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// The specification's default explicit bucket bounds.
pub const DEFAULT_BUCKETS: [f64; 15] = [
    0.0, 5.0, 10.0, 25.0, 50.0, 75.0, 100.0, 250.0, 500.0, 750.0, 1000.0, 2500.0, 5000.0, 7500.0,
    10000.0,
];

/// The attribute marking the series that collects measurements past the
/// cardinality limit.
pub const OVERFLOW_ATTRIBUTE: &str = "otel.metric.overflow";

pub(crate) struct Config {
    pub(crate) cardinality_limit: usize,
    pub(crate) exemplar_filter: ExemplarFilter,
    pub(crate) buckets: Vec<(String, Arc<[f64]>)>,
}

/// The facade's key hash is computed once per key; pass it through.
#[derive(Default)]
struct PassThrough(u64);

impl Hasher for PassThrough {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
}

type KeyMap<V> = HashMap<Key, V, BuildHasherDefault<PassThrough>>;

const SHARDS: usize = 16;

#[derive(Clone)]
enum Entry {
    Counter(Arc<CounterCell>),
    Gauge(Arc<GaugeCell>),
    Histogram(Arc<HistogramCell>),
    /// The name is already registered as another kind.
    Unusable,
}

struct Instrument {
    name: String,
    kind: MetricKind,
    scope: Scope,
    unit: Mutex<Option<String>>,
    description: Mutex<Option<String>>,
    bounds: Arc<[f64]>,
    series: Mutex<HashMap<Vec<(String, String)>, Entry>>,
}

pub(crate) struct Store {
    shards: Box<[RwLock<KeyMap<Entry>>]>,
    instruments: RwLock<HashMap<String, Arc<Instrument>>>,
    descriptions: Mutex<HashMap<String, (Option<Unit>, String)>>,
    config: Config,
    start_time: SystemTime,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Store {
    pub(crate) fn new(config: Config) -> Self {
        Store {
            shards: (0..SHARDS).map(|_| RwLock::default()).collect(),
            instruments: RwLock::default(),
            descriptions: Mutex::default(),
            config,
            start_time: SystemTime::now(),
        }
    }

    pub(crate) fn start_time(&self) -> SystemTime {
        self.start_time
    }

    fn describe(&self, name: &str, unit: Option<Unit>, description: &str) {
        lock(&self.descriptions).insert(name.to_string(), (unit, description.to_string()));
        if let Some(instrument) = self
            .instruments
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
        {
            if let Some(unit) = unit.and_then(ucum) {
                *lock(&instrument.unit) = Some(unit.to_string());
            }
            *lock(&instrument.description) = Some(description.to_string());
        }
    }

    fn register(&self, kind: MetricKind, key: &Key, meta: &Metadata<'_>) -> Entry {
        let shard = &self.shards[(key.get_hash() as usize) % SHARDS];
        if let Some(entry) = shard.read().unwrap_or_else(|e| e.into_inner()).get(key) {
            return entry.clone();
        }
        let entry = self.create(kind, key, meta);
        shard
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.clone())
            .or_insert(entry)
            .clone()
    }

    fn instrument(&self, kind: MetricKind, name: &str, meta: &Metadata<'_>) -> Arc<Instrument> {
        if let Some(i) = self
            .instruments
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
        {
            return Arc::clone(i);
        }
        let mut instruments = self.instruments.write().unwrap_or_else(|e| e.into_inner());
        let instrument = instruments.entry(name.to_string()).or_insert_with(|| {
            let (unit, description) = match lock(&self.descriptions).get(name) {
                Some((unit, description)) => (
                    unit.and_then(ucum).map(str::to_string),
                    Some(description.clone()),
                ),
                None => (None, None),
            };
            let bounds = self.bounds_for(name, unit.as_deref());
            let target = meta.target();
            let scope = target.split("::").next().unwrap_or(target).to_string();
            Arc::new(Instrument {
                name: name.to_string(),
                kind,
                scope: Scope {
                    name: Cow::Owned(scope),
                    version: None,
                },
                unit: Mutex::new(unit),
                description: Mutex::new(description),
                bounds,
                series: Mutex::default(),
            })
        });
        Arc::clone(instrument)
    }

    fn bounds_for(&self, name: &str, unit: Option<&str>) -> Arc<[f64]> {
        if let Some((_, bounds)) = self.config.buckets.iter().find(|(n, _)| n == name) {
            return Arc::clone(bounds);
        }
        match unit {
            Some("s") => Arc::from(SECONDS_BUCKETS.as_slice()),
            _ => Arc::from(DEFAULT_BUCKETS.as_slice()),
        }
    }

    fn create(&self, kind: MetricKind, key: &Key, meta: &Metadata<'_>) -> Entry {
        let instrument = self.instrument(kind, key.name(), meta);
        if instrument.kind != kind {
            return Entry::Unusable;
        }
        let mut labels: Vec<(String, String)> = key
            .labels()
            .map(|l| (l.key().to_string(), l.value().to_string()))
            .collect();
        labels.sort();
        labels.dedup_by(|later, earlier| later.0 == earlier.0);
        let mut series = lock(&instrument.series);
        if let Some(entry) = series.get(&labels) {
            return entry.clone();
        }
        let overflow = vec![(OVERFLOW_ATTRIBUTE.to_string(), "true".to_string())];
        let labels = if series.len() + 1 >= self.config.cardinality_limit.max(1) {
            if let Some(entry) = series.get(&overflow) {
                return entry.clone();
            }
            overflow
        } else {
            labels
        };
        let filter = self.config.exemplar_filter;
        let entry = match kind {
            MetricKind::Counter => Entry::Counter(Arc::new(CounterCell {
                value: AtomicU64::new(0),
                exemplar: Mutex::new(None),
                filter,
            })),
            MetricKind::Gauge => Entry::Gauge(Arc::new(GaugeCell {
                bits: AtomicU64::new(0f64.to_bits()),
            })),
            MetricKind::Histogram => {
                let buckets = instrument.bounds.len() + 1;
                Entry::Histogram(Arc::new(HistogramCell {
                    bounds: Arc::clone(&instrument.bounds),
                    counts: (0..buckets).map(|_| AtomicU64::new(0)).collect(),
                    sum: AtomicU64::new(0f64.to_bits()),
                    min: AtomicU64::new(f64::INFINITY.to_bits()),
                    max: AtomicU64::new(f64::NEG_INFINITY.to_bits()),
                    exemplars: Mutex::new(vec![None; buckets]),
                    filter,
                }))
            }
        };
        series.insert(labels, entry.clone());
        entry
    }

    pub(crate) fn collect(&self) -> Vec<MetricFamily> {
        let time = SystemTime::now();
        let mut instruments: Vec<Arc<Instrument>> = self
            .instruments
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        instruments.sort_by(|a, b| a.name.cmp(&b.name));
        instruments
            .into_iter()
            .map(|instrument| {
                let mut points: Vec<Point> = lock(&instrument.series)
                    .iter()
                    .filter_map(|(labels, entry)| entry.point(labels))
                    .collect();
                points.sort_by(|a, b| a.attributes.cmp(&b.attributes));
                MetricFamily {
                    name: instrument.name.clone(),
                    description: lock(&instrument.description).clone(),
                    unit: lock(&instrument.unit).clone(),
                    kind: instrument.kind,
                    scope: instrument.scope.clone(),
                    points,
                    start_time: self.start_time,
                    time,
                }
            })
            .collect()
    }
}

impl Entry {
    fn point(&self, labels: &[(String, String)]) -> Option<Point> {
        let (value, exemplar) = match self {
            Entry::Counter(c) => (
                PointValue::Counter(c.value.load(Ordering::Relaxed)),
                lock(&c.exemplar).clone(),
            ),
            Entry::Gauge(g) => (
                PointValue::Gauge(f64::from_bits(g.bits.load(Ordering::Relaxed))),
                None,
            ),
            Entry::Histogram(h) => (PointValue::Histogram(h.snapshot()), None),
            Entry::Unusable => return None,
        };
        Some(Point {
            attributes: labels.to_vec(),
            value,
            exemplar,
        })
    }
}

struct CounterCell {
    value: AtomicU64,
    exemplar: Mutex<Option<Exemplar>>,
    filter: ExemplarFilter,
}

impl CounterFn for CounterCell {
    fn increment(&self, value: u64) {
        self.value.fetch_add(value, Ordering::Relaxed);
        if let Some(e) = self.filter.capture(value as f64)
            && let Ok(mut slot) = self.exemplar.try_lock()
        {
            *slot = Some(e);
        }
    }

    fn absolute(&self, value: u64) {
        self.value.fetch_max(value, Ordering::Relaxed);
    }
}

struct GaugeCell {
    bits: AtomicU64,
}

fn update_f64(cell: &AtomicU64, f: impl Fn(f64) -> f64) {
    let _ = cell.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
        Some(f(f64::from_bits(bits)).to_bits())
    });
}

impl GaugeFn for GaugeCell {
    fn increment(&self, value: f64) {
        update_f64(&self.bits, |v| v + value);
    }
    fn decrement(&self, value: f64) {
        update_f64(&self.bits, |v| v - value);
    }
    fn set(&self, value: f64) {
        self.bits.store(value.to_bits(), Ordering::Relaxed);
    }
}

struct HistogramCell {
    bounds: Arc<[f64]>,
    counts: Box<[AtomicU64]>,
    sum: AtomicU64,
    min: AtomicU64,
    max: AtomicU64,
    exemplars: Mutex<Vec<Option<Exemplar>>>,
    filter: ExemplarFilter,
}

impl HistogramFn for HistogramCell {
    fn record(&self, value: f64) {
        if value.is_nan() {
            return;
        }
        let bucket = self.bounds.partition_point(|&bound| bound < value);
        self.counts[bucket].fetch_add(1, Ordering::Relaxed);
        update_f64(&self.sum, |s| s + value);
        update_f64(&self.min, |m| m.min(value));
        update_f64(&self.max, |m| m.max(value));
        if let Some(e) = self.filter.capture(value)
            && let Ok(mut slots) = self.exemplars.try_lock()
        {
            slots[bucket] = Some(e);
        }
    }
}

impl HistogramCell {
    fn snapshot(&self) -> HistogramPoint {
        let bucket_counts: Vec<u64> = self
            .counts
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect();
        let any = bucket_counts.iter().any(|&c| c > 0);
        let min = f64::from_bits(self.min.load(Ordering::Relaxed));
        let max = f64::from_bits(self.max.load(Ordering::Relaxed));
        HistogramPoint {
            bounds: Arc::clone(&self.bounds),
            bucket_counts,
            sum: f64::from_bits(self.sum.load(Ordering::Relaxed)),
            min: any.then_some(min),
            max: any.then_some(max),
            exemplars: lock(&self.exemplars).clone(),
        }
    }
}

/// The `metrics` facade recorder. Install it with
/// [`metrics::set_global_recorder`].
#[derive(Clone)]
pub struct MetricsRecorder {
    pub(crate) store: Arc<Store>,
}

impl Recorder for MetricsRecorder {
    fn describe_counter(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.store.describe(key.as_str(), unit, &description);
    }

    fn describe_gauge(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.store.describe(key.as_str(), unit, &description);
    }

    fn describe_histogram(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        self.store.describe(key.as_str(), unit, &description);
    }

    fn register_counter(&self, key: &Key, metadata: &Metadata<'_>) -> Counter {
        match self.store.register(MetricKind::Counter, key, metadata) {
            Entry::Counter(c) => Counter::from_arc(c),
            _ => Counter::noop(),
        }
    }

    fn register_gauge(&self, key: &Key, metadata: &Metadata<'_>) -> Gauge {
        match self.store.register(MetricKind::Gauge, key, metadata) {
            Entry::Gauge(g) => Gauge::from_arc(g),
            _ => Gauge::noop(),
        }
    }

    fn register_histogram(&self, key: &Key, metadata: &Metadata<'_>) -> Histogram {
        match self.store.register(MetricKind::Histogram, key, metadata) {
            Entry::Histogram(h) => Histogram::from_arc(h),
            _ => Histogram::noop(),
        }
    }
}
