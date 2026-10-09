//! Exporters: where finished spans go.

use std::fmt;
use std::sync::{Arc, Mutex};

use crate::resource::Resource;
use crate::trace::data::SpanData;

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

/// Sends finished spans somewhere. Called from a processor's thread, never
/// from the instrumented code's.
pub trait SpanExporter: Send + Sync + 'static {
    /// Export one batch.
    fn export(&self, batch: &[SpanData], resource: &Resource) -> Result<(), ExportError>;

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
    fn export(&self, batch: &[SpanData], resource: &Resource) -> Result<(), ExportError> {
        self.spans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(batch);
        *self.resource.lock().unwrap_or_else(|e| e.into_inner()) = Some(resource.clone());
        Ok(())
    }
}
