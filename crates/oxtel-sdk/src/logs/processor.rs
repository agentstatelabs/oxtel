//! Processors: what happens to a log record once it's emitted.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::export::{BatchConfig, BatchSink, Batcher, Counters, ExportError, ProcessorStats};
use crate::logs::data::LogRecord;
use crate::logs::export::LogExporter;
use crate::resource::Resource;

/// Receives log records as they're emitted.
pub trait LogProcessor: Send + Sync + 'static {
    /// A record was emitted. Must not block for long: it runs on the
    /// instrumented code's thread.
    fn on_emit(&self, record: LogRecord);

    /// Export everything received so far, and wait for it.
    fn force_flush(&self) -> Result<(), ExportError>;

    /// Flush, then stop. Records emitted afterwards are dropped.
    fn shutdown(&self) -> Result<(), ExportError>;

    /// Counts of what happened to records so far.
    fn stats(&self) -> ProcessorStats {
        ProcessorStats::default()
    }
}

/// Drops every record.
#[derive(Debug, Default)]
pub struct NoopLogProcessor;

impl LogProcessor for NoopLogProcessor {
    fn on_emit(&self, _record: LogRecord) {}
    fn force_flush(&self) -> Result<(), ExportError> {
        Ok(())
    }
    fn shutdown(&self) -> Result<(), ExportError> {
        Ok(())
    }
}

/// Exports each record as it's emitted, on the emitting thread. For tests
/// and debugging; use [`BatchLogProcessor`] in production.
pub struct SimpleLogProcessor {
    exporter: Box<dyn LogExporter>,
    resource: Resource,
    shut_down: AtomicBool,
    counters: Counters,
}

impl SimpleLogProcessor {
    /// A processor exporting through `exporter`.
    pub fn new(exporter: impl LogExporter, resource: Resource) -> Self {
        SimpleLogProcessor {
            exporter: Box::new(exporter),
            resource,
            shut_down: AtomicBool::new(false),
            counters: Counters::default(),
        }
    }
}

impl LogProcessor for SimpleLogProcessor {
    fn on_emit(&self, record: LogRecord) {
        if self.shut_down.load(Ordering::Relaxed) {
            self.counters.drop_one();
            return;
        }
        self.counters.export(1, || {
            self.exporter
                .export(std::slice::from_ref(&record), &self.resource)
        });
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

struct Sink<E> {
    exporter: E,
    resource: Resource,
}

impl<E: LogExporter> BatchSink<LogRecord> for Sink<E> {
    fn export(&self, batch: &[LogRecord]) -> Result<(), ExportError> {
        self.exporter.export(batch, &self.resource)
    }

    fn shutdown(&self) {
        let _ = self.exporter.shutdown();
    }
}

/// Queues records and exports them in batches from its own thread. A full
/// queue drops records, counted in [`ProcessorStats::dropped`], rather than
/// blocking the program.
pub struct BatchLogProcessor {
    batcher: Batcher<LogRecord>,
}

impl BatchLogProcessor {
    /// A processor exporting through `exporter` with `config`.
    pub fn new(exporter: impl LogExporter, resource: Resource, config: BatchConfig) -> Self {
        BatchLogProcessor {
            batcher: Batcher::new("oxtel-log-export", config, Sink { exporter, resource }),
        }
    }
}

impl LogProcessor for BatchLogProcessor {
    fn on_emit(&self, record: LogRecord) {
        self.batcher.push(record);
    }

    fn force_flush(&self) -> Result<(), ExportError> {
        self.batcher.force_flush()
    }

    fn shutdown(&self) -> Result<(), ExportError> {
        self.batcher.shutdown()
    }

    fn stats(&self) -> ProcessorStats {
        self.batcher.stats()
    }
}
