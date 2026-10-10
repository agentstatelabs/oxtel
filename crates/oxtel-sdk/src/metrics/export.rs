//! Exporters: where collected metrics go.

use std::sync::{Arc, Mutex};

use crate::export::ExportError;
use crate::metrics::data::MetricFamily;
use crate::resource::Resource;

/// Sends collected metrics somewhere. Called from a reader's thread, never
/// from the instrumented code's.
///
/// Values are cumulative: each export carries every series' total since the
/// provider started.
pub trait MetricExporter: Send + Sync + 'static {
    /// Export one collection.
    fn export(&self, metrics: &[MetricFamily], resource: &Resource) -> Result<(), ExportError>;

    /// Release resources. No exports follow.
    fn shutdown(&self) -> Result<(), ExportError> {
        Ok(())
    }
}

/// Keeps each export in memory, for tests.
#[derive(Clone, Default)]
pub struct InMemoryMetricExporter {
    exports: Arc<Mutex<Vec<Vec<MetricFamily>>>>,
    resource: Arc<Mutex<Option<Resource>>>,
}

impl InMemoryMetricExporter {
    /// Every export so far, oldest first.
    pub fn exports(&self) -> Vec<Vec<MetricFamily>> {
        self.exports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The most recent export, if any.
    pub fn last(&self) -> Option<Vec<MetricFamily>> {
        self.exports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .cloned()
    }

    /// The resource of the last export.
    pub fn resource(&self) -> Option<Resource> {
        self.resource
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Forget the exports so far.
    pub fn reset(&self) {
        self.exports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

impl MetricExporter for InMemoryMetricExporter {
    fn export(&self, metrics: &[MetricFamily], resource: &Resource) -> Result<(), ExportError> {
        self.exports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(metrics.to_vec());
        *self.resource.lock().unwrap_or_else(|e| e.into_inner()) = Some(resource.clone());
        Ok(())
    }
}
