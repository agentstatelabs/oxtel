//! Spans made from a library's events: for libraries that report an
//! operation once it's done, with how long it took, rather than wrapping it
//! in a span.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use tracing_core::{Event, Level, Metadata};

use crate::trace::data::{SpanKind, Status};
use crate::value::KeyValue;

/// Recognises a library's events and records each as a span of its own,
/// after the fact. Give one to
/// [`TracerProviderBuilder::with_event_mapping`](crate::trace::TracerProviderBuilder::with_event_mapping).
///
/// The span is a child of the span the event happened in, ends when the
/// event is seen, and starts [`MappedSpan::duration`] earlier. It's sampled
/// like any other span. An event that's mapped isn't also kept as a span
/// event; one that isn't goes on as usual. Either way, other layers see the
/// event unchanged.
///
/// The trace layer's filter must let the events through: a library's
/// events are often at `debug`. [`targets`](EventMapping::targets) says
/// which, so that a setup such as oxtel's can let them through to the trace
/// layer alone.
pub trait EventMapping: Send + Sync + 'static {
    /// The span `event` stands for, if it's one this mapping knows. `parent`
    /// is the metadata of the span the event happened in, so a mapping can
    /// stay out of the way of instrumentation that already records the same
    /// operation.
    fn map(
        &self,
        event: &Event<'_>,
        parent: Option<&'static Metadata<'static>>,
    ) -> Option<MappedSpan>;

    /// The event targets this mapping reads, each with the most verbose
    /// level its events can come at. Default: none.
    fn targets(&self) -> &'static [(&'static str, Level)] {
        &[]
    }
}

impl<M: EventMapping + ?Sized> EventMapping for Arc<M> {
    fn map(
        &self,
        event: &Event<'_>,
        parent: Option<&'static Metadata<'static>>,
    ) -> Option<MappedSpan> {
        (**self).map(event, parent)
    }

    fn targets(&self) -> &'static [(&'static str, Level)] {
        (**self).targets()
    }
}

impl<M: EventMapping + ?Sized> EventMapping for Box<M> {
    fn map(
        &self,
        event: &Event<'_>,
        parent: Option<&'static Metadata<'static>>,
    ) -> Option<MappedSpan> {
        (**self).map(event, parent)
    }

    fn targets(&self) -> &'static [(&'static str, Level)] {
        (**self).targets()
    }
}

/// A span an [`EventMapping`] made from an event.
#[derive(Clone, Debug, PartialEq)]
pub struct MappedSpan {
    /// The span's name.
    pub name: Cow<'static, str>,
    /// Its kind.
    pub kind: SpanKind,
    /// How long the operation took: the span starts this long before the
    /// event.
    pub duration: Duration,
    /// Its attributes. The span limits apply.
    pub attributes: Vec<KeyValue>,
    /// How it ended.
    pub status: Status,
    /// The instrumentation scope's name, such as the library's crate.
    pub scope: Cow<'static, str>,
}
