//! Exporters: where log records go.

use std::sync::{Arc, Mutex};

use crate::export::ExportError;
use crate::logs::data::LogRecord;
use crate::resource::Resource;

/// Sends log records somewhere. Called from a processor's thread, never
/// from the instrumented code's (except by [`SimpleLogProcessor`]).
///
/// [`SimpleLogProcessor`]: crate::logs::SimpleLogProcessor
pub trait LogExporter: Send + Sync + 'static {
    /// Export one batch.
    fn export(&self, batch: &[LogRecord], resource: &Resource) -> Result<(), ExportError>;

    /// Release resources. No exports follow.
    fn shutdown(&self) -> Result<(), ExportError> {
        Ok(())
    }
}

/// Keeps exported log records in memory, for tests.
#[derive(Clone, Default)]
pub struct InMemoryLogExporter {
    records: Arc<Mutex<Vec<LogRecord>>>,
    resource: Arc<Mutex<Option<Resource>>>,
}

impl InMemoryLogExporter {
    /// The log records exported so far.
    pub fn records(&self) -> Vec<LogRecord> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The resource of the last export.
    pub fn resource(&self) -> Option<Resource> {
        self.resource
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Forget the log records exported so far.
    pub fn reset(&self) {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

impl LogExporter for InMemoryLogExporter {
    fn export(&self, batch: &[LogRecord], resource: &Resource) -> Result<(), ExportError> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(batch);
        *self.resource.lock().unwrap_or_else(|e| e.into_inner()) = Some(resource.clone());
        Ok(())
    }
}
