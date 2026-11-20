//! The periodic reader: collects the store on a timer and exports it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crate::export::{Counters, ExportError, ProcessorStats};
use crate::metrics::data::{MetricFamily, MetricKind, PointValue, Temporality};
use crate::metrics::exponential;
use crate::metrics::export::MetricExporter;
use crate::metrics::store::{Collection, Store};
use crate::resource::Resource;
use crate::sdk_metrics::ReaderMetrics;

/// Settings for a periodic reader. Defaults follow the specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeriodicReaderConfig {
    /// Time between exports (60 s).
    pub interval: Duration,
    /// How long `force_flush` and `shutdown` wait for an export (30 s).
    pub timeout: Duration,
}

impl Default for PeriodicReaderConfig {
    fn default() -> Self {
        PeriodicReaderConfig {
            interval: Duration::from_millis(60_000),
            timeout: Duration::from_millis(30_000),
        }
    }
}

impl PeriodicReaderConfig {
    /// Defaults, overridden by `OTEL_METRIC_EXPORT_INTERVAL` and
    /// `OTEL_METRIC_EXPORT_TIMEOUT` (milliseconds).
    pub fn from_env() -> Self {
        let ms = |name: &str| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .filter(|&v| v > 0)
                .map(Duration::from_millis)
        };
        let d = PeriodicReaderConfig::default();
        PeriodicReaderConfig {
            interval: ms("OTEL_METRIC_EXPORT_INTERVAL").unwrap_or(d.interval),
            timeout: ms("OTEL_METRIC_EXPORT_TIMEOUT").unwrap_or(d.timeout),
        }
    }
}

enum Control {
    Flush(mpsc::Sender<Result<(), ExportError>>),
    Shutdown(mpsc::Sender<Result<(), ExportError>>),
}

/// Exports the store every interval from its own thread, and once more on
/// shutdown. Counts are of data points.
pub(crate) struct PeriodicReader {
    sender: mpsc::Sender<Control>,
    worker: Mutex<Option<JoinHandle<()>>>,
    shut_down: AtomicBool,
    counters: Arc<Counters>,
    timeout: Duration,
}

impl PeriodicReader {
    pub(crate) fn new(
        store: Arc<Store>,
        resource: Resource,
        exporter: Box<dyn MetricExporter>,
        config: PeriodicReaderConfig,
        collection: Collection,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let counters = Arc::new(Counters::new(
            ("metric point", "metric points"),
            exporter.destination(),
        ));
        let worker_counters = Arc::clone(&counters);
        let worker = std::thread::Builder::new()
            .name("oxtel-metric-export".into())
            .spawn(move || {
                crate::suppress(|| {
                    run(
                        &receiver,
                        &store,
                        &resource,
                        exporter.as_ref(),
                        config.interval,
                        &worker_counters,
                        collection,
                    )
                })
            })
            .expect("spawn the metric export thread");
        PeriodicReader {
            sender,
            worker: Mutex::new(Some(worker)),
            shut_down: AtomicBool::new(false),
            counters,
            timeout: config.timeout,
        }
    }

    pub(crate) fn force_flush(&self) -> Result<(), ExportError> {
        if self.shut_down.load(Ordering::Relaxed) {
            return Err(ExportError::Shutdown);
        }
        self.request(Control::Flush)
    }

    pub(crate) fn shutdown(&self) -> Result<(), ExportError> {
        if self.shut_down.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        let result = self.request(Control::Shutdown);
        if let Some(worker) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take()
            && result != Err(ExportError::Timeout)
        {
            let _ = worker.join();
        }
        result
    }

    pub(crate) fn stats(&self) -> ProcessorStats {
        self.counters.snapshot()
    }

    fn request(
        &self,
        make: impl FnOnce(mpsc::Sender<Result<(), ExportError>>) -> Control,
    ) -> Result<(), ExportError> {
        let (ack, done) = mpsc::channel();
        self.sender
            .send(make(ack))
            .map_err(|_| ExportError::Shutdown)?;
        match done.recv_timeout(self.timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(ExportError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(ExportError::Shutdown),
        }
    }
}

impl Drop for PeriodicReader {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn run(
    receiver: &mpsc::Receiver<Control>,
    store: &Store,
    resource: &Resource,
    exporter: &dyn MetricExporter,
    interval: Duration,
    counters: &Counters,
    collection: Collection,
) {
    let reader_metrics = ReaderMetrics::new("periodic_metric_reader");
    let mut deltas = Deltas {
        since: store.start_time(),
        last: HashMap::new(),
    };
    let mut export = || {
        let started = Instant::now();
        let metrics = deltas.apply(store.collect_with(collection), exporter);
        reader_metrics.collected(started.elapsed());
        let points: usize = metrics.iter().map(|m| m.points.len()).sum();
        if points == 0 {
            return Ok(());
        }
        counters.export(points, || exporter.export(&metrics, resource))
    };
    let mut next = Instant::now() + interval;
    loop {
        match receiver.recv_timeout(next.saturating_duration_since(Instant::now())) {
            Err(RecvTimeoutError::Timeout) => {
                let _ = export();
                next = Instant::now() + interval;
            }
            Ok(Control::Flush(ack)) => {
                let _ = ack.send(export());
            }
            Ok(Control::Shutdown(ack)) => {
                let result = export();
                let _ = exporter.shutdown();
                let _ = ack.send(result);
                return;
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = export();
                let _ = exporter.shutdown();
                return;
            }
        }
    }
}

/// What a reader exported last, to turn cumulative values into deltas:
/// each series' total then, and when that was.
struct Deltas {
    since: SystemTime,
    last: HashMap<(String, Vec<(String, String)>), PointValue>,
}

impl Deltas {
    /// `families`, cumulative, as `exporter` wants them. Counters and
    /// histograms it wants as deltas become the change since the previous
    /// collection, and series that didn't change are left out, as the
    /// specification requires of synchronous instruments. Exemplars from
    /// before then are left out too.
    fn apply(
        &mut self,
        families: Vec<MetricFamily>,
        exporter: &dyn MetricExporter,
    ) -> Vec<MetricFamily> {
        let since = self.since;
        let mut collected = None;
        let families = families
            .into_iter()
            .map(|mut family| {
                collected = Some(family.time);
                if family.kind == MetricKind::Gauge
                    || exporter.temporality(family.kind) != Temporality::Delta
                {
                    return family;
                }
                family.temporality = Temporality::Delta;
                family.start_time = since;
                let points = std::mem::take(&mut family.points);
                family.points = points
                    .into_iter()
                    .filter_map(|mut point| {
                        let key = (family.name.clone(), point.attributes.clone());
                        let before = self.last.insert(key, point.value.clone());
                        let changed = match (&mut point.value, before) {
                            (PointValue::Counter(now), Some(PointValue::Counter(before))) => {
                                *now = now.saturating_sub(before);
                                *now > 0
                            }
                            (PointValue::Counter(now), _) => *now > 0,
                            (PointValue::Histogram(now), Some(PointValue::Histogram(before)))
                                if now.bounds == before.bounds =>
                            {
                                for (n, b) in
                                    now.bucket_counts.iter_mut().zip(&before.bucket_counts)
                                {
                                    *n = n.saturating_sub(*b);
                                }
                                now.sum -= before.sum;
                                now.count() > 0
                            }
                            (PointValue::Histogram(now), _) => now.count() > 0,
                            (
                                PointValue::ExponentialHistogram(now),
                                Some(PointValue::ExponentialHistogram(before)),
                            ) => {
                                *now = exponential::delta(now, &before);
                                now.count > 0
                            }
                            (PointValue::ExponentialHistogram(now), _) => now.count > 0,
                            (PointValue::Gauge(_), _) => true,
                        };
                        let recent = |e: &crate::metrics::Exemplar| e.time > since;
                        point.exemplar = point.exemplar.filter(recent);
                        match &mut point.value {
                            PointValue::Histogram(h) => {
                                for slot in &mut h.exemplars {
                                    *slot = slot.take().filter(recent);
                                }
                            }
                            PointValue::ExponentialHistogram(h) => h.exemplars.retain(recent),
                            _ => {}
                        }
                        changed.then_some(point)
                    })
                    .collect();
                family
            })
            .collect();
        if let Some(time) = collected {
            self.since = time;
        }
        families
    }
}
