//! The logger provider: one log pipeline, from layer to exporter.

use std::sync::Arc;

use crate::export::{BatchConfig, ExportError, ProcessorStats};
use crate::logs::data::LogRecord;
use crate::logs::export::LogExporter;
use crate::logs::layer::{LogLayer, LogLimits, Shared};
use crate::logs::processor::{
    BatchLogProcessor, LogProcessor, NoopLogProcessor, SimpleLogProcessor,
};
use crate::resource::Resource;

/// One log pipeline. Hand its [`layer`](LoggerProvider::layer) to a
/// `tracing` subscriber, and keep the provider alive: dropping it flushes
/// and shuts the pipeline down.
pub struct LoggerProvider {
    shared: Arc<Shared>,
}

impl LoggerProvider {
    /// A builder. Without an exporter, records go nowhere.
    pub fn builder() -> LoggerProviderBuilder {
        LoggerProviderBuilder::default()
    }

    /// The layer to add to a `tracing` subscriber.
    pub fn layer(&self) -> LogLayer {
        LogLayer::new(Arc::clone(&self.shared))
    }

    /// Export everything emitted so far, and wait for it.
    pub fn force_flush(&self) -> Result<(), ExportError> {
        self.shared.processor.force_flush()
    }

    /// Flush and stop. Later records are dropped.
    pub fn shutdown(&self) -> Result<(), ExportError> {
        self.shared.processor.shutdown()
    }

    /// What happened to records so far.
    pub fn stats(&self) -> ProcessorStats {
        self.shared.processor.stats()
    }
}

impl Drop for LoggerProvider {
    fn drop(&mut self) {
        let _ = self.shared.processor.shutdown();
    }
}

enum ProcessorChoice {
    Simple(Box<dyn FnOnce(Resource) -> SimpleLogProcessor>),
    Batch(Box<dyn FnOnce(Resource) -> BatchLogProcessor>),
    Custom(Arc<dyn LogProcessor>),
}

/// Builds a [`LoggerProvider`].
#[derive(Default)]
pub struct LoggerProviderBuilder {
    resource: Option<Resource>,
    limits: Option<LogLimits>,
    code_attributes: bool,
    processors: Vec<ProcessorChoice>,
}

impl LoggerProviderBuilder {
    /// The resource. Default: [`Resource::from_env`].
    pub fn with_resource(mut self, resource: Resource) -> Self {
        self.resource = Some(resource);
        self
    }

    /// Attribute limits. Default: [`LogLimits::from_env`].
    pub fn with_limits(mut self, limits: LogLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Record each event's source file and line as `code.file.path` and
    /// `code.line.number`. Default: false.
    pub fn with_code_attributes(mut self, enabled: bool) -> Self {
        self.code_attributes = enabled;
        self
    }

    /// Export each record as it's emitted, on the emitting thread. For tests
    /// and the console. Exporters and processors add up: each record goes to
    /// all of them.
    pub fn with_simple_exporter(mut self, exporter: impl LogExporter) -> Self {
        self.processors
            .push(ProcessorChoice::Simple(Box::new(move |resource| {
                SimpleLogProcessor::new(exporter, resource)
            })));
        self
    }

    /// Export in batches from a background thread, configured from the
    /// environment ([`BatchConfig::logs_from_env`]).
    pub fn with_batch_exporter(self, exporter: impl LogExporter) -> Self {
        self.with_batch_exporter_config(exporter, BatchConfig::logs_from_env())
    }

    /// Export in batches with `config`.
    pub fn with_batch_exporter_config(
        mut self,
        exporter: impl LogExporter,
        config: BatchConfig,
    ) -> Self {
        self.processors
            .push(ProcessorChoice::Batch(Box::new(move |resource| {
                BatchLogProcessor::new(exporter, resource, config)
            })));
        self
    }

    /// Use your own processor. It receives records with no resource
    /// attached; give it one yourself.
    pub fn with_processor(mut self, processor: impl LogProcessor) -> Self {
        self.processors
            .push(ProcessorChoice::Custom(Arc::new(processor)));
        self
    }

    /// The provider.
    pub fn build(self) -> LoggerProvider {
        let resource = self.resource.unwrap_or_else(Resource::from_env);
        let mut processors: Vec<Arc<dyn LogProcessor>> = self
            .processors
            .into_iter()
            .map(|choice| -> Arc<dyn LogProcessor> {
                match choice {
                    ProcessorChoice::Simple(make) => Arc::new(make(resource.clone())),
                    ProcessorChoice::Batch(make) => Arc::new(make(resource.clone())),
                    ProcessorChoice::Custom(p) => p,
                }
            })
            .collect();
        let processor: Arc<dyn LogProcessor> = match processors.len() {
            0 => Arc::new(NoopLogProcessor),
            1 => processors.remove(0),
            _ => Arc::new(FanOut(processors)),
        };
        LoggerProvider {
            shared: Arc::new(Shared {
                processor,
                limits: self.limits.unwrap_or_else(LogLimits::from_env),
                code_attributes: self.code_attributes,
            }),
        }
    }
}

/// Hands each record to several processors.
struct FanOut(Vec<Arc<dyn LogProcessor>>);

impl LogProcessor for FanOut {
    fn on_emit(&self, item: LogRecord) {
        if let Some((last, rest)) = self.0.split_last() {
            for p in rest {
                p.on_emit(item.clone());
            }
            last.on_emit(item);
        }
    }

    fn force_flush(&self) -> Result<(), ExportError> {
        self.0
            .iter()
            .map(|p| p.force_flush())
            .fold(Ok(()), Result::and)
    }

    fn shutdown(&self) -> Result<(), ExportError> {
        self.0
            .iter()
            .map(|p| p.shutdown())
            .fold(Ok(()), Result::and)
    }

    fn stats(&self) -> ProcessorStats {
        self.0
            .iter()
            .map(|p| p.stats())
            .fold(ProcessorStats::default(), |a, b| ProcessorStats {
                exported: a.exported + b.exported,
                export_failed: a.export_failed + b.export_failed,
                dropped: a.dropped + b.dropped,
            })
    }
}
