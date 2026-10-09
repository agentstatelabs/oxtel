//! The tracer provider: one trace pipeline, from layer to exporter.

use std::sync::Arc;

use crate::export::{BatchConfig, ExportError, ProcessorStats};
use crate::resource::Resource;
use crate::trace::export::SpanExporter;
use crate::trace::layer::{Shared, SpanLimits, TraceLayer};
use crate::trace::processor::{
    BatchSpanProcessor, NoopSpanProcessor, SimpleSpanProcessor, SpanProcessor,
};
use crate::trace::sampler::Sampler;

/// One trace pipeline. Hand its [`layer`](TracerProvider::layer) to a
/// `tracing` subscriber, and keep the provider alive: dropping it flushes
/// and shuts the pipeline down.
pub struct TracerProvider {
    shared: Arc<Shared>,
}

impl TracerProvider {
    /// A builder. Without an exporter, spans get ids but go nowhere.
    pub fn builder() -> TracerProviderBuilder {
        TracerProviderBuilder::default()
    }

    /// The layer to add to a `tracing` subscriber.
    pub fn layer(&self) -> TraceLayer {
        TraceLayer::new(Arc::clone(&self.shared))
    }

    /// Export everything that has ended so far, and wait for it.
    pub fn force_flush(&self) -> Result<(), ExportError> {
        self.shared.processor.force_flush()
    }

    /// Flush and stop. Later spans are dropped.
    pub fn shutdown(&self) -> Result<(), ExportError> {
        self.shared.processor.shutdown()
    }

    /// What happened to spans so far.
    pub fn stats(&self) -> ProcessorStats {
        self.shared.processor.stats()
    }
}

impl Drop for TracerProvider {
    fn drop(&mut self) {
        let _ = self.shared.processor.shutdown();
    }
}

enum ProcessorChoice {
    None,
    Simple(Box<dyn FnOnce(Resource) -> SimpleSpanProcessor>),
    Batch(Box<dyn FnOnce(Resource) -> BatchSpanProcessor>),
    Custom(Arc<dyn SpanProcessor>),
}

/// Builds a [`TracerProvider`].
pub struct TracerProviderBuilder {
    resource: Option<Resource>,
    sampler: Option<Sampler>,
    limits: Option<SpanLimits>,
    span_events: bool,
    processor: ProcessorChoice,
}

impl Default for TracerProviderBuilder {
    fn default() -> Self {
        TracerProviderBuilder {
            resource: None,
            sampler: None,
            limits: None,
            span_events: true,
            processor: ProcessorChoice::None,
        }
    }
}

impl TracerProviderBuilder {
    /// The resource. Default: [`Resource::from_env`].
    pub fn with_resource(mut self, resource: Resource) -> Self {
        self.resource = Some(resource);
        self
    }

    /// The sampler. Default: [`Sampler::from_env`].
    pub fn with_sampler(mut self, sampler: Sampler) -> Self {
        self.sampler = Some(sampler);
        self
    }

    /// Span limits. Default: [`SpanLimits::from_env`].
    pub fn with_span_limits(mut self, limits: SpanLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Whether `tracing` events inside a span become span events. Default:
    /// true.
    pub fn with_span_events(mut self, enabled: bool) -> Self {
        self.span_events = enabled;
        self
    }

    /// Export each span as it ends, on the ending thread. For tests.
    pub fn with_simple_exporter(mut self, exporter: impl SpanExporter) -> Self {
        self.processor = ProcessorChoice::Simple(Box::new(move |resource| {
            SimpleSpanProcessor::new(exporter, resource)
        }));
        self
    }

    /// Export in batches from a background thread, configured from the
    /// environment ([`BatchConfig::from_env`]).
    pub fn with_batch_exporter(self, exporter: impl SpanExporter) -> Self {
        self.with_batch_exporter_config(exporter, BatchConfig::from_env())
    }

    /// Export in batches with `config`.
    pub fn with_batch_exporter_config(
        mut self,
        exporter: impl SpanExporter,
        config: BatchConfig,
    ) -> Self {
        self.processor = ProcessorChoice::Batch(Box::new(move |resource| {
            BatchSpanProcessor::new(exporter, resource, config)
        }));
        self
    }

    /// Use your own processor. It receives spans with no resource attached;
    /// give it one yourself.
    pub fn with_processor(mut self, processor: impl SpanProcessor) -> Self {
        self.processor = ProcessorChoice::Custom(Arc::new(processor));
        self
    }

    /// The provider.
    pub fn build(self) -> TracerProvider {
        let resource = self.resource.unwrap_or_else(Resource::from_env);
        let processor: Arc<dyn SpanProcessor> = match self.processor {
            ProcessorChoice::None => Arc::new(NoopSpanProcessor),
            ProcessorChoice::Simple(make) => Arc::new(make(resource)),
            ProcessorChoice::Batch(make) => Arc::new(make(resource)),
            ProcessorChoice::Custom(p) => p,
        };
        TracerProvider {
            shared: Arc::new(Shared {
                processor,
                sampler: self.sampler.unwrap_or_else(Sampler::from_env),
                limits: self.limits.unwrap_or_else(SpanLimits::from_env),
                span_events: self.span_events,
            }),
        }
    }
}
