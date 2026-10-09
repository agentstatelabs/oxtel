//! Processors: what happens to a span when it ends.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::resource::Resource;
use crate::trace::data::SpanData;
use crate::trace::export::{ExportError, SpanExporter};

/// Receives spans as they end.
pub trait SpanProcessor: Send + Sync + 'static {
    /// A span ended. Must not block for long: it runs on the instrumented
    /// code's thread.
    fn on_end(&self, span: SpanData);

    /// Export everything received so far, and wait for it.
    fn force_flush(&self) -> Result<(), ExportError>;

    /// Flush, then stop. Spans that end afterwards are dropped.
    fn shutdown(&self) -> Result<(), ExportError>;

    /// Counts of what happened to spans so far.
    fn stats(&self) -> ProcessorStats {
        ProcessorStats::default()
    }
}

/// What happened to the spans a processor received.
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
struct Counters {
    exported: AtomicU64,
    export_failed: AtomicU64,
    dropped: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> ProcessorStats {
        ProcessorStats {
            exported: self.exported.load(Ordering::Relaxed),
            export_failed: self.export_failed.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }

    fn export(&self, exporter: &dyn SpanExporter, batch: &[SpanData], resource: &Resource) {
        let n = batch.len() as u64;
        match crate::suppress(|| exporter.export(batch, resource)) {
            Ok(()) => self.exported.fetch_add(n, Ordering::Relaxed),
            Err(_) => self.export_failed.fetch_add(n, Ordering::Relaxed),
        };
    }
}

/// Drops every span. Used when nothing is configured to export, so ids
/// still exist for logs and propagation.
#[derive(Debug, Default)]
pub struct NoopSpanProcessor;

impl SpanProcessor for NoopSpanProcessor {
    fn on_end(&self, _span: SpanData) {}
    fn force_flush(&self) -> Result<(), ExportError> {
        Ok(())
    }
    fn shutdown(&self) -> Result<(), ExportError> {
        Ok(())
    }
}

/// Exports each span as it ends, on the ending thread. For tests and
/// debugging; use [`BatchSpanProcessor`] in production.
pub struct SimpleSpanProcessor {
    exporter: Box<dyn SpanExporter>,
    resource: Resource,
    shut_down: AtomicBool,
    counters: Counters,
}

impl SimpleSpanProcessor {
    /// A processor exporting through `exporter`.
    pub fn new(exporter: impl SpanExporter, resource: Resource) -> Self {
        SimpleSpanProcessor {
            exporter: Box::new(exporter),
            resource,
            shut_down: AtomicBool::new(false),
            counters: Counters::default(),
        }
    }
}

impl SpanProcessor for SimpleSpanProcessor {
    fn on_end(&self, span: SpanData) {
        if self.shut_down.load(Ordering::Relaxed) {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.counters.export(
            self.exporter.as_ref(),
            std::slice::from_ref(&span),
            &self.resource,
        );
    }

    fn force_flush(&self) -> Result<(), ExportError> {
        Ok(())
    }

    fn shutdown(&self) -> Result<(), ExportError> {
        if self.shut_down.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        self.exporter.shutdown()
    }

    fn stats(&self) -> ProcessorStats {
        self.counters.snapshot()
    }
}

/// Settings for [`BatchSpanProcessor`]. Defaults follow the specification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchConfig {
    /// Spans held while waiting to export; more are dropped (2048).
    pub max_queue_size: usize,
    /// Longest wait before exporting what has queued (5 s).
    pub scheduled_delay: Duration,
    /// Most spans per export (512).
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
    /// Defaults, overridden by `OTEL_BSP_MAX_QUEUE_SIZE`,
    /// `OTEL_BSP_SCHEDULE_DELAY`, `OTEL_BSP_MAX_EXPORT_BATCH_SIZE` and
    /// `OTEL_BSP_EXPORT_TIMEOUT` (milliseconds).
    pub fn from_env() -> Self {
        let num = |name: &str| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        let d = BatchConfig::default();
        let max_queue_size = num("OTEL_BSP_MAX_QUEUE_SIZE")
            .map_or(d.max_queue_size, |v| v as usize)
            .max(1);
        BatchConfig {
            max_queue_size,
            scheduled_delay: num("OTEL_BSP_SCHEDULE_DELAY")
                .map_or(d.scheduled_delay, Duration::from_millis),
            // The batch can't exceed the queue.
            max_export_batch_size: num("OTEL_BSP_MAX_EXPORT_BATCH_SIZE")
                .map_or(d.max_export_batch_size, |v| v as usize)
                .clamp(1, max_queue_size),
            export_timeout: num("OTEL_BSP_EXPORT_TIMEOUT")
                .map_or(d.export_timeout, Duration::from_millis),
        }
    }
}

enum Message {
    Span(Box<SpanData>),
    Flush(mpsc::Sender<()>),
    Shutdown(mpsc::Sender<()>),
}

/// Queues spans and exports them in batches from its own thread. A full
/// queue drops spans, counted in [`ProcessorStats::dropped`], rather than
/// blocking the program.
pub struct BatchSpanProcessor {
    sender: SyncSender<Message>,
    worker: Mutex<Option<JoinHandle<()>>>,
    shut_down: AtomicBool,
    counters: Arc<Counters>,
    timeout: Duration,
}

impl BatchSpanProcessor {
    /// A processor exporting through `exporter` with `config`.
    pub fn new(exporter: impl SpanExporter, resource: Resource, config: BatchConfig) -> Self {
        let (sender, receiver) = mpsc::sync_channel(config.max_queue_size.max(1));
        let counters = Arc::new(Counters::default());
        let worker_counters = Arc::clone(&counters);
        let timeout = config.export_timeout;
        let worker = std::thread::Builder::new()
            .name("oxtel-span-export".into())
            .spawn(move || {
                crate::suppress(|| run(receiver, exporter, resource, config, &worker_counters))
            })
            .expect("spawn the span export thread");
        BatchSpanProcessor {
            sender,
            worker: Mutex::new(Some(worker)),
            shut_down: AtomicBool::new(false),
            counters,
            timeout,
        }
    }

    fn request(&self, make: impl FnOnce(mpsc::Sender<()>) -> Message) -> Result<(), ExportError> {
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

fn run(
    receiver: mpsc::Receiver<Message>,
    exporter: impl SpanExporter,
    resource: Resource,
    config: BatchConfig,
    counters: &Counters,
) {
    let mut batch: Vec<SpanData> = Vec::with_capacity(config.max_export_batch_size);
    let mut deadline = Instant::now() + config.scheduled_delay;
    let flush = |batch: &mut Vec<SpanData>| {
        for chunk in batch.chunks(config.max_export_batch_size) {
            counters.export(&exporter, chunk, &resource);
        }
        batch.clear();
    };
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(wait) {
            Ok(Message::Span(span)) => {
                batch.push(*span);
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
                let _ = exporter.shutdown();
                let _ = ack.send(());
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                flush(&mut batch);
                deadline = Instant::now() + config.scheduled_delay;
            }
            Err(RecvTimeoutError::Disconnected) => {
                flush(&mut batch);
                let _ = exporter.shutdown();
                return;
            }
        }
    }
}

impl SpanProcessor for BatchSpanProcessor {
    fn on_end(&self, span: SpanData) {
        if self.shut_down.load(Ordering::Relaxed) {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match self.sender.try_send(Message::Span(Box::new(span))) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn force_flush(&self) -> Result<(), ExportError> {
        if self.shut_down.load(Ordering::Relaxed) {
            return Err(ExportError::Shutdown);
        }
        self.request(Message::Flush)
    }

    fn shutdown(&self) -> Result<(), ExportError> {
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

    fn stats(&self) -> ProcessorStats {
        self.counters.snapshot()
    }
}

impl Drop for BatchSpanProcessor {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
