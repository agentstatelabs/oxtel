//! A finished span, ready to export.

use std::borrow::Cow;
use std::time::SystemTime;

use crate::context::SpanContext;
use crate::id::SpanId;
use crate::value::KeyValue;

/// The span's role in the request.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SpanKind {
    /// Work inside a process.
    #[default]
    Internal,
    /// Handling an incoming request.
    Server,
    /// Making an outgoing request.
    Client,
    /// Sending a message.
    Producer,
    /// Receiving a message.
    Consumer,
}

impl SpanKind {
    /// Parse the `otel.kind` field's value.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "internal" => SpanKind::Internal,
            "server" => SpanKind::Server,
            "client" => SpanKind::Client,
            "producer" => SpanKind::Producer,
            "consumer" => SpanKind::Consumer,
            _ => return None,
        })
    }
}

/// How the operation ended.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Status {
    /// Not set; the default.
    #[default]
    Unset,
    /// Explicitly successful.
    Ok,
    /// Failed.
    Error {
        /// Why.
        description: String,
    },
}

/// Something that happened during a span.
#[derive(Clone, PartialEq, Debug)]
pub struct Event {
    /// The event's name.
    pub name: Cow<'static, str>,
    /// When it happened.
    pub time: SystemTime,
    /// Its attributes.
    pub attributes: Vec<KeyValue>,
}

/// A causal link to another span.
#[derive(Clone, PartialEq, Debug)]
pub struct Link {
    /// The linked span.
    pub context: SpanContext,
    /// Attributes describing the link.
    pub attributes: Vec<KeyValue>,
}

/// The instrumentation scope that produced a span. For `tracing` spans,
/// this is the crate at the root of the span's target.
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub struct Scope {
    /// Its name.
    pub name: Cow<'static, str>,
    /// Its version, if known.
    pub version: Option<Cow<'static, str>>,
}

/// A finished span.
#[derive(Clone, PartialEq, Debug)]
pub struct SpanData {
    /// The span's own context.
    pub context: SpanContext,
    /// The parent span's id, if any.
    pub parent_span_id: Option<SpanId>,
    /// Whether the parent came from another process.
    pub parent_is_remote: bool,
    /// The span's name.
    pub name: Cow<'static, str>,
    /// The span's kind.
    pub kind: SpanKind,
    /// When it started.
    pub start_time: SystemTime,
    /// When it ended.
    pub end_time: SystemTime,
    /// Its attributes.
    pub attributes: Vec<KeyValue>,
    /// Attributes dropped by the attribute limit.
    pub dropped_attributes: u32,
    /// Its events.
    pub events: Vec<Event>,
    /// Events dropped by the event limit.
    pub dropped_events: u32,
    /// Its links.
    pub links: Vec<Link>,
    /// Links dropped by the link limit.
    pub dropped_links: u32,
    /// How it ended.
    pub status: Status,
    /// What produced it.
    pub scope: Scope,
}
