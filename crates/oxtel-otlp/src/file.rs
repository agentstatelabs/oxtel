//! OTLP/JSON lines to standard output or a file: the specification's OTLP
//! file exporter.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use oxtel_sdk::Resource;
use oxtel_sdk::export::ExportError;
use oxtel_sdk::logs::{LogExporter, LogRecord};
use oxtel_sdk::metrics::{MetricExporter, MetricFamily};
use oxtel_sdk::trace::{SpanData, SpanExporter};

use crate::json;
use crate::transform::{logs_request, metrics_request, traces_request};

/// Writes each export as one line of OTLP/JSON, the encoding OTLP/HTTP
/// uses, to standard output or a file: the
/// [OTLP file exporter](https://opentelemetry.io/docs/specs/otel/protocol/file-exporter/)
/// of the specification. Each line is a whole export request, so the
/// Collector's `otlpjsonfile` receiver reads the lines back without loss.
///
/// ```no_run
/// use oxtel_otlp::OtlpFileExporter;
/// use oxtel_sdk::trace::TracerProvider;
///
/// let traces = TracerProvider::builder()
///     .with_batch_exporter(OtlpFileExporter::file("traces.jsonl")?)
///     .build();
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// Clones share the output, so lines from several pipelines never
/// interleave. Give each signal a file of its own: the `otlpjsonfile`
/// receiver reads a file as one signal.
#[derive(Clone, Debug)]
pub struct OtlpFileExporter {
    out: Arc<Output>,
}

#[derive(Debug)]
enum Output {
    Stdout,
    File { path: PathBuf, file: Mutex<File> },
}

impl OtlpFileExporter {
    /// Lines on standard output.
    pub fn stdout() -> OtlpFileExporter {
        OtlpFileExporter {
            out: Arc::new(Output::Stdout),
        }
    }

    /// Lines appended to the file at `path`, which is created if it doesn't
    /// exist. Fails if it can't be opened for writing.
    pub fn file(path: impl AsRef<Path>) -> io::Result<OtlpFileExporter> {
        let path = path.as_ref();
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(OtlpFileExporter {
            out: Arc::new(Output::File {
                path: path.to_path_buf(),
                file: Mutex::new(file),
            }),
        })
    }

    /// The exporter a declarative configuration's `output_stream` names:
    /// `stdout`, or `file://` and a path, such as
    /// `file:///var/log/traces.jsonl`.
    pub fn from_output_stream(stream: &str) -> Result<OtlpFileExporter, String> {
        if stream == "stdout" {
            return Ok(OtlpFileExporter::stdout());
        }
        let Some(path) = stream.strip_prefix("file://") else {
            return Err(format!(
                "`{stream}` isn't `stdout` or a `file://` path, such as file:///var/log/traces.jsonl"
            ));
        };
        OtlpFileExporter::file(path).map_err(|e| format!("can't write to {path}: {e}"))
    }

    fn write_line(&self, mut line: String) -> Result<(), ExportError> {
        line.push('\n');
        let written = match &*self.out {
            // One write, under stdout's lock, so lines stay whole.
            Output::Stdout => {
                let mut out = io::stdout().lock();
                out.write_all(line.as_bytes()).and_then(|()| out.flush())
            }
            Output::File { file, .. } => {
                let mut file = file.lock().unwrap_or_else(|e| e.into_inner());
                file.write_all(line.as_bytes())
            }
        };
        written.map_err(|e| ExportError::Transport(e.to_string()))
    }

    fn describe(&self) -> String {
        match &*self.out {
            Output::Stdout => "standard output (OTLP/JSON lines)".into(),
            Output::File { path, .. } => format!("{} (OTLP/JSON lines)", path.display()),
        }
    }
}

impl SpanExporter for OtlpFileExporter {
    fn export(&self, batch: &[SpanData], resource: &Resource) -> Result<(), ExportError> {
        if batch.is_empty() {
            return Ok(());
        }
        self.write_line(json::traces(&traces_request(batch, resource)))
    }

    fn destination(&self) -> String {
        self.describe()
    }
}

impl LogExporter for OtlpFileExporter {
    fn export(&self, batch: &[LogRecord], resource: &Resource) -> Result<(), ExportError> {
        if batch.is_empty() {
            return Ok(());
        }
        self.write_line(json::logs(&logs_request(batch, resource)))
    }

    fn destination(&self) -> String {
        self.describe()
    }
}

impl MetricExporter for OtlpFileExporter {
    fn export(&self, metrics: &[MetricFamily], resource: &Resource) -> Result<(), ExportError> {
        if metrics.iter().all(|m| m.points.is_empty()) {
            return Ok(());
        }
        self.write_line(json::metrics(&metrics_request(metrics, resource)))
    }

    fn destination(&self) -> String {
        self.describe()
    }
}
