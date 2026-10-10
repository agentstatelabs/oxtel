//! The periodic reader: collects the store on a timer and exports it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::export::{Counters, ExportError, ProcessorStats};
use crate::metrics::export::MetricExporter;
use crate::metrics::store::Store;
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
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let counters = Arc::new(Counters::default());
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
) {
    let reader_metrics = ReaderMetrics::new("periodic_metric_reader");
    let export = || {
        let started = Instant::now();
        let metrics = store.collect();
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
