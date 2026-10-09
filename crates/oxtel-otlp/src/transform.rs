//! oxtel's data model to OTLP's.

use std::time::{SystemTime, UNIX_EPOCH};

use oxtel_sdk::trace::{Link, Scope, SpanData, SpanKind, Status};
use oxtel_sdk::{KeyValue, Resource, Value};

use crate::proto::collector::trace::v1::ExportTraceServiceRequest;
use crate::proto::common::v1 as common;
use crate::proto::resource::v1 as resource;
use crate::proto::trace::v1 as trace;

/// The OTLP request for a batch of spans: one resource, the spans grouped by
/// instrumentation scope, in the order the scopes first appear.
pub fn traces_request(batch: &[SpanData], res: &Resource) -> ExportTraceServiceRequest {
    let mut scopes: Vec<(&Scope, Vec<trace::Span>)> = Vec::new();
    for span in batch {
        let converted = span_to_proto(span);
        match scopes.iter_mut().find(|(s, _)| *s == &span.scope) {
            Some((_, spans)) => spans.push(converted),
            None => scopes.push((&span.scope, vec![converted])),
        }
    }
    ExportTraceServiceRequest {
        resource_spans: vec![trace::ResourceSpans {
            resource: Some(resource_to_proto(res)),
            scope_spans: scopes
                .into_iter()
                .map(|(scope, spans)| trace::ScopeSpans {
                    scope: Some(common::InstrumentationScope {
                        name: scope.name.to_string(),
                        version: scope.version.as_deref().unwrap_or_default().to_string(),
                        attributes: Vec::new(),
                        dropped_attributes_count: 0,
                    }),
                    spans,
                    schema_url: String::new(),
                })
                .collect(),
            schema_url: res.schema_url().unwrap_or_default().to_string(),
        }],
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
