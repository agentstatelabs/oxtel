//! Spans made after the fact from a library's events.

use std::borrow::Cow;
use std::time::Duration;

use oxtel_sdk::trace::{
    EventMapping, InMemorySpanExporter, MappedSpan, Sampler, SpanKind, Status, TracerProvider,
};
use oxtel_sdk::{KeyValue, Resource};
use tracing_core::field::{Field, Visit};
use tracing_core::{Event, Metadata};
use tracing_subscriber::layer::SubscriberExt;

/// Maps `lib::op` events, which say how long an operation took.
struct Op;

#[derive(Default)]
struct Elapsed(Option<f64>);

impl Visit for Elapsed {
    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_secs" {
            self.0 = Some(value);
        }
    }
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
}

impl EventMapping for Op {
    fn map(
        &self,
        event: &Event<'_>,
        parent: Option<&'static Metadata<'static>>,
    ) -> Option<MappedSpan> {
        if event.metadata().target() != "lib::op" {
            return None;
        }
        // Inside a span that already records the operation: leave it.
        if parent.is_some_and(|p| p.name() == "already instrumented") {
            return None;
        }
        let mut elapsed = Elapsed::default();
        event.record(&mut elapsed);
        Some(MappedSpan {
            name: Cow::Borrowed("op"),
            kind: SpanKind::Client,
            duration: Duration::from_secs_f64(elapsed.0?),
            attributes: vec![KeyValue::new("op.kind", "read")],
            status: Status::Unset,
            scope: Cow::Borrowed("lib"),
        })
    }
}

fn run(sampler: Sampler, work: impl FnOnce()) -> Vec<oxtel_sdk::trace::SpanData> {
    let spans = InMemorySpanExporter::default();
    let provider = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(sampler)
        .with_simple_exporter(spans.clone())
        .with_event_mapping(Op)
        .build();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(provider.layer()), work);
    spans.finished_spans()
}

#[test]
fn a_mapped_event_becomes_a_child_span_that_started_earlier() {
    let spans = run(Sampler::AlwaysOn, || {
        tracing::info_span!("request").in_scope(|| {
            tracing::debug!(target: "lib::op", elapsed_secs = 0.25, "op finished");
            tracing::info!("handled");
        });
    });
    let op = spans
        .iter()
        .find(|s| s.name == "op")
        .expect("a span for the event");
    let request = spans.iter().find(|s| s.name == "request").unwrap();
    assert_eq!(op.parent_span_id, Some(request.context.span_id));
    assert_eq!(op.context.trace_id, request.context.trace_id);
    assert_eq!(op.kind, SpanKind::Client);
    assert_eq!(op.scope.name, "lib");
    assert_eq!(op.attributes, vec![KeyValue::new("op.kind", "read")]);
    let duration = op.end_time.duration_since(op.start_time).unwrap();
    assert_eq!(duration, Duration::from_millis(250));

    // The mapped event isn't kept as a span event too; the other one is.
    let events: Vec<&str> = request.events.iter().map(|e| e.name.as_ref()).collect();
    assert_eq!(events, ["handled"]);
}

#[test]
fn an_event_the_mapping_declines_goes_on_as_usual() {
    let spans = run(Sampler::AlwaysOn, || {
        tracing::info_span!("request").in_scope(|| {
            // No elapsed time: not this mapping's shape.
            tracing::debug!(target: "lib::op", "something else");
        });
    });
    assert!(spans.iter().all(|s| s.name != "op"));
    let request = spans.iter().find(|s| s.name == "request").unwrap();
    assert_eq!(request.events.len(), 1, "kept as a span event");
}

#[test]
fn a_mapping_can_see_the_span_it_would_be_inside() {
    let spans = run(Sampler::AlwaysOn, || {
        tracing::info_span!("already instrumented").in_scope(|| {
            tracing::debug!(target: "lib::op", elapsed_secs = 0.1);
        });
    });
    assert!(spans.iter().all(|s| s.name != "op"), "not counted twice");
}

#[test]
fn mapped_spans_follow_sampling() {
    let spans = run(Sampler::ParentBased(Box::new(Sampler::AlwaysOff)), || {
        tracing::debug!(target: "lib::op", elapsed_secs = 0.1);
    });
    assert!(spans.is_empty());

    // Without a parent span, a mapped span starts a trace of its own.
    let spans = run(Sampler::AlwaysOn, || {
        tracing::debug!(target: "lib::op", elapsed_secs = 0.1);
    });
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].parent_span_id, None);
}
