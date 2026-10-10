//! The facade, built without installing anything: what one builder sets up,
//! how its filters split spans from log records, and how it shuts down.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use oxtel::Telemetry;
use oxtel::sdk::export::{BatchConfig, ExportError};
use oxtel::sdk::logs::{InMemoryLogExporter, LocalFormat, LogExporter, LogRecord};
use oxtel::sdk::metrics::{
    InMemoryMetricExporter, MetricExporter, MetricFamily, PeriodicReaderConfig, PointValue,
};
use oxtel::sdk::trace::{InMemorySpanExporter, Sampler, SpanData, SpanExporter, span_context};
use oxtel::{Resource, Value};
use tracing_subscriber::Layer;

const HOURLY: PeriodicReaderConfig = PeriodicReaderConfig {
    interval: Duration::from_secs(3600),
    timeout: Duration::from_secs(5),
};

fn builder() -> oxtel::Builder {
    oxtel::builder()
        .without_env_exporters()
        .with_resource(
            Resource::builder()
                .without_env()
                .with_service_name("facade")
                .build(),
        )
        .with_sampler(Sampler::AlwaysOn)
        .with_traces_filter("info")
        .with_logs_filter("info")
}

struct Exporters {
    spans: InMemorySpanExporter,
    logs: InMemoryLogExporter,
    metrics: InMemoryMetricExporter,
}

fn all_signals(builder: oxtel::Builder) -> (Telemetry, Exporters) {
    let e = Exporters {
        spans: InMemorySpanExporter::default(),
        logs: InMemoryLogExporter::default(),
        metrics: InMemoryMetricExporter::default(),
    };
    let (spans, logs, metrics) = (e.spans.clone(), e.logs.clone(), e.metrics.clone());
    let telemetry = builder
        .with_traces(move |b| b.with_simple_exporter(spans))
        .with_logs(move |b| b.with_simple_exporter(logs))
        .with_metrics(move |b| b.with_periodic_exporter_config(metrics, HOURLY))
        .build()
        .unwrap();
    (telemetry, e)
}

fn run<R>(telemetry: &Telemetry, f: impl FnOnce() -> R) -> R {
    tracing::dispatcher::with_default(telemetry.dispatch(), || match telemetry.recorder() {
        Some(recorder) => metrics::with_local_recorder(recorder, f),
        None => f(),
    })
}

fn bodies(records: &[LogRecord]) -> Vec<String> {
    records
        .iter()
        .filter_map(|r| match &r.body {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn one_builder_sets_up_all_three_signals() {
    let (telemetry, e) = all_signals(builder());
    let request = run(&telemetry, || {
        let span = tracing::info_span!("request");
        span.in_scope(|| {
            tracing::warn!("slow");
            tracing::debug!("hidden");
            tracing::debug_span!("internal").in_scope(|| tracing::info!("in a debug span"));
            metrics::counter!("requests.handled").increment(1);
        });
        span_context(&span).unwrap()
    });
    telemetry.guard().force_flush().unwrap();

    let spans = e.spans.finished_spans();
    assert_eq!(spans.len(), 1, "the debug span is filtered out: {spans:?}");
    assert_eq!(spans[0].name, "request");
    assert!(
        spans[0].events.is_empty(),
        "with a log pipeline, events are log records"
    );

    let records = e.logs.records();
    assert_eq!(bodies(&records), ["slow", "in a debug span"]);
    assert!(
        records.iter().all(|r| r.span_id == Some(request.span_id)),
        "an event in an unrecorded span carries its nearest recorded ancestor's id"
    );

    let exported = e.metrics.last().unwrap();
    let counter = exported
        .iter()
        .find(|m| m.name == "requests.handled")
        .unwrap();
    assert_eq!(counter.points[0].value, PointValue::Counter(1));
    assert_eq!(
        e.metrics.resource().unwrap().get("service.name"),
        Some(&Value::from("facade"))
    );
}

#[test]
fn without_a_log_pipeline_events_stay_span_events() {
    let spans = InMemorySpanExporter::default();
    let exporter = spans.clone();
    let telemetry = builder()
        .with_traces(move |b| b.with_simple_exporter(exporter))
        .build()
        .unwrap();
    assert!(telemetry.guard().logger_provider().is_none());
    assert!(telemetry.recorder().is_none(), "no metrics pipeline either");
    run(&telemetry, || {
        tracing::info_span!("request").in_scope(|| tracing::info!("done"));
    });
    assert_eq!(spans.finished_spans()[0].events[0].name, "done");
}

#[test]
fn log_directives_filter_events_not_the_spans_they_find() {
    let (telemetry, e) = all_signals(builder().with_logs_filter("warn"));
    let request = run(&telemetry, || {
        let span = tracing::info_span!("request");
        span.in_scope(|| {
            tracing::info!("routine");
            tracing::error!("broken");
        });
        span_context(&span).unwrap()
    });
    let records = e.logs.records();
    assert_eq!(bodies(&records), ["broken"]);
    assert_eq!(
        records[0].span_id,
        Some(request.span_id),
        "the info span is still visible to the log layer"
    );
}

#[test]
fn layers_of_your_own_see_everything() {
    let seen = Arc::new(AtomicUsize::new(0));
    struct Count(Arc<AtomicUsize>);
    impl<S: tracing::Subscriber> Layer<S> for Count {
        fn on_event(&self, _: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let telemetry = builder()
        .with_layer(Count(Arc::clone(&seen)))
        .build()
        .unwrap();
    run(&telemetry, || {
        tracing::trace!("fine detail");
        tracing::info!("routine");
    });
    assert_eq!(seen.load(Ordering::Relaxed), 2);
}

#[test]
fn propagation_continues_and_passes_on_the_trace() {
    let spans = InMemorySpanExporter::default();
    let exporter = spans.clone();
    let telemetry = builder()
        .with_traces(move |b| b.with_simple_exporter(exporter))
        .build()
        .unwrap();
    let mut incoming = HashMap::new();
    incoming.insert(
        "traceparent".to_string(),
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
    );
    let outgoing = run(&telemetry, || {
        let span = {
            let _parent = oxtel::propagation::extract_parent(&incoming);
            tracing::info_span!("handle")
        };
        let mut outgoing = HashMap::new();
        assert!(span.in_scope(|| oxtel::propagation::inject_current(&mut outgoing)));
        outgoing
    });
    let span = &spans.finished_spans()[0];
    assert_eq!(
        span.context.trace_id.to_string(),
        "4bf92f3577b34da6a3ce929d0e0e4736"
    );
    assert_eq!(
        outgoing["traceparent"],
        format!(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-{}-01",
            span.context.span_id
        )
    );
}

/// An exporter for every signal that takes `delay` per export.
#[derive(Clone)]
struct Slow(Duration);

impl SpanExporter for Slow {
    fn export(&self, _: &[SpanData], _: &Resource) -> Result<(), ExportError> {
        std::thread::sleep(self.0);
        Ok(())
    }
}

impl LogExporter for Slow {
    fn export(&self, _: &[LogRecord], _: &Resource) -> Result<(), ExportError> {
        std::thread::sleep(self.0);
        Ok(())
    }
}

impl MetricExporter for Slow {
    fn export(&self, _: &[MetricFamily], _: &Resource) -> Result<(), ExportError> {
        std::thread::sleep(self.0);
        Ok(())
    }
}

fn slow_pipelines(delay: Duration, timeout: Duration) -> Telemetry {
    let slow = Slow(delay);
    let (traces, logs, metrics) = (slow.clone(), slow.clone(), slow);
    let batch = BatchConfig {
        scheduled_delay: Duration::from_secs(3600),
        ..BatchConfig::default()
    };
    let lb = batch.clone();
    let telemetry = builder()
        .with_traces(move |b| b.with_batch_exporter_config(traces, batch))
        .with_logs(move |b| b.with_batch_exporter_config(logs, lb))
        .with_metrics(move |b| b.with_periodic_exporter_config(metrics, HOURLY))
        .with_shutdown_timeout(timeout)
        .build()
        .unwrap();
    run(&telemetry, || {
        tracing::info_span!("work").in_scope(|| tracing::info!("done"));
        metrics::counter!("jobs").increment(1);
    });
    telemetry
}

#[test]
fn pipelines_shut_down_together() {
    let telemetry = slow_pipelines(Duration::from_millis(400), Duration::from_secs(5));
    let started = Instant::now();
    telemetry.into_guard().shutdown().unwrap();
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(1000),
        "three 400 ms exports one after another would take 1.2 s; took {took:?}"
    );
}

#[test]
fn shutdown_waits_no_longer_than_its_timeout() {
    let telemetry = slow_pipelines(Duration::from_secs(3), Duration::from_millis(300));
    let started = Instant::now();
    let result = telemetry.into_guard().shutdown();
    assert_eq!(result, Err(ExportError::Timeout));
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
}

/// A writer the test can read back.
#[derive(Clone, Default)]
struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[test]
fn local_lines_carry_the_same_ids_as_exported_records() {
    let out = Captured::default();
    let (telemetry, e) = all_signals(
        builder()
            .with_local_logs(LocalFormat::Json)
            .with_local_logs_writer(out.clone()),
    );
    let request = run(&telemetry, || {
        let span = tracing::info_span!("request", http.route = "/items");
        span.in_scope(|| tracing::warn!(key = "items", "slow"));
        span_context(&span).unwrap()
    });
    let text = out.text();
    assert!(text.contains(r#""message":"slow""#), "{text}");
    assert!(text.contains(r#""http.route":"/items""#), "{text}");
    assert!(
        text.contains(&format!(r#""span_id":"{}""#, request.span_id)),
        "{text}"
    );
    assert_eq!(e.logs.records()[0].span_id, Some(request.span_id));
}

#[test]
fn local_lines_have_their_own_filter() {
    let out = Captured::default();
    let (telemetry, e) = all_signals(
        builder()
            .with_logs_filter("warn")
            .with_local_logs(LocalFormat::Text)
            .with_local_logs_filter("debug")
            .with_local_logs_writer(out.clone()),
    );
    run(&telemetry, || {
        tracing::debug!("detail");
        tracing::trace!("too fine");
        tracing::warn!("trouble");
    });
    let text = out.text();
    assert!(
        text.contains("detail") && text.contains("trouble") && !text.contains("too fine"),
        "{text}"
    );
    assert_eq!(bodies(&e.logs.records()), ["trouble"]);
}
