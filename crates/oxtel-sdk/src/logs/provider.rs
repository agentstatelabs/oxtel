//! The logger provider: one log pipeline, from layer to exporter.

use std::sync::Arc;

use crate::export::{BatchConfig, ExportError, ProcessorStats};
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
    None,
    Simple(Box<dyn FnOnce(Resource) -> SimpleLogProcessor>),
    Batch(Box<dyn FnOnce(Resource) -> BatchLogProcessor>),
    Custom(Arc<dyn LogProcessor>),
}

/// Builds a [`LoggerProvider`].
pub struct LoggerProviderBuilder {
    resource: Option<Resource>,
    limits: Option<LogLimits>,
    code_attributes: bool,
    processor: ProcessorChoice,
}

impl Default for LoggerProviderBuilder {
    fn default() -> Self {
        LoggerProviderBuilder {
            resource: None,
            limits: None,
            code_attributes: false,
            processor: ProcessorChoice::None,
        }
    }
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

    /// Export each record as it's emitted, on the emitting thread. For
    /// tests.
    pub fn with_simple_exporter(mut self, exporter: impl LogExporter) -> Self {
        self.processor = ProcessorChoice::Simple(Box::new(move |resource| {
            SimpleLogProcessor::new(exporter, resource)
        }));
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
        self.processor = ProcessorChoice::Batch(Box::new(move |resource| {
            BatchLogProcessor::new(exporter, resource, config)
        }));
        self
    }

    /// Use your own processor. It receives records with no resource
    /// attached; give it one yourself.
    pub fn with_processor(mut self, processor: impl LogProcessor) -> Self {
        self.processor = ProcessorChoice::Custom(Arc::new(processor));
        self
    }

    /// The provider.
    pub fn build(self) -> LoggerProvider {
        let resource = self.resource.unwrap_or_else(Resource::from_env);
        let processor: Arc<dyn LogProcessor> = match self.processor {
            ProcessorChoice::None => Arc::new(NoopLogProcessor),
            ProcessorChoice::Simple(make) => Arc::new(make(resource)),
            ProcessorChoice::Batch(make) => Arc::new(make(resource)),
            ProcessorChoice::Custom(p) => p,
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
