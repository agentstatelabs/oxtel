//! Local log output: each `tracing` event as a line of text, flat JSON or
//! ECS JSON, on standard error or a writer of your choice.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use tracing_core::field::{Field, Visit};
use tracing_core::span::{Attributes, Id, Record};
use tracing_core::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::{LookupSpan, SpanRef};

use crate::console::{rfc3339, text_value};
use crate::id::{SpanId, TraceId};
use crate::resource::Resource;
use crate::trace::layer::SpanState;
use crate::value::{Value, int_value};

/// How a [`LocalLayer`] writes each event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalFormat {
    /// For people, like `tracing`'s own output:
    ///
    /// ```text
    /// 2026-10-10T03:20:00.123456Z  WARN request{http.route=/items}:load: shop::cache: slow lookup key=items trace_id=… span_id=…
    /// ```
    Text,
    /// One flat JSON object per line: `timestamp`, `level`, `target`,
    /// `message`, `trace_id` and `span_id`, then the fields of every
    /// enclosing span (outermost first) and of the event, under their names
    /// as written, such as `http.request.method`. An inner field wins over an
    /// outer one of the same name.
    Json,
    /// One JSON object per line in the Elastic Common Schema, as the
    /// ecs-logging specification lays out: `@timestamp`, `log.level`,
    /// `message`, `ecs.version`, then `log.logger`, `log.origin.*`,
    /// `trace.id`, `span.id`, the resource's `service.*`, and the span and
    /// event fields as in [`Json`](LocalFormat::Json).
    Ecs,
}

impl LocalFormat {
    /// The format named `text`, `json` or `ecs`, in any case.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "text" => Some(LocalFormat::Text),
            "json" => Some(LocalFormat::Json),
            "ecs" => Some(LocalFormat::Ecs),
            _ => None,
        }
    }
}

/// The ECS version the [`Ecs`](LocalFormat::Ecs) format follows.
pub const ECS_VERSION: &str = "8.11.0";

/// A [`Layer`] that writes each event locally, in a [`LocalFormat`]. Inside
/// a span recorded by a [`TraceLayer`](crate::trace::TraceLayer), lines
/// carry its trace and span ids.
///
/// Give it a filter for the events you want. As with the
/// [`LogLayer`](crate::logs::LogLayer), leave enabled the spans the trace
/// layer records, so lines find their ids.
pub struct LocalLayer {
    format: LocalFormat,
    out: Arc<Mutex<Box<dyn Write + Send>>>,
    service: Vec<(&'static str, Value)>,
}

impl LocalLayer {
    /// Write `format` to standard error.
    pub fn new(format: LocalFormat) -> Self {
        LocalLayer {
            format,
            out: Arc::new(Mutex::new(Box::new(std::io::stderr()))),
            service: Vec::new(),
        }
    }

    /// Write to `out` instead.
    pub fn with_writer(mut self, out: impl Write + Send + 'static) -> Self {
        self.out = Arc::new(Mutex::new(Box::new(out)));
        self
    }

    /// For [`Ecs`](LocalFormat::Ecs), add `service.name`, `service.version`
    /// and `service.environment` from `resource`.
    pub fn with_resource(mut self, resource: &Resource) -> Self {
        self.service.clear();
        for (ecs, otel) in [
            ("service.name", "service.name"),
            ("service.version", "service.version"),
            ("service.environment", "deployment.environment.name"),
        ] {
            if let Some(value) = resource.get(otel) {
                self.service.push((ecs, value.clone()));
            }
        }
        self
    }
}

/// A span's fields, kept for the lines written inside it. The `otel.*`
/// fields instruct the trace layer rather than describe the work, so
/// they're kept apart: `otel.name` names the span in text, and the rest
/// are left out.
#[derive(Default)]
struct SpanFields {
    values: Vec<(&'static str, Value)>,
    /// `otel.name`, if set.
    name: Option<String>,
    /// `otel.kind` is set: the span comes from instrumentation such as
    /// oxtel-tower's, whose name says enough in text.
    instrumented: bool,
}

impl SpanFields {
    /// Take `values` in, sorting out the `otel.*` fields.
    fn absorb(&mut self, values: Vec<(&'static str, Value)>) {
        for (k, v) in values {
            match k {
                "otel.name" => {
                    self.name = Some(match v {
                        Value::String(s) => s,
                        other => {
                            let mut s = String::new();
                            text_value(&mut s, &other);
                            s
                        }
                    });
                }
                "otel.kind" => self.instrumented = true,
                _ if k.starts_with("otel.") => {}
                _ => self.values.push((k, v)),
            }
        }
    }
}

/// Records fields, a later value replacing an earlier one of the same name.
/// An event's `message` is kept apart.
#[derive(Default)]
struct Fields {
    message: Option<Value>,
    values: Vec<(&'static str, Value)>,
    take_message: bool,
}

impl Fields {
    fn put(&mut self, field: &Field, value: Value) {
        let name = field.name();
        if self.take_message && name == "message" {
            self.message = Some(value);
        } else if let Some(slot) = self.values.iter_mut().find(|(k, _)| *k == name) {
            slot.1 = value;
        } else {
            self.values.push((name, value));
        }
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, Value::String(value.to_string()));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, Value::Bool(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, Value::I64(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, int_value(i64::try_from(value).ok(), value));
    }
    fn record_i128(&mut self, field: &Field, value: i128) {
        self.put(field, int_value(i64::try_from(value).ok(), value));
    }
    fn record_u128(&mut self, field: &Field, value: u128) {
        self.put(field, int_value(i64::try_from(value).ok(), value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, Value::F64(value));
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, Value::String(value.to_string()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.put(field, Value::String(format!("{value:?}")));
    }
}

thread_local! {
    static LINE: RefCell<String> = const { RefCell::new(String::new()) };
}

impl<S> Layer<S> for LocalLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let mut span_fields = SpanFields::default();
        span_fields.absorb(fields.values);
        span.extensions_mut().insert(span_fields);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();
        if let Some(span_fields) = extensions.get_mut::<SpanFields>() {
            let mut fields = Fields {
                values: std::mem::take(&mut span_fields.values),
                ..Fields::default()
            };
            values.record(&mut fields);
            span_fields.absorb(fields.values);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields {
            take_message: true,
            ..Fields::default()
        };
        event.record(&mut fields);
        let scope: Vec<SpanRef<'_, S>> = ctx
            .event_scope(event)
            .map(|s| s.from_root().collect())
            .unwrap_or_default();
        let ids = scope.iter().rev().find_map(|span| {
            span.extensions()
                .get::<SpanState>()
                .map(|s| (s.context.trace_id, s.context.span_id))
        });
        let line = Line {
            time: SystemTime::now(),
            event,
            scope: &scope,
            fields,
            ids,
        };
        let write = |text: &mut String| {
            text.clear();
            match self.format {
                LocalFormat::Text => line.text(text),
                LocalFormat::Json => line.json(text),
                LocalFormat::Ecs => line.ecs(text, &self.service),
            }
            text.push('\n');
            let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
            let _ = out.write_all(text.as_bytes());
        };
        // A writer that itself logs would find the buffer in use.
        LINE.with(|buf| match buf.try_borrow_mut() {
            Ok(mut text) => write(&mut text),
            Err(_) => write(&mut String::new()),
        });
    }
}

/// One event, ready to format.
struct Line<'s, 'a, 'e, S: for<'l> LookupSpan<'l>> {
    time: SystemTime,
    event: &'s Event<'e>,
    scope: &'s [SpanRef<'a, S>],
    fields: Fields,
    ids: Option<(TraceId, SpanId)>,
}

impl<S: for<'l> LookupSpan<'l>> Line<'_, '_, '_, S> {
    fn text(&self, out: &mut String) {
        let meta = self.event.metadata();
        let _ = write!(out, "{} {:>5} ", rfc3339(self.time), meta.level().as_str());
        for span in self.scope {
            let extensions = span.extensions();
            let fields = extensions.get::<SpanFields>();
            out.push_str(
                fields
                    .and_then(|f| f.name.as_deref())
                    .unwrap_or(span.name()),
            );
            if let Some(fields) = fields
                && !fields.instrumented
                && !fields.values.is_empty()
            {
                out.push('{');
                for (i, (k, v)) in fields.values.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    let _ = write!(out, "{k}=");
                    text_value(out, v);
                }
                out.push('}');
            }
            out.push_str(": ");
        }
        let _ = write!(out, "{}:", meta.target());
        match &self.fields.message {
            Some(Value::String(m)) => {
                out.push(' ');
                out.push_str(m);
            }
            Some(other) => {
                out.push(' ');
                text_value(out, other);
            }
            None => {}
        }
        for (k, v) in &self.fields.values {
            let _ = write!(out, " {k}=");
            text_value(out, v);
        }
        if let Some((trace, span)) = self.ids {
            let _ = write!(out, " trace_id={trace} span_id={span}");
        }
    }

    fn json(&self, out: &mut String) {
        let meta = self.event.metadata();
        let mut obj = Object::new(out);
        obj.str("timestamp", &rfc3339(self.time));
        obj.str("level", meta.level().as_str());
        obj.str("target", meta.target());
        if let Some(message) = &self.fields.message {
            obj.value("message", message);
        }
        if let Some((trace, span)) = self.ids {
            obj.str("trace_id", &trace.to_string());
            obj.str("span_id", &span.to_string());
        }
        self.merged_fields(&mut obj);
        obj.close();
    }

    fn ecs(&self, out: &mut String, service: &[(&'static str, Value)]) {
        let meta = self.event.metadata();
        let mut obj = Object::new(out);
        obj.str("@timestamp", &rfc3339(self.time));
        obj.str("log.level", meta.level().as_str());
        match &self.fields.message {
            Some(Value::String(m)) => obj.str("message", m),
            Some(other) => {
                let mut text = String::new();
                text_value(&mut text, other);
                obj.str("message", &text);
            }
            None => obj.str("message", ""),
        }
        obj.str("ecs.version", ECS_VERSION);
        obj.str("log.logger", meta.target());
        if let Some(file) = meta.file() {
            obj.str("log.origin.file.name", file);
        }
        if let Some(line) = meta.line() {
            obj.value("log.origin.file.line", &Value::I64(i64::from(line)));
        }
        if let Some((trace, span)) = self.ids {
            obj.str("trace.id", &trace.to_string());
            obj.str("span.id", &span.to_string());
        }
        for (k, v) in service {
            obj.value(k, v);
        }
        self.merged_fields(&mut obj);
        obj.close();
    }

    /// The span fields, outermost first, then the event's; an inner field
    /// replaces an outer one. A field named like a key already written is
    /// prefixed with `fields.`, so lines never carry duplicate keys.
    fn merged_fields(&self, obj: &mut Object<'_>) {
        let mut merged: Vec<(&'static str, Value)> = Vec::new();
        let mut put = |k: &'static str, v: &Value| match merged.iter_mut().find(|(n, _)| *n == k) {
            Some(slot) => slot.1 = v.clone(),
            None => merged.push((k, v.clone())),
        };
        for span in self.scope {
            if let Some(fields) = span.extensions().get::<SpanFields>() {
                for (k, v) in &fields.values {
                    put(k, v);
                }
            }
        }
        for (k, v) in &self.fields.values {
            put(k, v);
        }
        for (k, v) in &merged {
            if obj.has(k) {
                obj.value(&format!("fields.{k}"), v);
            } else {
                obj.value(k, v);
            }
        }
    }
}

/// Writes one JSON object, remembering its keys.
struct Object<'a> {
    out: &'a mut String,
    keys: Vec<String>,
}

impl<'a> Object<'a> {
    fn new(out: &'a mut String) -> Self {
        out.push('{');
        Object {
            out,
            keys: Vec::new(),
        }
    }

    fn has(&self, key: &str) -> bool {
        self.keys.iter().any(|k| k == key)
    }

    fn key(&mut self, key: &str) {
        if !self.keys.is_empty() {
            self.out.push(',');
        }
        json_string(self.out, key);
        self.out.push(':');
        self.keys.push(key.to_string());
    }

    fn str(&mut self, key: &str, value: &str) {
        self.key(key);
        json_string(self.out, value);
    }

    fn value(&mut self, key: &str, value: &Value) {
        self.key(key);
        json_value(self.out, value);
    }

    fn close(self) {
        self.out.push('}');
    }
}

/// `s` as a JSON string.
fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `v` as JSON. Non-finite numbers, which JSON can't carry, become strings.
fn json_value(out: &mut String, v: &Value) {
    match v {
        Value::Bool(b) => {
            let _ = write!(out, "{b}");
        }
        Value::I64(i) => {
            let _ = write!(out, "{i}");
        }
        Value::F64(f) if f.is_finite() => {
            let _ = write!(out, "{f:?}");
        }
        Value::F64(f) => json_string(out, &f.to_string()),
        Value::String(s) => json_string(out, s),
        Value::Bytes(b) => {
            out.push('[');
            for (i, byte) in b.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let _ = write!(out, "{byte}");
            }
            out.push(']');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_value(out, item);
            }
            out.push(']');
        }
        Value::Map(kvs) => {
            out.push('{');
            for (i, kv) in kvs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_string(out, &kv.key);
                out.push(':');
                json_value(out, &kv.value);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strings_are_escaped() {
        let mut out = String::new();
        json_string(&mut out, "a\"b\\c\nd\u{1}é");
        assert_eq!(out, r#""a\"b\\c\nd\u0001é""#);
    }

    #[test]
    fn json_numbers_stay_numbers_when_they_can() {
        let mut out = String::new();
        for v in [
            Value::F64(0.25),
            Value::F64(1.0),
            Value::F64(f64::NAN),
            Value::I64(-3),
        ] {
            json_value(&mut out, &v);
            out.push(' ');
        }
        assert_eq!(out, r#"0.25 1.0 "NaN" -3 "#);
    }
}
