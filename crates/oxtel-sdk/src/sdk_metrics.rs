//! oxtel's own metrics: what its pipelines did, named by the semantic
//! conventions for SDK self-observability (`otel.sdk.*`).
//!
//! They're off by default; [`set_enabled(true)`](set_enabled) turns them on
//! for the whole process. They're recorded through the `metrics` facade, so
//! they reach whichever recorder the program installed. An oxtel
//! [`MeterProvider`](crate::metrics::MeterProvider) exports them with
//! everything else. The conventions are still in development, so the names
//! may change.
//!
//! | Metric | What it counts |
//! |---|---|
//! | `otel.sdk.processor.span.processed`, `otel.sdk.processor.log.processed` | Items a batch processor handed to its exporter; with `error.type` = `queue_full` or `shutdown`, items it dropped |
//! | `otel.sdk.exporter.span.exported`, `otel.sdk.exporter.log.exported`, `otel.sdk.exporter.metric_data_point.exported` | Items exported; with `error.type`, items that failed or were rejected |
//! | `otel.sdk.exporter.operation.duration` | Seconds per export, retries included |
//! | `otel.sdk.metric_reader.collection.duration` | Seconds per collection of the metrics store |

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use metrics::{Label, SharedString, Unit};

use crate::export::ExportError;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turn oxtel's own metrics on or off for the whole process.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether oxtel's own metrics are on.
#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// One pipeline component, as oxtel's own metrics name it: its type, such
/// as `batching_span_processor`, and an instance name made from the type and
/// a number, such as `batching_span_processor/0`.
#[derive(Clone, Debug)]
pub struct Component {
    kind: &'static str,
    name: SharedString,
}

impl Component {
    /// The next instance of `kind`.
    pub fn new(kind: &'static str) -> Self {
        static COUNTS: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());
        let mut counts = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
        let n = match counts.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => {
                *n += 1;
                *n
            }
            None => {
                counts.push((kind, 0));
                0
            }
        };
        Component {
            kind,
            name: SharedString::from_shared(Arc::<str>::from(format!("{kind}/{n}"))),
        }
    }

    /// The type, the `otel.component.type` attribute.
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// The instance name, the `otel.component.name` attribute.
    pub fn name(&self) -> &str {
        &self.name
    }

    fn labels(&self) -> Vec<Label> {
        vec![
            Label::new("otel.component.type", self.kind),
            Label::new("otel.component.name", self.name.clone()),
        ]
    }
}

/// The `error.type` of a failed export: the HTTP status code when the
/// receiver refused it, otherwise `timeout`, `transport` or `shutdown`.
pub fn error_type(error: &ExportError) -> SharedString {
    match error {
        ExportError::Timeout => SharedString::const_str("timeout"),
        ExportError::Transport(_) => SharedString::const_str("transport"),
        ExportError::Rejected { status, .. } => SharedString::from_owned(status.to_string()),
        ExportError::Shutdown => SharedString::const_str("shutdown"),
    }
}

const OPERATION_DURATION: &str = "otel.sdk.exporter.operation.duration";
const COLLECTION_DURATION: &str = "otel.sdk.metric_reader.collection.duration";

fn describe() {
    for (name, description) in [
        (
            "otel.sdk.processor.span.processed",
            "The number of spans for which the processing has finished, either successful or failed.",
        ),
        (
            "otel.sdk.processor.log.processed",
            "The number of log records for which the processing has finished, either successful or failed.",
        ),
        (
            "otel.sdk.exporter.span.exported",
            "The number of spans for which the export has finished, either successful or failed.",
        ),
        (
            "otel.sdk.exporter.log.exported",
            "The number of log records for which the export has finished, either successful or failed.",
        ),
        (
            "otel.sdk.exporter.metric_data_point.exported",
            "The number of metric data points for which the export has finished, either successful or failed.",
        ),
    ] {
        metrics::describe_counter!(name, description);
    }
    metrics::describe_histogram!(
        OPERATION_DURATION,
        Unit::Seconds,
        "The duration of exporting a batch of telemetry records."
    );
    metrics::describe_histogram!(
        COLLECTION_DURATION,
        Unit::Seconds,
        "The duration of the collect operation of the metric reader."
    );
}

/// The metrics one exporter records: how many items it exported, and how
/// long each export took. For exporters outside this crate, such as OTLP's.
#[derive(Clone, Debug)]
pub struct ExporterMetrics {
    exported: &'static str,
    labels: Vec<Label>,
}

impl ExporterMetrics {
    /// Metrics for a new exporter of type `kind`, such as
    /// `otlp_http_span_exporter`, counting items in `exported`, such as
    /// `otel.sdk.exporter.span.exported`, and sending to `server`'s address
    /// and port, if it has one.
    pub fn new(kind: &'static str, exported: &'static str, server: Option<(&str, u16)>) -> Self {
        let mut labels = Component::new(kind).labels();
        if let Some((address, port)) = server {
            labels.push(Label::new("server.address", address.to_string()));
            labels.push(Label::new("server.port", port.to_string()));
        }
        ExporterMetrics { exported, labels }
    }

    /// Record one export of `items`, of which the receiver `rejected` some,
    /// with its `result`, `duration` and the last HTTP status, if any.
    pub fn record(
        &self,
        items: usize,
        rejected: usize,
        result: &Result<(), ExportError>,
        duration: Duration,
        http_status: Option<u16>,
    ) {
        if !enabled() {
            return;
        }
        describe();
        let mut labels = self.labels.clone();
        match result {
            Ok(()) => {
                let accepted = items.saturating_sub(rejected) as u64;
                metrics::counter!(self.exported, labels.clone()).increment(accepted);
                if rejected > 0 {
                    let mut rejected_labels = labels.clone();
                    rejected_labels.push(Label::new("error.type", "rejected"));
                    metrics::counter!(self.exported, rejected_labels).increment(rejected as u64);
                }
            }
            Err(e) => {
                labels.push(Label::new("error.type", error_type(e)));
                metrics::counter!(self.exported, labels.clone()).increment(items as u64);
            }
        }
        if let Some(status) = http_status {
            labels.push(Label::new("http.response.status_code", status.to_string()));
        }
        metrics::histogram!(OPERATION_DURATION, labels).record(duration.as_secs_f64());
    }
}

/// A batch processor's metrics.
pub(crate) struct ProcessorMetrics {
    processed: &'static str,
    labels: Vec<Label>,
}

impl ProcessorMetrics {
    pub(crate) fn new(kind: &'static str, processed: &'static str) -> Self {
        ProcessorMetrics {
            processed,
            labels: Component::new(kind).labels(),
        }
    }

    /// `n` items handed to the exporter.
    pub(crate) fn handed_over(&self, n: usize) {
        if enabled() {
            describe();
            metrics::counter!(self.processed, self.labels.clone()).increment(n as u64);
        }
    }

    /// One item dropped, for `reason`.
    #[cold]
    pub(crate) fn dropped(&self, reason: &'static str) {
        if enabled() {
            let mut labels = self.labels.clone();
            labels.push(Label::new("error.type", reason));
            metrics::counter!(self.processed, labels).increment(1);
        }
    }
}

/// A metric reader's metrics.
pub(crate) struct ReaderMetrics {
    labels: Vec<Label>,
}

impl ReaderMetrics {
    pub(crate) fn new(kind: &'static str) -> Self {
        ReaderMetrics {
            labels: Component::new(kind).labels(),
        }
    }

    pub(crate) fn collected(&self, duration: Duration) {
        if enabled() {
            describe();
            metrics::histogram!(COLLECTION_DURATION, self.labels.clone())
                .record(duration.as_secs_f64());
        }
    }
}
