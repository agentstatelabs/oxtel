//! What every signal's pipeline shares: export errors, processor counts,
//! and the worker thread behind the batch processors.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::sdk_metrics::ProcessorMetrics;

/// Why an export failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportError {
    /// The export didn't finish within its timeout.
    Timeout,
    /// The data couldn't be delivered: connection, TLS or I/O failure.
    Transport(String),
    /// The receiver refused the data.
    Rejected {
        /// The response status, such as an HTTP status code.
        status: u16,
        /// The receiver's explanation, if any.
        message: String,
    },
    /// The exporter or processor has been shut down.
    Shutdown,
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Timeout => write!(f, "export timed out"),
            ExportError::Transport(e) => write!(f, "export failed: {e}"),
            ExportError::Rejected { status, message } => {
                write!(f, "export rejected ({status}): {message}")
            }
            ExportError::Shutdown => write!(f, "exporter is shut down"),
        }
    }
}

impl std::error::Error for ExportError {}

/// What happened to the items (spans or log records) a processor received.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessorStats {
    /// Exported successfully.
    pub exported: u64,
    /// Lost because the exporter failed.
    pub export_failed: u64,
    /// Dropped because the queue was full or the processor was shut down.
    pub dropped: u64,
}

#[derive(Default)]
pub(crate) struct Counters {
    exported: AtomicU64,
    export_failed: AtomicU64,
    dropped: AtomicU64,
}

impl Counters {
    pub(crate) fn snapshot(&self) -> ProcessorStats {
        ProcessorStats {
            exported: self.exported.load(Ordering::Relaxed),
            export_failed: self.export_failed.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn drop_one(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Run one export of `n` items with oxtel's own telemetry suppressed,
    /// count how it went, and return its result.
    pub(crate) fn export(
        &self,
        n: usize,
        export: impl FnOnce() -> Result<(), ExportError>,
    ) -> Result<(), ExportError> {
        let result = crate::suppress(export);
        match result {
            Ok(()) => self.exported.fetch_add(n as u64, Ordering::Relaxed),
            Err(_) => self.export_failed.fetch_add(n as u64, Ordering::Relaxed),
        };
        result
    }
}

/// Settings for the batch processors. Defaults follow the specification for
/// spans; [`BatchConfig::logs`] has the defaults for log records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchConfig {
    /// Items held while waiting to export; more are dropped (2048).
    pub max_queue_size: usize,
    /// Longest wait before exporting what has queued (5 s for spans, 1 s for
    /// log records).
    pub scheduled_delay: Duration,
    /// Most items per export (512).
    pub max_export_batch_size: usize,
    /// How long `force_flush` and `shutdown` wait (30 s).
    pub export_timeout: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        BatchConfig {
            max_queue_size: 2048,
            scheduled_delay: Duration::from_millis(5000),
            max_export_batch_size: 512,
            export_timeout: Duration::from_millis(30_000),
        }
    }
}

impl BatchConfig {
    /// The specification's defaults for log records: like spans', but
    /// exported every second.
    pub fn logs() -> Self {
        BatchConfig {
            scheduled_delay: Duration::from_millis(1000),
            ..BatchConfig::default()
        }
    }

    /// Span defaults, overridden by `OTEL_BSP_MAX_QUEUE_SIZE`,
    /// `OTEL_BSP_SCHEDULE_DELAY`, `OTEL_BSP_MAX_EXPORT_BATCH_SIZE` and
    /// `OTEL_BSP_EXPORT_TIMEOUT` (milliseconds).
    pub fn from_env() -> Self {
        Self::from_env_with("OTEL_BSP", BatchConfig::default())
    }

    /// Log record defaults, overridden by the `OTEL_BLRP_*` variables named
    /// like [`from_env`](BatchConfig::from_env)'s.
    pub fn logs_from_env() -> Self {
        Self::from_env_with("OTEL_BLRP", BatchConfig::logs())
    }

    fn from_env_with(prefix: &str, d: BatchConfig) -> Self {
        let num = |name: &str| {
            std::env::var(format!("{prefix}_{name}"))
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        let max_queue_size = num("MAX_QUEUE_SIZE")
            .map_or(d.max_queue_size, |v| v as usize)
            .max(1);
        BatchConfig {
            max_queue_size,
            scheduled_delay: num("SCHEDULE_DELAY").map_or(d.scheduled_delay, Duration::from_millis),
            // The batch can't exceed the queue.
            max_export_batch_size: num("MAX_EXPORT_BATCH_SIZE")
                .map_or(d.max_export_batch_size, |v| v as usize)
                .clamp(1, max_queue_size),
            export_timeout: num("EXPORT_TIMEOUT").map_or(d.export_timeout, Duration::from_millis),
        }
    }
}

/// Where a [`Batcher`] sends its batches.
pub(crate) trait BatchSink<T>: Send + 'static {
    fn export(&self, batch: &[T]) -> Result<(), ExportError>;
    fn shutdown(&self);
}

enum Message<T> {
    Item(Box<T>),
    Flush(mpsc::Sender<()>),
    Shutdown(mpsc::Sender<()>),
}

/// Queues items and exports them in batches from its own thread. A full
/// queue drops items, counted in [`ProcessorStats::dropped`], rather than
/// blocking the program.
pub(crate) struct Batcher<T: Send + 'static> {
    sender: SyncSender<Message<T>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    shut_down: AtomicBool,
    counters: Arc<Counters>,
    metrics: Arc<ProcessorMetrics>,
    timeout: Duration,
}

impl<T: Send + 'static> Batcher<T> {
    pub(crate) fn new(
        thread: &str,
        metrics: ProcessorMetrics,
        config: BatchConfig,
        sink: impl BatchSink<T>,
    ) -> Self {
        let (sender, receiver) = mpsc::sync_channel(config.max_queue_size.max(1));
        let counters = Arc::new(Counters::default());
        let metrics = Arc::new(metrics);
        let (worker_counters, worker_metrics) = (Arc::clone(&counters), Arc::clone(&metrics));
        let timeout = config.export_timeout;
        let worker = std::thread::Builder::new()
            .name(thread.into())
            .spawn(move || {
                crate::suppress(|| run(receiver, sink, config, &worker_counters, &worker_metrics))
            })
            .expect("spawn an export thread");
        Batcher {
            sender,
            worker: Mutex::new(Some(worker)),
            shut_down: AtomicBool::new(false),
            counters,
            metrics,
            timeout,
        }
    }

    /// Queue `item`, or drop and count it if the queue is full.
    pub(crate) fn push(&self, item: T) {
        if self.shut_down.load(Ordering::Relaxed) {
            self.counters.drop_one();
            self.metrics.dropped("shutdown");
            return;
        }
        match self.sender.try_send(Message::Item(Box::new(item))) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.counters.drop_one();
                self.metrics.dropped("queue_full");
            }
            Err(TrySendError::Disconnected(_)) => {
                self.counters.drop_one();
                self.metrics.dropped("shutdown");
            }
        }
    }

    pub(crate) fn force_flush(&self) -> Result<(), ExportError> {
        if self.shut_down.load(Ordering::Relaxed) {
            return Err(ExportError::Shutdown);
        }
        self.request(Message::Flush)
    }

    pub(crate) fn shutdown(&self) -> Result<(), ExportError> {
        if self.shut_down.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        let result = self.request(Message::Shutdown);
        if let Some(worker) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take()
            && result.is_ok()
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
        make: impl FnOnce(mpsc::Sender<()>) -> Message<T>,
    ) -> Result<(), ExportError> {
        let (ack, done) = mpsc::channel();
        self.sender
            .send(make(ack))
            .map_err(|_| ExportError::Shutdown)?;
        done.recv_timeout(self.timeout).map_err(|e| match e {
            RecvTimeoutError::Timeout => ExportError::Timeout,
            RecvTimeoutError::Disconnected => ExportError::Shutdown,
        })
    }
}

impl<T: Send + 'static> Drop for Batcher<T> {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn run<T>(
    receiver: mpsc::Receiver<Message<T>>,
    sink: impl BatchSink<T>,
    config: BatchConfig,
    counters: &Counters,
    metrics: &ProcessorMetrics,
) {
    let mut batch: Vec<T> = Vec::with_capacity(config.max_export_batch_size);
    let mut deadline = Instant::now() + config.scheduled_delay;
    let flush = |batch: &mut Vec<T>| {
        for chunk in batch.chunks(config.max_export_batch_size) {
            metrics.handed_over(chunk.len());
            let _ = counters.export(chunk.len(), || sink.export(chunk));
        }
        batch.clear();
    };
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(wait) {
            Ok(Message::Item(item)) => {
                batch.push(*item);
                if batch.len() >= config.max_export_batch_size {
                    flush(&mut batch);
                    deadline = Instant::now() + config.scheduled_delay;
                }
            }
            Ok(Message::Flush(ack)) => {
                flush(&mut batch);
                let _ = ack.send(());
            }
            Ok(Message::Shutdown(ack)) => {
                flush(&mut batch);
                sink.shutdown();
                let _ = ack.send(());
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                flush(&mut batch);
                deadline = Instant::now() + config.scheduled_delay;
            }
            Err(RecvTimeoutError::Disconnected) => {
                flush(&mut batch);
                sink.shutdown();
                return;
            }
        }
    }
}
