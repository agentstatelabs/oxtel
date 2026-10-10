//! The `console` exporter: one readable line per span, log record or metric
//! series, for development.
//!
//! ```text
//! 2026-10-10T03:20:00.123456Z span "GET /items" kind=server status=ok duration=1.234ms trace_id=… span_id=… http.route=/items
//! 2026-10-10T03:20:00.122001Z WARN shop::cache: slow lookup key=items trace_id=… span_id=…
//! 2026-10-10T03:20:05.000000Z metric jobs.done{queue=email} 5
//! ```
//!
//! The format is for people and may change; don't parse it. It writes to
//! standard output by default, so don't use it in a program whose standard
//! output carries a protocol, such as an MCP server over stdio.

use std::fmt::Write as _;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::export::ExportError;
use crate::logs::{LogExporter, LogRecord};
use crate::metrics::{MetricExporter, MetricFamily, PointValue};
use crate::resource::Resource;
use crate::trace::{SpanData, SpanExporter, SpanKind, Status};
use crate::value::{KeyValue, Value};

/// Writes spans, log records and metrics as text lines. Clones share the
/// output.
#[derive(Clone)]
pub struct ConsoleExporter {
    out: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Default for ConsoleExporter {
    fn default() -> Self {
        ConsoleExporter::stdout()
    }
}

impl ConsoleExporter {
    /// Write to standard output.
    pub fn stdout() -> Self {
        ConsoleExporter::to_writer(std::io::stdout())
    }

    /// Write to `out`.
    pub fn to_writer(out: impl Write + Send + 'static) -> Self {
        ConsoleExporter {
            out: Arc::new(Mutex::new(Box::new(out))),
        }
    }

    fn write(&self, text: &str) -> Result<(), ExportError> {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        out.write_all(text.as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| ExportError::Transport(e.to_string()))
    }
}

/// `t` as an RFC 3339 UTC timestamp with microseconds, such as
/// `2026-10-10T03:20:00.123456Z`.
pub fn rfc3339(t: SystemTime) -> String {
    let since = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        since.subsec_micros()
    )
}

/// A value as it appears after `key=`: quoted when it would otherwise be
/// ambiguous.
pub(crate) fn text_value(out: &mut String, v: &Value) {
    match v {
        Value::String(s)
            if s.is_empty() || s.contains(|c: char| c.is_whitespace() || c == '=' || c == '"') =>
        {
            let _ = write!(out, "{s:?}");
        }
        Value::String(s) => out.push_str(s),
        Value::Bool(b) => {
            let _ = write!(out, "{b}");
        }
        Value::I64(i) => {
            let _ = write!(out, "{i}");
        }
        Value::F64(f) => {
            let _ = write!(out, "{f}");
        }
        Value::Bytes(b) => {
            let _ = write!(out, "<{} bytes>", b.len());
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                text_value(out, item);
            }
            out.push(']');
        }
        Value::Map(kvs) => {
            out.push('{');
            for (i, kv) in kvs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let _ = write!(out, "{}=", kv.key);
                text_value(out, &kv.value);
            }
            out.push('}');
        }
    }
}

fn attributes(out: &mut String, kvs: &[KeyValue]) {
    for kv in kvs {
        let _ = write!(out, " {}=", kv.key);
        text_value(out, &kv.value);
    }
}

impl SpanExporter for ConsoleExporter {
    fn export(&self, batch: &[SpanData], _resource: &Resource) -> Result<(), ExportError> {
        let mut text = String::new();
        for span in batch {
            let kind = match span.kind {
                SpanKind::Internal => "internal",
                SpanKind::Server => "server",
                SpanKind::Client => "client",
                SpanKind::Producer => "producer",
                SpanKind::Consumer => "consumer",
            };
            let duration = span
                .end_time
                .duration_since(span.start_time)
                .unwrap_or_default();
            let _ = write!(
                text,
                "{} span {:?} kind={kind} status=",
                rfc3339(span.start_time),
                span.name
            );
            match &span.status {
                Status::Unset => text.push_str("unset"),
                Status::Ok => text.push_str("ok"),
                Status::Error { description } => {
                    let _ = write!(text, "error({description:?})");
                }
            }
            let _ = write!(
                text,
                " duration={:.3}ms trace_id={} span_id={}",
                duration.as_secs_f64() * 1000.0,
                span.context.trace_id,
                span.context.span_id
            );
            if let Some(parent) = span.parent_span_id {
                let _ = write!(text, " parent_id={parent}");
            }
            attributes(&mut text, &span.attributes);
            for event in &span.events {
                let _ = write!(text, " event={:?}", event.name);
            }
            text.push('\n');
        }
        self.write(&text)
    }
}

impl LogExporter for ConsoleExporter {
    fn export(&self, batch: &[LogRecord], _resource: &Resource) -> Result<(), ExportError> {
        let mut text = String::new();
        for record in batch {
            let _ = write!(
                text,
                "{} {} {}:",
                rfc3339(record.time),
                record.severity_text,
                record.scope.name
            );
            if let Some(name) = &record.event_name {
                let _ = write!(text, " [{name}]");
            }
            if let Some(body) = &record.body {
                text.push(' ');
                match body {
                    Value::String(s) => text.push_str(s),
                    other => text_value(&mut text, other),
                }
            }
            attributes(&mut text, &record.attributes);
            if let (Some(trace), Some(span)) = (record.trace_id, record.span_id) {
                let _ = write!(text, " trace_id={trace} span_id={span}");
            }
            text.push('\n');
        }
        self.write(&text)
    }
}

impl MetricExporter for ConsoleExporter {
    fn export(&self, metrics: &[MetricFamily], _resource: &Resource) -> Result<(), ExportError> {
        let mut text = String::new();
        for family in metrics {
            for point in &family.points {
                let _ = write!(text, "{} metric {}", rfc3339(family.time), family.name);
                if !point.attributes.is_empty() {
                    text.push('{');
                    for (i, (k, v)) in point.attributes.iter().enumerate() {
                        if i > 0 {
                            text.push(',');
                        }
                        let _ = write!(text, "{k}=");
                        text_value(&mut text, &Value::String(v.clone()));
                    }
                    text.push('}');
                }
                match &point.value {
                    PointValue::Counter(v) => {
                        let _ = write!(text, " {v}");
                    }
                    PointValue::Gauge(v) => {
                        let _ = write!(text, " {v}");
                    }
                    PointValue::Histogram(h) => {
                        let _ = write!(text, " count={} sum={}", h.count(), h.sum);
                        if let (Some(min), Some(max)) = (h.min, h.max) {
                            let _ = write!(text, " min={min} max={max}");
                        }
                    }
                }
                if let Some(unit) = &family.unit {
                    let _ = write!(text, " {unit}");
                }
                text.push('\n');
            }
        }
        self.write(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000000Z");
        let t = UNIX_EPOCH + Duration::from_micros(1_791_602_400_123_456);
        assert_eq!(rfc3339(t), "2026-10-10T03:20:00.123456Z");
        // A leap day.
        let t = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(rfc3339(t), "2024-02-29T00:00:00.000000Z");
    }

    #[test]
    fn ambiguous_strings_are_quoted() {
        let mut out = String::new();
        attributes(
            &mut out,
            &[
                KeyValue::new("plain", "items"),
                KeyValue::new("spaced", "two words"),
                KeyValue::new("empty", ""),
                KeyValue::new("n", 3i64),
            ],
        );
        assert_eq!(out, r#" plain=items spaced="two words" empty="" n=3"#);
    }
}
