//! The `tracing` layer: what it records, how it links spans, and how it
//! samples.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use oxtel_sdk::context::TraceFlags;
use oxtel_sdk::trace::{
    BatchConfig, ExportError, InMemorySpanExporter, Sampler, SpanData, SpanExporter, SpanKind,
    SpanLimits, Status, TracerProvider, attach_remote_parent, current_span_context, span_context,
};
use oxtel_sdk::{Resource, SpanContext, SpanId, TraceId, Value};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;

fn provider(sampler: Sampler) -> (TracerProvider, InMemorySpanExporter) {
    let exporter = InMemorySpanExporter::default();
    let provider = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(sampler)
        .with_simple_exporter(exporter.clone())
        .build();
    (provider, exporter)
}

fn with_layer<R>(provider: &TracerProvider, f: impl FnOnce() -> R) -> R {
    let subscriber = tracing_subscriber::registry().with(provider.layer());
    tracing::subscriber::with_default(subscriber, f)
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| &kv.value)
}

fn by_name<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
    spans
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no span {name}"))
}

#[test]
fn children_share_the_trace_and_name_their_parent() {
    let (provider, exporter) = provider(Sampler::AlwaysOn);
    with_layer(&provider, || {
        let parent = tracing::info_span!("parent");
        parent.in_scope(|| {
            tracing::info_span!("child").in_scope(|| {});
        });
        tracing::info_span!("other root").in_scope(|| {});
    });
    let spans = exporter.finished_spans();
    let (parent, child, other) = (
        by_name(&spans, "parent"),
        by_name(&spans, "child"),
        by_name(&spans, "other root"),
    );
    assert_eq!(child.context.trace_id, parent.context.trace_id);
    assert_eq!(child.parent_span_id, Some(parent.context.span_id));
    assert_eq!(parent.parent_span_id, None);
    assert_ne!(other.context.trace_id, parent.context.trace_id);
    assert!(parent.context.flags.is_random() && parent.context.is_sampled());
    assert!(child.start_time >= parent.start_time && child.end_time <= parent.end_time);
    assert_eq!(parent.scope.name, "trace_layer");
}

#[test]
fn fields_become_attributes_and_otel_fields_set_name_kind_and_status() {
    let (provider, exporter) = provider(Sampler::AlwaysOn);
    with_layer(&provider, || {
        let span = tracing::info_span!(
            "GET",
            otel.name = "GET /api/state/{ref_name}",
            otel.kind = "server",
            http.request.method = "GET",
            http.response.status_code = tracing::field::Empty,
            big = u64::MAX,
            ratio = 0.5,
            cached = true,
            debugged = ?vec![1, 2],
            otel.status_code = tracing::field::Empty,
            otel.status_description = tracing::field::Empty,
        );
        span.record("http.response.status_code", 500);
        span.record("otel.status_code", "error");
        span.record("otel.status_description", "store unavailable");
        span.in_scope(|| {});
    });
    let span = &exporter.finished_spans()[0];
    assert_eq!(span.name, "GET /api/state/{ref_name}");
    assert_eq!(span.kind, SpanKind::Server);
    assert_eq!(
        span.status,
        Status::Error {
            description: "store unavailable".into()
        }
    );
    assert_eq!(
        attr(span, "http.request.method"),
        Some(&Value::String("GET".into()))
    );
    assert_eq!(
        attr(span, "http.response.status_code"),
        Some(&Value::I64(500))
    );
    assert_eq!(
        attr(span, "big"),
        Some(&Value::String(u64::MAX.to_string()))
    );
    assert_eq!(attr(span, "ratio"), Some(&Value::F64(0.5)));
    assert_eq!(attr(span, "cached"), Some(&Value::Bool(true)));
    assert_eq!(
        attr(span, "debugged"),
        Some(&Value::String("[1, 2]".into()))
    );
    assert!(attr(span, "otel.name").is_none() && attr(span, "otel.kind").is_none());
}

#[test]
fn events_inside_a_span_become_span_events_and_follows_from_becomes_a_link() {
    let (provider, exporter) = provider(Sampler::AlwaysOn);
    with_layer(&provider, || {
        let cause = tracing::info_span!("cause");
        cause.in_scope(|| {});
        let span = tracing::info_span!("work");
        span.follows_from(&cause);
        span.in_scope(|| {
            tracing::warn!(retries = 3, "write conflict");
        });
        tracing::info!("outside any span");
    });
    let spans = exporter.finished_spans();
    let (cause, work) = (by_name(&spans, "cause"), by_name(&spans, "work"));
    assert_eq!(work.events.len(), 1);
    let event = &work.events[0];
    assert_eq!(event.name, "write conflict");
    assert!(
        event
            .attributes
            .iter()
            .any(|kv| kv.key == "retries" && kv.value == Value::I64(3))
    );
    assert!(
        event
            .attributes
            .iter()
            .any(|kv| kv.key == "level" && kv.value == Value::String("WARN".into()))
    );
    assert_eq!(work.links.len(), 1);
    assert_eq!(work.links[0].context.span_id, cause.context.span_id);
}

#[test]
fn unsampled_spans_keep_ids_but_export_nothing() {
    let (provider, exporter) = provider(Sampler::AlwaysOff);
    let (outer, inner) = with_layer(&provider, || {
        let span = tracing::info_span!("quiet");
        let outer = span_context(&span).expect("a context even when unsampled");
        let inner =
            span.in_scope(|| tracing::info_span!("quiet child").in_scope(current_span_context));
        (outer, inner.expect("child context"))
    });
    assert!(outer.is_valid() && !outer.is_sampled());
    assert_eq!(inner.trace_id, outer.trace_id);
    assert!(exporter.finished_spans().is_empty());
}

/// With no exporter there's nothing to record a span for, but log records
/// and propagation still need its ids and its sampling decision.
#[test]
fn without_an_exporter_spans_keep_ids_and_their_sampling_decision() {
    let provider = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .build();
    let (outer, inner) = with_layer(&provider, || {
        let span = tracing::info_span!("unexported", field = 1);
        span.record("field", 2);
        let outer = span_context(&span).expect("a context without an exporter");
        let inner = span.in_scope(|| {
            tracing::info!("an event inside");
            tracing::info_span!("child").in_scope(current_span_context)
        });
        (outer, inner.expect("child context"))
    });
    assert!(outer.is_valid() && outer.is_sampled());
    assert_eq!(inner.trace_id, outer.trace_id);
    assert!(inner.is_sampled());
    assert_eq!(provider.stats().exported, 0);
}

#[test]
fn a_remote_parent_joins_the_callers_trace_and_follows_its_decision() {
    let (provider, exporter) = provider(Sampler::default());
    let remote = |sampled| SpanContext {
        trace_id: TraceId::random(),
        span_id: SpanId::random(),
        flags: TraceFlags::default().with_sampled(sampled),
        is_remote: true,
        ..Default::default()
    };
    let (sampled, unsampled) = (remote(true), remote(false));
    with_layer(&provider, || {
        let request = {
            let _parent = attach_remote_parent(sampled.clone());
            tracing::info_span!("request", otel.kind = "server")
        };
        // The attached parent applies to the next span only; this child's
        // parent is the request span.
        request.in_scope(|| tracing::info_span!("handler").in_scope(|| {}));
        drop(request);
        {
            let _parent = attach_remote_parent(unsampled.clone());
            tracing::info_span!("not sampled upstream").in_scope(|| {});
        }
        // An explicit parent wins over an attached one.
        let local = tracing::info_span!("local root");
        {
            let _parent = attach_remote_parent(sampled.clone());
            tracing::info_span!(parent: &local, "explicit").in_scope(|| {});
        }
    });
    let spans = exporter.finished_spans();
    let (request, handler) = (by_name(&spans, "request"), by_name(&spans, "handler"));
    assert_eq!(request.context.trace_id, sampled.trace_id);
    assert_eq!(request.parent_span_id, Some(sampled.span_id));
    assert!(request.parent_is_remote);
    assert_eq!(handler.parent_span_id, Some(request.context.span_id));
    assert!(!handler.parent_is_remote);
    assert!(spans.iter().all(|s| s.name != "not sampled upstream"));
    let explicit = by_name(&spans, "explicit");
    assert_ne!(explicit.context.trace_id, sampled.trace_id);
}

#[test]
fn attributes_past_the_limit_are_dropped_and_counted() {
    let exporter = InMemorySpanExporter::default();
    let provider = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_span_limits(SpanLimits {
            max_attributes: 2,
            ..SpanLimits::default()
        })
        .with_simple_exporter(exporter.clone())
        .build();
    with_layer(&provider, || {
        let span = tracing::info_span!("limited", a = 1, b = 2, c = 3, d = 4);
        // Overwriting a kept attribute isn't a new one.
        span.record("a", 10);
        span.in_scope(|| {});
    });
    let span = &exporter.finished_spans()[0];
    assert_eq!(span.attributes.len(), 2);
    assert_eq!(span.dropped_attributes, 2);
    assert_eq!(attr(span, "a"), Some(&Value::I64(10)));
}

#[test]
fn suppressed_threads_record_nothing() {
    let (provider, exporter) = provider(Sampler::AlwaysOn);
    with_layer(&provider, || {
        oxtel_sdk::suppress(|| tracing::info_span!("exporter internals").in_scope(|| {}));
        tracing::info_span!("visible").in_scope(|| {});
    });
    let spans = exporter.finished_spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "visible");
}

#[test]
fn a_filter_on_the_layer_applies_only_to_traces() {
    let (provider, exporter) = provider(Sampler::AlwaysOn);
    let subscriber = tracing_subscriber::registry().with(
        provider
            .layer()
            .with_filter(tracing_subscriber::filter::LevelFilter::INFO),
    );
    tracing::subscriber::with_default(subscriber, || {
        tracing::info_span!("kept").in_scope(|| {
            tracing::debug_span!("filtered").in_scope(|| {
                tracing::info_span!("grandchild").in_scope(|| {});
            });
        });
    });
    let spans = exporter.finished_spans();
    assert!(spans.iter().all(|s| s.name != "filtered"));
    let (kept, grandchild) = (by_name(&spans, "kept"), by_name(&spans, "grandchild"));
    // The filtered span is skipped, not a break in the trace.
    assert_eq!(grandchild.parent_span_id, Some(kept.context.span_id));
}

/// An exporter that says when an export starts, then waits for a signal
/// before finishing it, and counts.
struct Gate {
    started: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
    gate: Arc<Barrier>,
    exported: Arc<AtomicUsize>,
}

impl SpanExporter for Gate {
    fn export(&self, batch: &[SpanData], _: &Resource) -> Result<(), ExportError> {
        let _ = self.started.lock().unwrap().send(());
        self.gate.wait();
        self.exported.fetch_add(batch.len(), Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn the_batch_processor_drops_when_full_and_flushes_on_shutdown() {
    let gate = Arc::new(Barrier::new(2));
    let exported = Arc::new(AtomicUsize::new(0));
    let (started, export_started) = std::sync::mpsc::channel();
    let provider = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_batch_exporter_config(
            Gate {
                started: std::sync::Mutex::new(started),
                gate: Arc::clone(&gate),
                exported: Arc::clone(&exported),
            },
            BatchConfig {
                max_queue_size: 4,
                max_export_batch_size: 1,
                scheduled_delay: Duration::from_secs(60),
                export_timeout: Duration::from_secs(5),
            },
        )
        .build();
    with_layer(&provider, || {
        // The worker takes the first span and blocks in its export; then
        // four more fill the queue, and the other five are dropped.
        tracing::info_span!("first").in_scope(|| {});
        export_started.recv().unwrap();
        for i in 0..9 {
            tracing::info_span!("span", i).in_scope(|| {});
        }
    });
    let stats = provider.stats();
    assert_eq!(stats.dropped, 5, "{stats:?}");
    // Let the five held spans through, then shut down.
    let releaser = std::thread::spawn(move || {
        for _ in 0..5 {
            gate.wait();
        }
    });
    provider.shutdown().expect("shutdown");
    releaser.join().unwrap();
    assert_eq!(exported.load(Ordering::SeqCst), 5);
    assert_eq!(provider.stats().exported, 5);
}

#[test]
fn force_flush_exports_without_waiting_for_the_delay() {
    let exporter = InMemorySpanExporter::default();
    let provider = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_batch_exporter_config(
            exporter.clone(),
            BatchConfig {
                scheduled_delay: Duration::from_secs(60),
                ..BatchConfig::default()
            },
        )
        .build();
    with_layer(&provider, || {
        tracing::info_span!("flushed").in_scope(|| {});
    });
    assert!(exporter.finished_spans().is_empty());
    provider.force_flush().unwrap();
    assert_eq!(exporter.finished_spans().len(), 1);
    assert!(exporter.resource().is_some());
}
