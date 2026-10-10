//! Processors: what happens to a span when it ends.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::export::{BatchConfig, BatchSink, Batcher, Counters, ExportError, ProcessorStats};
use crate::resource::Resource;
use crate::sdk_metrics::ProcessorMetrics;
use crate::trace::data::SpanData;
use crate::trace::export::SpanExporter;

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
            counters: Counters::new(("span", "spans"), exporter.destination()),
            exporter: Box::new(exporter),
            resource,
            shut_down: AtomicBool::new(false),
        }
    }
}

impl SpanProcessor for SimpleSpanProcessor {
    fn on_end(&self, span: SpanData) {
        if self.shut_down.load(Ordering::Relaxed) {
            self.counters.drop_one();
            return;
        }
        let _ = self.counters.export(1, || {
            self.exporter
                .export(std::slice::from_ref(&span), &self.resource)
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

impl<E: SpanExporter> BatchSink<SpanData> for Sink<E> {
    fn export(&self, batch: &[SpanData]) -> Result<(), ExportError> {
        self.exporter.export(batch, &self.resource)
    }

    fn shutdown(&self) {
        let _ = self.exporter.shutdown();
    }

    fn destination(&self) -> String {
        self.exporter.destination()
    }
}

/// Queues spans and exports them in batches from its own thread. A full
/// queue drops spans, counted in [`ProcessorStats::dropped`], rather than
/// blocking the program.
pub struct BatchSpanProcessor {
    batcher: Batcher<SpanData>,
}

impl BatchSpanProcessor {
    /// A processor exporting through `exporter` with `config`.
    pub fn new(exporter: impl SpanExporter, resource: Resource, config: BatchConfig) -> Self {
        BatchSpanProcessor {
            batcher: Batcher::new(
                "oxtel-span-export",
                ("span", "spans"),
                ProcessorMetrics::new(
                    "batching_span_processor",
                    "otel.sdk.processor.span.processed",
                ),
                config,
                Sink { exporter, resource },
            ),
        }
    }
}

impl SpanProcessor for BatchSpanProcessor {
    fn on_end(&self, span: SpanData) {
        self.batcher.push(span);
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
