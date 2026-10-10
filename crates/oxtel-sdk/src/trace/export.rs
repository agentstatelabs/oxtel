//! Exporters: where finished spans go.

use std::sync::{Arc, Mutex};

use crate::export::ExportError;
use crate::resource::Resource;
use crate::trace::data::SpanData;

/// Sends finished spans somewhere. Called from a processor's thread, never
/// from the instrumented code's.
pub trait SpanExporter: Send + Sync + 'static {
    /// Export one batch.
    fn export(&self, batch: &[SpanData], resource: &Resource) -> Result<(), ExportError>;

    /// Where this exporter sends data, in a few words for people, such as
    /// `http://collector:4318/v1/traces (OTLP)`. oxtel's warnings about
    /// failed exports name it. Never include credentials.
    fn destination(&self) -> String {
        "a custom exporter".into()
    }

    /// Release resources. No exports follow.
    fn shutdown(&self) -> Result<(), ExportError> {
        Ok(())
    }
}

/// Keeps exported spans in memory, for tests.
#[derive(Clone, Default)]
pub struct InMemorySpanExporter {
    spans: Arc<Mutex<Vec<SpanData>>>,
    resource: Arc<Mutex<Option<Resource>>>,
}

impl InMemorySpanExporter {
    /// The spans exported so far.
    pub fn finished_spans(&self) -> Vec<SpanData> {
        self.spans.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The resource of the last export.
    pub fn resource(&self) -> Option<Resource> {
        self.resource
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Forget the spans exported so far.
    pub fn reset(&self) {
        self.spans.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

impl SpanExporter for InMemorySpanExporter {
    fn destination(&self) -> String {
        "memory".into()
    }

    fn export(&self, batch: &[SpanData], resource: &Resource) -> Result<(), ExportError> {
        self.spans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(batch);
        *self.resource.lock().unwrap_or_else(|e| e.into_inner()) = Some(resource.clone());
        Ok(())
    }
}
