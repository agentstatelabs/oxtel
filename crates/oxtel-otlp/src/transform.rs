//! oxtel's data model to OTLP's.

use std::time::{SystemTime, UNIX_EPOCH};

use oxtel_sdk::logs::LogRecord;
use oxtel_sdk::metrics::{Exemplar, MetricFamily, OVERFLOW_ATTRIBUTE, Point, PointValue};
use oxtel_sdk::trace::{Link, Scope, SpanData, SpanKind, Status};
use oxtel_sdk::{KeyValue, Resource, Value};

use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::collector::metrics::v1::ExportMetricsServiceRequest;
use crate::proto::collector::trace::v1::ExportTraceServiceRequest;
use crate::proto::common::v1 as common;
use crate::proto::logs::v1 as logs;
use crate::proto::metrics::v1 as metrics;
use crate::proto::resource::v1 as resource;
use crate::proto::trace::v1 as trace;

/// The OTLP request for a batch of spans: one resource, the spans grouped by
/// instrumentation scope, in the order the scopes first appear.
pub fn traces_request(batch: &[SpanData], res: &Resource) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: vec![trace::ResourceSpans {
            resource: Some(resource_to_proto(res)),
            scope_spans: by_scope(batch, |s| &s.scope, span_to_proto)
                .into_iter()
                .map(|(scope, spans)| trace::ScopeSpans {
                    scope: Some(scope_to_proto(scope)),
                    spans,
                    schema_url: String::new(),
                })
                .collect(),
            schema_url: res.schema_url().unwrap_or_default().to_string(),
        }],
    }
}

/// The OTLP request for a batch of log records: one resource, the records
/// grouped by instrumentation scope, in the order the scopes first appear.
pub fn logs_request(batch: &[LogRecord], res: &Resource) -> ExportLogsServiceRequest {
    ExportLogsServiceRequest {
        resource_logs: vec![logs::ResourceLogs {
            resource: Some(resource_to_proto(res)),
            scope_logs: by_scope(batch, |r| &r.scope, log_to_proto)
                .into_iter()
                .map(|(scope, log_records)| logs::ScopeLogs {
                    scope: Some(scope_to_proto(scope)),
                    log_records,
                    schema_url: String::new(),
                })
                .collect(),
            schema_url: res.schema_url().unwrap_or_default().to_string(),
        }],
    }
}

/// The OTLP request for one collection of metrics: one resource, the
/// metrics grouped by instrumentation scope. Values are cumulative since
/// each family's start time. Families without series are left out.
pub fn metrics_request(families: &[MetricFamily], res: &Resource) -> ExportMetricsServiceRequest {
    let families: Vec<&MetricFamily> = families.iter().filter(|f| !f.points.is_empty()).collect();
    ExportMetricsServiceRequest {
        resource_metrics: vec![metrics::ResourceMetrics {
            resource: Some(resource_to_proto(res)),
            scope_metrics: by_scope(&families, |f| &f.scope, |f| metric_to_proto(f))
                .into_iter()
                .map(|(scope, metrics)| metrics::ScopeMetrics {
                    scope: Some(scope_to_proto(scope)),
                    metrics,
                    schema_url: String::new(),
                })
                .collect(),
            schema_url: res.schema_url().unwrap_or_default().to_string(),
        }],
    }
}

const CUMULATIVE: i32 = metrics::AggregationTemporality::Cumulative as i32;

fn metric_to_proto(family: &MetricFamily) -> metrics::Metric {
    use metrics::metric::Data;
    use metrics::number_data_point::Value as Number;
    let (start, time) = (unix_nanos(family.start_time), unix_nanos(family.time));
    let number = |point: &Point, value: Number, start: u64| metrics::NumberDataPoint {
        attributes: point_attributes(point),
        start_time_unix_nano: start,
        time_unix_nano: time,
        exemplars: point.exemplar.iter().map(exemplar_to_proto).collect(),
        flags: 0,
        value: Some(value),
    };
    let data = match family.points.first().map(|p| &p.value) {
        Some(PointValue::Counter(_)) => Data::Sum(metrics::Sum {
            data_points: family
                .points
                .iter()
                .filter_map(|p| match p.value {
                    // OTLP's integers are signed; a total beyond that is a double.
                    PointValue::Counter(v) => Some(number(
                        p,
                        i64::try_from(v).map_or(Number::AsDouble(v as f64), Number::AsInt),
                        start,
                    )),
                    _ => None,
                })
                .collect(),
            aggregation_temporality: CUMULATIVE,
            is_monotonic: true,
        }),
        Some(PointValue::Gauge(_)) => Data::Gauge(metrics::Gauge {
            data_points: family
                .points
                .iter()
                .filter_map(|p| match p.value {
                    PointValue::Gauge(v) => Some(number(p, Number::AsDouble(v), 0)),
                    _ => None,
                })
                .collect(),
        }),
        _ => Data::Histogram(metrics::Histogram {
            data_points: family
                .points
                .iter()
                .filter_map(|p| match &p.value {
                    PointValue::Histogram(h) => Some(metrics::HistogramDataPoint {
                        attributes: point_attributes(p),
                        start_time_unix_nano: start,
                        time_unix_nano: time,
                        count: h.count(),
                        sum: Some(h.sum),
                        bucket_counts: h.bucket_counts.clone(),
                        explicit_bounds: h.bounds.to_vec(),
                        exemplars: h
                            .exemplars
                            .iter()
                            .flatten()
                            .map(exemplar_to_proto)
                            .collect(),
                        flags: 0,
                        min: h.min,
                        max: h.max,
                    }),
                    _ => None,
                })
                .collect(),
            aggregation_temporality: CUMULATIVE,
        }),
    };
    metrics::Metric {
        name: family.name.clone(),
        description: family.description.clone().unwrap_or_default(),
        unit: family.unit.clone().unwrap_or_default(),
        metadata: Vec::new(),
        data: Some(data),
    }
}

/// A series' attributes. The `metrics` facade's labels are strings; the
/// overflow series' marker is the boolean the specification defines.
fn point_attributes(point: &Point) -> Vec<common::KeyValue> {
    point
        .attributes
        .iter()
        .map(|(k, v)| {
            let value = if k == OVERFLOW_ATTRIBUTE && v == "true" {
                Value::Bool(true)
            } else {
                Value::String(v.clone())
            };
            key_value(&KeyValue::new(k.clone(), value))
        })
        .collect()
}

fn exemplar_to_proto(e: &Exemplar) -> metrics::Exemplar {
    metrics::Exemplar {
        filtered_attributes: Vec::new(),
        time_unix_nano: unix_nanos(e.time),
        span_id: e.span_id.map(|s| s.to_bytes().to_vec()).unwrap_or_default(),
        trace_id: e
            .trace_id
            .map(|t| t.to_bytes().to_vec())
            .unwrap_or_default(),
        value: Some(metrics::exemplar::Value::AsDouble(e.value)),
    }
}

/// `items` converted and grouped by scope, scopes in order of first
/// appearance.
fn by_scope<'a, T, P>(
    items: &'a [T],
    scope: impl Fn(&'a T) -> &'a Scope,
    convert: impl Fn(&T) -> P,
) -> Vec<(&'a Scope, Vec<P>)> {
    let mut scopes: Vec<(&Scope, Vec<P>)> = Vec::new();
    for item in items {
        let s = scope(item);
        let converted = convert(item);
        match scopes.iter_mut().find(|(existing, _)| *existing == s) {
            Some((_, group)) => group.push(converted),
            None => scopes.push((s, vec![converted])),
        }
    }
    scopes
}

fn scope_to_proto(scope: &Scope) -> common::InstrumentationScope {
    common::InstrumentationScope {
        name: scope.name.to_string(),
        version: scope.version.as_deref().unwrap_or_default().to_string(),
        attributes: Vec::new(),
        dropped_attributes_count: 0,
    }
}

/// The resource as OTLP carries it.
pub fn resource_to_proto(res: &Resource) -> resource::Resource {
    resource::Resource {
        attributes: res.attributes().iter().map(key_value).collect(),
        dropped_attributes_count: 0,
        entity_refs: Vec::new(),
    }
}

const HAS_IS_REMOTE: u32 = trace::SpanFlags::ContextHasIsRemoteMask as u32;
const IS_REMOTE: u32 = trace::SpanFlags::ContextIsRemoteMask as u32;

/// W3C trace flags in the low byte; for a span with a parent (or a link),
/// whether that context is remote in bits 8 and 9.
fn flags(trace_flags: u8, remote: Option<bool>) -> u32 {
    let mut flags = u32::from(trace_flags);
    if let Some(remote) = remote {
        flags |= HAS_IS_REMOTE;
        if remote {
            flags |= IS_REMOTE;
        }
    }
    flags
}

fn span_to_proto(span: &SpanData) -> trace::Span {
    use trace::span::SpanKind as K;
    let (code, message) = match &span.status {
        Status::Unset => (trace::status::StatusCode::Unset, String::new()),
        Status::Ok => (trace::status::StatusCode::Ok, String::new()),
        Status::Error { description } => (trace::status::StatusCode::Error, description.clone()),
    };
    trace::Span {
        trace_id: span.context.trace_id.to_bytes().to_vec(),
        span_id: span.context.span_id.to_bytes().to_vec(),
        trace_state: span.context.trace_state.header_value(),
        parent_span_id: span
            .parent_span_id
            .map(|p| p.to_bytes().to_vec())
            .unwrap_or_default(),
        flags: flags(
            span.context.flags.to_u8(),
            span.parent_span_id.map(|_| span.parent_is_remote),
        ),
        name: span.name.to_string(),
        kind: match span.kind {
            SpanKind::Internal => K::Internal,
            SpanKind::Server => K::Server,
            SpanKind::Client => K::Client,
            SpanKind::Producer => K::Producer,
            SpanKind::Consumer => K::Consumer,
        } as i32,
        start_time_unix_nano: unix_nanos(span.start_time),
        end_time_unix_nano: unix_nanos(span.end_time),
        attributes: span.attributes.iter().map(key_value).collect(),
        dropped_attributes_count: span.dropped_attributes,
        events: span
            .events
            .iter()
            .map(|e| trace::span::Event {
                time_unix_nano: unix_nanos(e.time),
                name: e.name.to_string(),
                attributes: e.attributes.iter().map(key_value).collect(),
                dropped_attributes_count: 0,
            })
            .collect(),
        dropped_events_count: span.dropped_events,
        links: span.links.iter().map(link_to_proto).collect(),
        dropped_links_count: span.dropped_links,
        status: Some(trace::Status {
            message,
            code: code as i32,
        }),
    }
}

fn log_to_proto(record: &LogRecord) -> logs::LogRecord {
    logs::LogRecord {
        time_unix_nano: unix_nanos(record.time),
        observed_time_unix_nano: unix_nanos(record.observed_time),
        severity_number: i32::from(record.severity.number()),
        severity_text: record.severity_text.to_string(),
        body: record.body.as_ref().map(any_value),
        attributes: record.attributes.iter().map(key_value).collect(),
        dropped_attributes_count: record.dropped_attributes,
        flags: record
            .trace_id
            .map_or(0, |_| u32::from(record.trace_flags.to_u8())),
        trace_id: record
            .trace_id
            .map(|t| t.to_bytes().to_vec())
            .unwrap_or_default(),
        span_id: record
            .span_id
            .map(|s| s.to_bytes().to_vec())
            .unwrap_or_default(),
        event_name: record.event_name.as_deref().unwrap_or_default().to_string(),
    }
}

fn link_to_proto(link: &Link) -> trace::span::Link {
    trace::span::Link {
        trace_id: link.context.trace_id.to_bytes().to_vec(),
        span_id: link.context.span_id.to_bytes().to_vec(),
        trace_state: link.context.trace_state.header_value(),
        attributes: link.attributes.iter().map(key_value).collect(),
        dropped_attributes_count: 0,
        flags: flags(link.context.flags.to_u8(), Some(link.context.is_remote)),
    }
}

/// Nanoseconds since the Unix epoch; zero for times before it.
pub fn unix_nanos(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// An attribute as OTLP carries it.
pub fn key_value(kv: &KeyValue) -> common::KeyValue {
    common::KeyValue {
        key: kv.key.to_string(),
        value: Some(any_value(&kv.value)),
        key_strindex: 0,
    }
}

fn any_value(v: &Value) -> common::AnyValue {
    use common::any_value::Value as V;
    common::AnyValue {
        value: Some(match v {
            Value::Bool(b) => V::BoolValue(*b),
            Value::I64(i) => V::IntValue(*i),
            Value::F64(f) => V::DoubleValue(*f),
            Value::String(s) => V::StringValue(s.clone()),
            Value::Bytes(b) => V::BytesValue(b.clone()),
            Value::Array(values) => V::ArrayValue(common::ArrayValue {
                values: values.iter().map(any_value).collect(),
            }),
            Value::Map(kvs) => V::KvlistValue(common::KeyValueList {
                values: kvs.iter().map(key_value).collect(),
            }),
        }),
    }
}
