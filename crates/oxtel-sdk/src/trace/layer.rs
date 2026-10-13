//! The `tracing` layer that turns spans into OpenTelemetry spans.

use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use tracing_core::field::{Field, Visit};
use tracing_core::span::{Attributes, Id, Record};
use tracing_core::{Event as TracingEvent, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::{LookupSpan, Registry};

use crate::context::{SpanContext, TraceFlags};
use crate::id::{SpanId, TraceId};
use crate::trace::data::{Event, Link, Scope, SpanData, SpanKind, Status};
use crate::trace::mapping::{EventMapping, MappedSpan};
use crate::trace::processor::SpanProcessor;
use crate::trace::sampler::Sampler;
use crate::value::{KeyValue, Value, int_value, upsert};

/// How many attributes, events and links a span keeps. More are dropped and
/// counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpanLimits {
    /// Attributes per span (128).
    pub max_attributes: usize,
    /// Events per span (128).
    pub max_events: usize,
    /// Links per span (128).
    pub max_links: usize,
}

impl Default for SpanLimits {
    fn default() -> Self {
        SpanLimits {
            max_attributes: 128,
            max_events: 128,
            max_links: 128,
        }
    }
}

impl SpanLimits {
    /// Defaults, overridden by `OTEL_SPAN_ATTRIBUTE_COUNT_LIMIT` (or
    /// `OTEL_ATTRIBUTE_COUNT_LIMIT`), `OTEL_SPAN_EVENT_COUNT_LIMIT` and
    /// `OTEL_SPAN_LINK_COUNT_LIMIT`.
    pub fn from_env() -> Self {
        let num = |name: &str| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
        };
        let d = SpanLimits::default();
        SpanLimits {
            max_attributes: num("OTEL_SPAN_ATTRIBUTE_COUNT_LIMIT")
                .or_else(|| num("OTEL_ATTRIBUTE_COUNT_LIMIT"))
                .unwrap_or(d.max_attributes),
            max_events: num("OTEL_SPAN_EVENT_COUNT_LIMIT").unwrap_or(d.max_events),
            max_links: num("OTEL_SPAN_LINK_COUNT_LIMIT").unwrap_or(d.max_links),
        }
    }
}

pub(crate) struct Shared {
    pub(crate) processor: Arc<dyn SpanProcessor>,
    /// Whether a processor takes finished spans. Without one, spans keep
    /// their ids, for log records and propagation, and record nothing else.
    pub(crate) records: bool,
    pub(crate) sampler: Sampler,
    pub(crate) limits: SpanLimits,
    pub(crate) span_events: bool,
    pub(crate) mappings: Vec<Arc<dyn EventMapping>>,
}

/// A [`Layer`] that records `tracing` spans as OpenTelemetry spans and hands
/// them to a processor when they close. Make one with
/// [`TracerProvider::layer`](crate::trace::TracerProvider::layer).
///
/// Span fields become attributes. These fields are read specially:
/// - `otel.name`: the span's name, instead of the callsite's name;
/// - `otel.kind`: `server`, `client`, `producer`, `consumer` or `internal`;
/// - `otel.status_code`: `ok` or `error`;
/// - `otel.status_description`: the error's description.
#[derive(Clone)]
pub struct TraceLayer {
    shared: Arc<Shared>,
}

impl TraceLayer {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        TraceLayer { shared }
    }
}

/// What the layer keeps in each span's extensions.
pub(crate) struct SpanState {
    pub(crate) context: SpanContext,
    parent: Option<SpanContext>,
    /// Present only when the span is sampled.
    recording: Option<Box<Recording>>,
}

struct Recording {
    name: Cow<'static, str>,
    kind: SpanKind,
    start: SystemTime,
    attributes: Vec<KeyValue>,
    dropped_attributes: u32,
    events: Vec<Event>,
    dropped_events: u32,
    links: Vec<Link>,
    dropped_links: u32,
    status: StatusField,
    scope: Scope,
}

#[derive(Default)]
struct StatusField {
    ok: bool,
    error: bool,
    description: Option<String>,
}

impl Recording {
    fn new(meta: &'static Metadata<'static>) -> Self {
        let target = meta.target();
        let scope = target.split("::").next().unwrap_or(target);
        Recording {
            name: Cow::Borrowed(meta.name()),
            kind: SpanKind::Internal,
            start: SystemTime::now(),
            attributes: Vec::new(),
            dropped_attributes: 0,
            events: Vec::new(),
            dropped_events: 0,
            links: Vec::new(),
            dropped_links: 0,
            status: StatusField::default(),
            scope: Scope {
                name: Cow::Borrowed(scope),
                version: None,
            },
        }
    }

    fn finish(self, state: SpanState) -> SpanData {
        let status = if self.status.error {
            Status::Error {
                description: self.status.description.unwrap_or_default(),
            }
        } else if self.status.ok {
            Status::Ok
        } else {
            Status::Unset
        };
        let parent_is_remote = state.parent.as_ref().is_some_and(|p| p.is_remote);
        SpanData {
            context: state.context,
            parent_span_id: state.parent.map(|p| p.span_id),
            parent_is_remote,
            name: self.name,
            kind: self.kind,
            start_time: self.start,
            end_time: SystemTime::now(),
            attributes: self.attributes,
            dropped_attributes: self.dropped_attributes,
            events: self.events,
            dropped_events: self.dropped_events,
            links: self.links,
            dropped_links: self.dropped_links,
            status,
            scope: self.scope,
        }
    }
}

/// Records span fields into a [`Recording`].
struct SpanFields<'a> {
    recording: &'a mut Recording,
    limit: usize,
}

impl SpanFields<'_> {
    fn attribute(&mut self, field: &Field, value: Value) {
        let kv = KeyValue::new(field.name(), value);
        if self.recording.attributes.len() >= self.limit
            && !self.recording.attributes.iter().any(|a| a.key == kv.key)
        {
            self.recording.dropped_attributes += 1;
            return;
        }
        upsert(&mut self.recording.attributes, kv);
    }

    fn special(&mut self, field: &Field, value: &str) -> bool {
        match field.name() {
            "otel.name" => self.recording.name = Cow::Owned(value.to_string()),
            "otel.kind" => {
                if let Some(kind) = SpanKind::parse(value) {
                    self.recording.kind = kind;
                }
            }
            "otel.status_code" => match value.to_ascii_lowercase().as_str() {
                "ok" => self.recording.status.ok = true,
                "error" => self.recording.status.error = true,
                _ => {}
            },
            "otel.status_description" | "otel.status_message" => {
                self.recording.status.description = Some(value.to_string())
            }
            _ => return false,
        }
        true
    }
}

impl Visit for SpanFields<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if !self.special(field, value) {
            self.attribute(field, Value::String(value.to_string()));
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.attribute(field, Value::Bool(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.attribute(field, Value::I64(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.attribute(field, int_value(i64::try_from(value).ok(), value));
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.attribute(field, int_value(i64::try_from(value).ok(), value));
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.attribute(field, int_value(i64::try_from(value).ok(), value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.attribute(field, Value::F64(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.attribute(field, Value::String(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let text = format!("{value:?}");
        if !self.special(field, &text) {
            self.attribute(field, Value::String(text));
        }
    }
}

/// Records an event's fields as a span event.
struct EventFields {
    name: Option<String>,
    attributes: Vec<KeyValue>,
}

impl Visit for EventFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.name = Some(value.to_string());
        } else {
            self.attributes.push(KeyValue::new(
                field.name(),
                Value::String(value.to_string()),
            ));
        }
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.attributes.push(KeyValue::new(field.name(), value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.attributes.push(KeyValue::new(field.name(), value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.attributes.push(KeyValue::new(
            field.name(),
            int_value(i64::try_from(value).ok(), value),
        ));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.attributes.push(KeyValue::new(field.name(), value));
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.attributes.push(KeyValue::new(
            field.name(),
            Value::String(value.to_string()),
        ));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let text = format!("{value:?}");
        if field.name() == "message" {
            self.name = Some(text);
        } else {
            self.attributes
                .push(KeyValue::new(field.name(), Value::String(text)));
        }
    }
}

thread_local! {
    static REMOTE_PARENT: RefCell<Option<SpanContext>> = const { RefCell::new(None) };
}

/// Makes `parent`, typically extracted from an incoming request's headers,
/// the parent of the next span created on this thread. The span then joins
/// the caller's trace and follows its sampling decision.
///
/// An explicit `parent:` in the span macro still wins. Create the span
/// while the guard is alive:
///
/// ```
/// # use oxtel_sdk::{propagation, trace};
/// # let headers = std::collections::HashMap::<String, String>::new();
/// let span = {
///     let _parent = propagation::extract_trace_context(&headers).map(trace::attach_remote_parent);
///     tracing::info_span!("handle request", otel.kind = "server")
/// };
/// ```
pub fn attach_remote_parent(parent: SpanContext) -> RemoteParentGuard {
    let previous = REMOTE_PARENT.with(|p| p.borrow_mut().replace(parent));
    RemoteParentGuard { previous }
}

/// Restores the previous remote parent when dropped.
#[must_use = "the remote parent applies only while the guard is alive"]
pub struct RemoteParentGuard {
    previous: Option<SpanContext>,
}

impl Drop for RemoteParentGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        REMOTE_PARENT.with(|p| *p.borrow_mut() = previous);
    }
}

fn take_remote_parent() -> Option<SpanContext> {
    REMOTE_PARENT.with(|p| p.borrow_mut().take())
}

/// The context of `span`, if this crate's layer is recording it in a
/// [`Registry`]-based subscriber. Unsampled spans have a context too, so
/// their ids can appear in logs and outgoing requests.
pub fn span_context(span: &tracing::Span) -> Option<SpanContext> {
    span.with_subscriber(|(id, dispatch)| {
        let registry = dispatch.downcast_ref::<Registry>()?;
        let data = registry.span(id)?;
        let extensions = data.extensions();
        extensions.get::<SpanState>().map(|s| s.context.clone())
    })
    .flatten()
}

/// The context of the current span, or of its nearest ancestor this crate's
/// layer records (a span filtered out of the trace layer has none). See
/// [`span_context`].
///
/// Cheap enough for hot paths, such as attaching exemplars to metrics: it
/// reads the dispatcher's current span without creating a `Span` handle.
pub fn current_span_context() -> Option<SpanContext> {
    tracing::dispatcher::get_default(|dispatch| {
        let current = dispatch.current_span();
        let id = current.id()?;
        let registry = dispatch.downcast_ref::<Registry>()?;
        registry.span(id)?.scope().find_map(|span| {
            span.extensions()
                .get::<SpanState>()
                .map(|s| s.context.clone())
        })
    })
}

fn context_of<S>(ctx: &Context<'_, S>, id: &Id) -> Option<SpanContext>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    ctx.span(id)?
        .extensions()
        .get::<SpanState>()
        .map(|s| s.context.clone())
}

impl TraceLayer {
    /// Record a span an [`EventMapping`] made, ending now.
    fn record_mapped(&self, mapped: MappedSpan, parent: Option<SpanContext>) {
        let parent = parent.filter(SpanContext::is_valid);
        let trace_id = parent.as_ref().map_or_else(TraceId::random, |p| p.trace_id);
        if !self.shared.sampler.should_sample(parent.as_ref(), trace_id) {
            return;
        }
        let flags = parent
            .as_ref()
            .map_or(TraceFlags::RANDOM, |p| p.flags)
            .with_sampled(true);
        let context = SpanContext {
            trace_id,
            span_id: SpanId::random(),
            flags,
            trace_state: parent
                .as_ref()
                .map(|p| p.trace_state.clone())
                .unwrap_or_default(),
            is_remote: false,
        };
        let end = SystemTime::now();
        let mut attributes = mapped.attributes;
        let limit = self.shared.limits.max_attributes;
        let dropped_attributes = attributes.len().saturating_sub(limit) as u32;
        attributes.truncate(limit);
        self.shared.processor.on_end(SpanData {
            context,
            parent_span_id: parent.as_ref().map(|p| p.span_id),
            parent_is_remote: parent.as_ref().is_some_and(|p| p.is_remote),
            name: mapped.name,
            kind: mapped.kind,
            start_time: end.checked_sub(mapped.duration).unwrap_or(end),
            end_time: end,
            attributes,
            dropped_attributes,
            events: Vec::new(),
            dropped_events: 0,
            links: Vec::new(),
            dropped_links: 0,
            status: mapped.status,
            scope: Scope {
                name: mapped.scope,
                version: None,
            },
        });
    }
}

impl<S> Layer<S> for TraceLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if crate::is_suppressed() {
            return;
        }
        let Some(span) = ctx.span(id) else { return };
        let parent = if attrs.is_root() {
            None
        } else if let Some(parent_id) = attrs.parent() {
            context_of(&ctx, parent_id)
        } else {
            take_remote_parent().or_else(|| {
                ctx.lookup_current()
                    .and_then(|p| p.extensions().get::<SpanState>().map(|s| s.context.clone()))
            })
        };
        let parent = parent.filter(SpanContext::is_valid);

        let trace_id = parent.as_ref().map_or_else(TraceId::random, |p| p.trace_id);
        let sampled = self.shared.sampler.should_sample(parent.as_ref(), trace_id);
        // A new trace id is random; a child keeps its parent's flags.
        let flags = parent
            .as_ref()
            .map_or(TraceFlags::RANDOM, |p| p.flags)
            .with_sampled(sampled);
        let context = SpanContext {
            trace_id,
            span_id: SpanId::random(),
            flags,
            trace_state: parent
                .as_ref()
                .map(|p| p.trace_state.clone())
                .unwrap_or_default(),
            is_remote: false,
        };
        let recording = (sampled && self.shared.records).then(|| {
            let mut recording = Box::new(Recording::new(attrs.metadata()));
            attrs.record(&mut SpanFields {
                recording: &mut recording,
                limit: self.shared.limits.max_attributes,
            });
            recording
        });
        span.extensions_mut().insert(SpanState {
            context,
            parent,
            recording,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();
        if let Some(recording) = extensions
            .get_mut::<SpanState>()
            .and_then(|s| s.recording.as_deref_mut())
        {
            values.record(&mut SpanFields {
                recording,
                limit: self.shared.limits.max_attributes,
            });
        }
    }

    fn on_follows_from(&self, id: &Id, follows: &Id, ctx: Context<'_, S>) {
        let Some(linked) = context_of(&ctx, follows) else {
            return;
        };
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();
        if let Some(recording) = extensions
            .get_mut::<SpanState>()
            .and_then(|s| s.recording.as_deref_mut())
        {
            if recording.links.len() >= self.shared.limits.max_links {
                recording.dropped_links += 1;
            } else {
                recording.links.push(Link {
                    context: linked,
                    attributes: Vec::new(),
                });
            }
        }
    }

    fn on_event(&self, event: &TracingEvent<'_>, ctx: Context<'_, S>) {
        if crate::is_suppressed() {
            return;
        }
        if !self.shared.mappings.is_empty() {
            let span = ctx.event_span(event);
            let metadata = span.as_ref().map(|s| s.metadata());
            if let Some(mapped) = self
                .shared
                .mappings
                .iter()
                .find_map(|m| m.map(event, metadata))
            {
                // The nearest span this layer records is the parent.
                let parent = span.and_then(|s| {
                    s.scope()
                        .find_map(|s| s.extensions().get::<SpanState>().map(|s| s.context.clone()))
                });
                self.record_mapped(mapped, parent);
                return;
            }
        }
        if !self.shared.span_events {
            return;
        }
        let Some(span) = ctx.event_span(event) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        let Some(recording) = extensions
            .get_mut::<SpanState>()
            .and_then(|s| s.recording.as_deref_mut())
        else {
            return;
        };
        if recording.events.len() >= self.shared.limits.max_events {
            recording.dropped_events += 1;
            return;
        }
        let meta = event.metadata();
        let mut fields = EventFields {
            name: None,
            attributes: vec![KeyValue::new("level", meta.level().as_str())],
        };
        event.record(&mut fields);
        recording.events.push(Event {
            name: fields.name.map_or(Cow::Borrowed(meta.name()), Cow::Owned),
            time: SystemTime::now(),
            attributes: fields.attributes,
        });
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(mut state) = span.extensions_mut().remove::<SpanState>() else {
            return;
        };
        if let Some(recording) = state.recording.take() {
            self.shared.processor.on_end(recording.finish(state));
        }
    }
}
