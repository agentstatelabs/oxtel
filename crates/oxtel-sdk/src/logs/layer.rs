//! The `tracing` layer that turns events into OpenTelemetry log records.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use tracing_core::field::{Field, Visit};
use tracing_core::{Event, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::context::TraceFlags;
use crate::logs::data::{LogRecord, Severity};
use crate::logs::processor::LogProcessor;
use crate::trace::Scope;
use crate::trace::layer::SpanState;
use crate::value::{Attributes, KeyValue, Value, int_value};

/// How many attributes a log record keeps. More are dropped and counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogLimits {
    /// Attributes per record (128).
    pub max_attributes: usize,
}

impl Default for LogLimits {
    fn default() -> Self {
        LogLimits {
            max_attributes: 128,
        }
    }
}

impl LogLimits {
    /// Defaults, overridden by `OTEL_LOGRECORD_ATTRIBUTE_COUNT_LIMIT` (or
    /// `OTEL_ATTRIBUTE_COUNT_LIMIT`).
    pub fn from_env() -> Self {
        let num = |name: &str| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
        };
        LogLimits {
            max_attributes: num("OTEL_LOGRECORD_ATTRIBUTE_COUNT_LIMIT")
                .or_else(|| num("OTEL_ATTRIBUTE_COUNT_LIMIT"))
                .unwrap_or(LogLimits::default().max_attributes),
        }
    }
}

pub(crate) struct Shared {
    pub(crate) processor: Arc<dyn LogProcessor>,
    pub(crate) limits: LogLimits,
    pub(crate) code_attributes: bool,
}

/// A [`Layer`] that turns each `tracing` event into an OpenTelemetry log
/// record and hands it to a processor. Make one with
/// [`LoggerProvider::layer`](crate::logs::LoggerProvider::layer).
///
/// - The `message` field becomes the body; other fields become attributes.
/// - The level becomes the severity.
/// - The target becomes the instrumentation scope, as a logger's name does
///   in other languages.
/// - A name given with `name:` in the macro becomes the event name.
/// - Inside a span recorded by a [`TraceLayer`](crate::trace::TraceLayer),
///   the record carries that span's trace and span ids, even when the span
///   isn't sampled.
/// - Events forwarded from the `log` crate by `tracing-log` keep their
///   original target.
///
/// If you give this layer a per-layer filter, filter events and leave
/// spans enabled: the layer finds trace context through the spans its
/// filter lets through.
#[derive(Clone)]
pub struct LogLayer {
    shared: Arc<Shared>,
}

impl LogLayer {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        LogLayer { shared }
    }
}

/// Records an event's fields into a log record's body and attributes.
struct LogFields {
    body: Option<Value>,
    attributes: Attributes,
    dropped: u32,
    limit: usize,
    /// The original target of an event forwarded from the `log` crate.
    log_target: Option<String>,
    code_attributes: bool,
}

impl LogFields {
    fn field(&mut self, field: &Field, value: Value) {
        let name = field.name();
        // Only `message` and the `log.*` fields are special; most names
        // aren't, and the first byte says so.
        if !matches!(name.as_bytes().first(), Some(b'm' | b'l')) {
            self.attribute(name, value);
            return;
        }
        match name {
            "message" => self.body = Some(value),
            // Fields `tracing-log` adds to events from the `log` crate.
            "log.target" => {
                if let Value::String(target) = value {
                    self.log_target = Some(target);
                }
            }
            "log.module_path" => {}
            "log.file" => self.code("code.file.path", value),
            "log.line" => self.code("code.line.number", value),
            name => self.attribute(name, value),
        }
    }

    fn code(&mut self, key: &'static str, value: Value) {
        if self.code_attributes {
            self.attribute(key, value);
        }
    }

    fn attribute(&mut self, key: &'static str, value: Value) {
        if self.attributes.len() >= self.limit {
            self.dropped += 1;
        } else {
            self.attributes.push(KeyValue::new(key, value));
        }
    }
}

impl Visit for LogFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.field(field, Value::String(value.to_string()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.field(field, Value::Bool(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.field(field, Value::I64(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.field(field, int_value(i64::try_from(value).ok(), value));
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.field(field, int_value(i64::try_from(value).ok(), value));
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.field(field, int_value(i64::try_from(value).ok(), value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.field(field, Value::F64(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.field(field, Value::String(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.field(field, Value::String(format!("{value:?}")));
    }
}

/// The event's own name, unless it's the one `tracing` generates from the
/// file and line (or `tracing-log`'s placeholder).
fn event_name(meta: &'static Metadata<'static>) -> Option<Cow<'static, str>> {
    let name = meta.name();
    let generated = name == "log event"
        || name
            .strip_prefix("event ")
            .is_some_and(|rest| meta.file().is_some_and(|file| rest.starts_with(file)));
    (!generated).then_some(Cow::Borrowed(name))
}

impl<S> Layer<S> for LogLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if crate::is_suppressed() {
            return;
        }
        let meta = event.metadata();
        let mut fields = LogFields {
            body: None,
            attributes: Attributes::new(),
            dropped: 0,
            limit: self.shared.limits.max_attributes,
            log_target: None,
            code_attributes: self.shared.code_attributes,
        };
        if self.shared.code_attributes {
            if let Some(file) = meta.file() {
                fields.attribute("code.file.path", Value::String(file.to_string()));
            }
            if let Some(line) = meta.line() {
                fields.attribute("code.line.number", Value::I64(i64::from(line)));
            }
        }
        event.record(&mut fields);

        let context = ctx.event_span(event).and_then(|span| {
            span.scope().find_map(|s| {
                s.extensions().get::<SpanState>().map(|state| {
                    (
                        state.context.trace_id,
                        state.context.span_id,
                        state.context.flags,
                    )
                })
            })
        });
        let level = *meta.level();
        let now = SystemTime::now();
        self.shared.processor.on_emit(LogRecord {
            time: now,
            observed_time: now,
            severity: Severity::from(level),
            severity_text: Cow::Borrowed(level.as_str()),
            body: fields.body,
            attributes: fields.attributes,
            dropped_attributes: fields.dropped,
            event_name: event_name(meta),
            trace_id: context.map(|c| c.0),
            span_id: context.map(|c| c.1),
            trace_flags: context.map_or(TraceFlags::default(), |c| c.2),
            scope: Scope {
                name: fields
                    .log_target
                    .map_or(Cow::Borrowed(meta.target()), Cow::Owned),
                version: None,
            },
        });
    }
}
