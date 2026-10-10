//! OTLP/JSON: the protocol messages as JSON, by the OTLP specification's
//! rules for the protobuf JSON mapping:
//!
//! - field names in lowerCamelCase;
//! - trace and span ids as hex strings, not base64;
//! - enums as integers;
//! - 64-bit integers as decimal strings;
//! - other bytes as base64;
//! - default values left out.

use std::fmt::Write as _;

use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::collector::metrics::v1::ExportMetricsServiceRequest;
use crate::proto::collector::trace::v1::ExportTraceServiceRequest;
use crate::proto::common::v1 as common;
use crate::proto::logs::v1 as logs;
use crate::proto::metrics::v1 as metrics;
use crate::proto::resource::v1 as resource;
use crate::proto::trace::v1 as trace;

/// A traces request as OTLP/JSON.
pub fn traces(request: &ExportTraceServiceRequest) -> String {
    let mut o = Obj::new();
    o.list("resourceSpans", &request.resource_spans, |o, rs| {
        o.opt_obj("resource", rs.resource.as_ref(), resource_obj);
        o.list("scopeSpans", &rs.scope_spans, |o, ss| {
            o.opt_obj("scope", ss.scope.as_ref(), scope_obj);
            o.list("spans", &ss.spans, span_obj);
            o.str("schemaUrl", &ss.schema_url);
        });
        o.str("schemaUrl", &rs.schema_url);
    });
    o.finish()
}

/// A logs request as OTLP/JSON.
pub fn logs(request: &ExportLogsServiceRequest) -> String {
    let mut o = Obj::new();
    o.list("resourceLogs", &request.resource_logs, |o, rl| {
        o.opt_obj("resource", rl.resource.as_ref(), resource_obj);
        o.list("scopeLogs", &rl.scope_logs, |o, sl| {
            o.opt_obj("scope", sl.scope.as_ref(), scope_obj);
            o.list("logRecords", &sl.log_records, log_obj);
            o.str("schemaUrl", &sl.schema_url);
        });
        o.str("schemaUrl", &rl.schema_url);
    });
    o.finish()
}

/// A metrics request as OTLP/JSON.
pub fn metrics(request: &ExportMetricsServiceRequest) -> String {
    let mut o = Obj::new();
    o.list("resourceMetrics", &request.resource_metrics, |o, rm| {
        o.opt_obj("resource", rm.resource.as_ref(), resource_obj);
        o.list("scopeMetrics", &rm.scope_metrics, |o, sm| {
            o.opt_obj("scope", sm.scope.as_ref(), scope_obj);
            o.list("metrics", &sm.metrics, metric_obj);
            o.str("schemaUrl", &sm.schema_url);
        });
        o.str("schemaUrl", &rm.schema_url);
    });
    o.finish()
}

fn resource_obj(o: &mut Obj, r: &resource::Resource) {
    o.list("attributes", &r.attributes, key_value_obj);
    o.u32("droppedAttributesCount", r.dropped_attributes_count);
}

fn scope_obj(o: &mut Obj, s: &common::InstrumentationScope) {
    o.str("name", &s.name);
    o.str("version", &s.version);
    o.list("attributes", &s.attributes, key_value_obj);
    o.u32("droppedAttributesCount", s.dropped_attributes_count);
}

fn span_obj(o: &mut Obj, s: &trace::Span) {
    o.hex("traceId", &s.trace_id);
    o.hex("spanId", &s.span_id);
    o.str("traceState", &s.trace_state);
    o.hex("parentSpanId", &s.parent_span_id);
    o.u32("flags", s.flags);
    o.str("name", &s.name);
    o.int("kind", i64::from(s.kind));
    o.u64("startTimeUnixNano", s.start_time_unix_nano);
    o.u64("endTimeUnixNano", s.end_time_unix_nano);
    o.list("attributes", &s.attributes, key_value_obj);
    o.u32("droppedAttributesCount", s.dropped_attributes_count);
    o.list("events", &s.events, |o, e| {
        o.u64("timeUnixNano", e.time_unix_nano);
        o.str("name", &e.name);
        o.list("attributes", &e.attributes, key_value_obj);
        o.u32("droppedAttributesCount", e.dropped_attributes_count);
    });
    o.u32("droppedEventsCount", s.dropped_events_count);
    o.list("links", &s.links, |o, l| {
        o.hex("traceId", &l.trace_id);
        o.hex("spanId", &l.span_id);
        o.str("traceState", &l.trace_state);
        o.list("attributes", &l.attributes, key_value_obj);
        o.u32("droppedAttributesCount", l.dropped_attributes_count);
        o.u32("flags", l.flags);
    });
    o.u32("droppedLinksCount", s.dropped_links_count);
    o.opt_obj("status", s.status.as_ref(), |o, st| {
        o.str("message", &st.message);
        o.int("code", i64::from(st.code));
    });
}

fn log_obj(o: &mut Obj, r: &logs::LogRecord) {
    o.u64("timeUnixNano", r.time_unix_nano);
    o.u64("observedTimeUnixNano", r.observed_time_unix_nano);
    o.int("severityNumber", i64::from(r.severity_number));
    o.str("severityText", &r.severity_text);
    o.opt_obj("body", r.body.as_ref(), any_value_obj);
    o.list("attributes", &r.attributes, key_value_obj);
    o.u32("droppedAttributesCount", r.dropped_attributes_count);
    o.u32("flags", r.flags);
    o.hex("traceId", &r.trace_id);
    o.hex("spanId", &r.span_id);
    o.str("eventName", &r.event_name);
}

fn metric_obj(o: &mut Obj, m: &metrics::Metric) {
    use metrics::metric::Data;
    o.str("name", &m.name);
    o.str("description", &m.description);
    o.str("unit", &m.unit);
    match &m.data {
        Some(Data::Gauge(g)) => o.obj("gauge", |o| {
            o.list("dataPoints", &g.data_points, number_point_obj);
        }),
        Some(Data::Sum(s)) => o.obj("sum", |o| {
            o.list("dataPoints", &s.data_points, number_point_obj);
            o.int(
                "aggregationTemporality",
                i64::from(s.aggregation_temporality),
            );
            o.bool("isMonotonic", s.is_monotonic);
        }),
        Some(Data::Histogram(h)) => o.obj("histogram", |o| {
            o.list("dataPoints", &h.data_points, histogram_point_obj);
            o.int(
                "aggregationTemporality",
                i64::from(h.aggregation_temporality),
            );
        }),
        // oxtel doesn't produce the other kinds.
        _ => {}
    }
    o.list("metadata", &m.metadata, key_value_obj);
}

fn number_point_obj(o: &mut Obj, p: &metrics::NumberDataPoint) {
    use metrics::number_data_point::Value;
    o.list("attributes", &p.attributes, key_value_obj);
    o.u64("startTimeUnixNano", p.start_time_unix_nano);
    o.u64("timeUnixNano", p.time_unix_nano);
    match p.value {
        Some(Value::AsDouble(d)) => o.double("asDouble", d),
        Some(Value::AsInt(i)) => o.int_string("asInt", i),
        None => {}
    }
    o.list("exemplars", &p.exemplars, exemplar_obj);
    o.u32("flags", p.flags);
}

fn histogram_point_obj(o: &mut Obj, p: &metrics::HistogramDataPoint) {
    o.list("attributes", &p.attributes, key_value_obj);
    o.u64("startTimeUnixNano", p.start_time_unix_nano);
    o.u64("timeUnixNano", p.time_unix_nano);
    o.u64("count", p.count);
    if let Some(sum) = p.sum {
        o.double("sum", sum);
    }
    o.raw_list("bucketCounts", &p.bucket_counts, |out, n| {
        let _ = write!(out, "\"{n}\"");
    });
    o.raw_list("explicitBounds", &p.explicit_bounds, |out, b| {
        number(out, *b)
    });
    o.list("exemplars", &p.exemplars, exemplar_obj);
    o.u32("flags", p.flags);
    if let Some(min) = p.min {
        o.double("min", min);
    }
    if let Some(max) = p.max {
        o.double("max", max);
    }
}

fn exemplar_obj(o: &mut Obj, e: &metrics::Exemplar) {
    use metrics::exemplar::Value;
    o.list("filteredAttributes", &e.filtered_attributes, key_value_obj);
    o.u64("timeUnixNano", e.time_unix_nano);
    match e.value {
        Some(Value::AsDouble(d)) => o.double("asDouble", d),
        Some(Value::AsInt(i)) => o.int_string("asInt", i),
        None => {}
    }
    o.hex("spanId", &e.span_id);
    o.hex("traceId", &e.trace_id);
}

fn key_value_obj(o: &mut Obj, kv: &common::KeyValue) {
    o.str("key", &kv.key);
    o.opt_obj("value", kv.value.as_ref(), any_value_obj);
}

fn any_value_obj(o: &mut Obj, v: &common::AnyValue) {
    use common::any_value::Value;
    match &v.value {
        // An empty string is a value, so it's written even though it's the
        // default.
        Some(Value::StringValue(s)) => {
            o.key("stringValue");
            string(&mut o.out, s);
        }
        Some(Value::BoolValue(b)) => {
            o.key("boolValue");
            let _ = write!(o.out, "{b}");
        }
        Some(Value::IntValue(i)) => {
            o.key("intValue");
            let _ = write!(o.out, "\"{i}\"");
        }
        Some(Value::DoubleValue(d)) => {
            o.key("doubleValue");
            number(&mut o.out, *d);
        }
        Some(Value::ArrayValue(a)) => o.obj("arrayValue", |o| {
            o.list("values", &a.values, any_value_obj);
        }),
        Some(Value::KvlistValue(kvs)) => o.obj("kvlistValue", |o| {
            o.list("values", &kvs.values, key_value_obj);
        }),
        Some(Value::BytesValue(b)) => {
            o.key("bytesValue");
            o.out.push('"');
            base64(&mut o.out, b);
            o.out.push('"');
        }
        Some(Value::StringValueStrindex(i)) => o.int("stringValueStrindex", i64::from(*i)),
        None => {}
    }
}

/// Writes one JSON object, leaving out fields with default values.
struct Obj {
    out: String,
    empty: bool,
}

impl Obj {
    fn new() -> Self {
        Obj {
            out: String::from("{"),
            empty: true,
        }
    }

    fn finish(mut self) -> String {
        self.out.push('}');
        self.out
    }

    fn key(&mut self, key: &str) {
        if !self.empty {
            self.out.push(',');
        }
        self.empty = false;
        self.out.push('"');
        self.out.push_str(key);
        self.out.push_str("\":");
    }

    fn str(&mut self, key: &str, value: &str) {
        if !value.is_empty() {
            self.key(key);
            string(&mut self.out, value);
        }
    }

    fn hex(&mut self, key: &str, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.key(key);
            self.out.push('"');
            for b in bytes {
                let _ = write!(self.out, "{b:02x}");
            }
            self.out.push('"');
        }
    }

    fn u32(&mut self, key: &str, value: u32) {
        if value != 0 {
            self.key(key);
            let _ = write!(self.out, "{value}");
        }
    }

    fn int(&mut self, key: &str, value: i64) {
        if value != 0 {
            self.key(key);
            let _ = write!(self.out, "{value}");
        }
    }

    /// A 64-bit integer, as a string.
    fn u64(&mut self, key: &str, value: u64) {
        if value != 0 {
            self.key(key);
            let _ = write!(self.out, "\"{value}\"");
        }
    }

    /// A 64-bit integer that's part of a oneof, so written even when zero.
    fn int_string(&mut self, key: &str, value: i64) {
        self.key(key);
        let _ = write!(self.out, "\"{value}\"");
    }

    /// A double that's optional or part of a oneof, so written even when
    /// zero.
    fn double(&mut self, key: &str, value: f64) {
        self.key(key);
        number(&mut self.out, value);
    }

    fn bool(&mut self, key: &str, value: bool) {
        if value {
            self.key(key);
            self.out.push_str("true");
        }
    }

    fn obj(&mut self, key: &str, fill: impl FnOnce(&mut Obj)) {
        self.key(key);
        let mut inner = Obj::new();
        fill(&mut inner);
        self.out.push_str(&inner.finish());
    }

    fn opt_obj<T>(&mut self, key: &str, value: Option<&T>, fill: impl FnOnce(&mut Obj, &T)) {
        if let Some(v) = value {
            self.obj(key, |o| fill(o, v));
        }
    }

    fn list<T>(&mut self, key: &str, items: &[T], mut each: impl FnMut(&mut Obj, &T)) {
        if items.is_empty() {
            return;
        }
        self.key(key);
        self.out.push('[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.out.push(',');
            }
            let mut inner = Obj::new();
            each(&mut inner, item);
            self.out.push_str(&inner.finish());
        }
        self.out.push(']');
    }

    fn raw_list<T>(&mut self, key: &str, items: &[T], mut each: impl FnMut(&mut String, &T)) {
        if items.is_empty() {
            return;
        }
        self.key(key);
        self.out.push('[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.out.push(',');
            }
            each(&mut self.out, item);
        }
        self.out.push(']');
    }
}

fn string(out: &mut String, s: &str) {
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

/// A double as JSON, with the protobuf mapping's strings for the values
/// JSON can't carry.
fn number(out: &mut String, d: f64) {
    if d.is_finite() {
        let _ = write!(out, "{d:?}");
    } else if d.is_nan() {
        out.push_str("\"NaN\"");
    } else if d > 0.0 {
        out.push_str("\"Infinity\"");
    } else {
        out.push_str("\"-Infinity\"");
    }
}

fn base64(out: &mut String, bytes: &[u8]) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
}

/// The rejected count in a JSON partial-success response, if any: the
/// value of `field` (such as `rejectedSpans`), as a number or a string.
#[cfg(feature = "http-proto")]
pub(crate) fn rejected(reply: &[u8], field: &str) -> Option<i64> {
    let text = std::str::from_utf8(reply).ok()?;
    let at = text.find(&format!("\"{field}\""))?;
    let rest = text[at + field.len() + 2..]
        .trim_start()
        .strip_prefix(':')?;
    let rest = rest.trim_start().trim_start_matches('"');
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet() {
        let cases: [(&[u8], &str); 5] = [
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"\xff\xfe\x00", "//4A"),
        ];
        for (input, expected) in cases {
            let mut out = String::new();
            base64(&mut out, input);
            assert_eq!(out, expected);
        }
    }

    #[test]
    fn non_finite_doubles_use_the_mappings_strings() {
        let mut out = String::new();
        for d in [0.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            number(&mut out, d);
            out.push(' ');
        }
        assert_eq!(out, r#"0.5 "NaN" "Infinity" "-Infinity" "#);
    }

    #[cfg(feature = "http-proto")]
    #[test]
    fn partial_success_counts_are_read_as_numbers_or_strings() {
        assert_eq!(
            rejected(
                br#"{"partialSuccess":{"rejectedSpans":"3"}}"#,
                "rejectedSpans"
            ),
            Some(3)
        );
        assert_eq!(
            rejected(
                br#"{"partialSuccess": {"rejectedLogRecords" : 2}}"#,
                "rejectedLogRecords"
            ),
            Some(2)
        );
        assert_eq!(rejected(b"{}", "rejectedSpans"), None);
    }
}
