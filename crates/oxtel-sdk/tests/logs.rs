//! The log layer: what a `tracing` event becomes, and how it's correlated
//! with the span it happened in.

use std::time::Duration;

use oxtel_sdk::context::TraceFlags;
use oxtel_sdk::logs::{
    BatchConfig, InMemoryLogExporter, LogLimits, LogRecord, LoggerProvider, Severity,
};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, TracerProvider, span_context};
use oxtel_sdk::{KeyValue, Resource, Value};
use tracing_subscriber::layer::SubscriberExt;

fn logs() -> (LoggerProvider, InMemoryLogExporter) {
    logs_with(LoggerProvider::builder())
}

fn logs_with(
    builder: oxtel_sdk::logs::LoggerProviderBuilder,
) -> (LoggerProvider, InMemoryLogExporter) {
    let exporter = InMemoryLogExporter::default();
    let provider = builder
        .with_resource(Resource::builder().without_env().build())
        .with_limits(LogLimits::default())
        .with_simple_exporter(exporter.clone())
        .build();
    (provider, exporter)
}

fn traces(sampler: Sampler) -> TracerProvider {
    TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(sampler)
        .build()
}

fn attr<'a>(record: &'a LogRecord, key: &str) -> Option<&'a Value> {
    record
        .attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| &kv.value)
}

#[test]
fn events_become_log_records() {
    let (provider, exporter) = logs();
    let subscriber = tracing_subscriber::registry().with(provider.layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::warn!(
            retries = 3,
            big = u64::MAX,
            ratio = 0.5,
            cached = true,
            store = "sqlite",
            reason = ?Some("busy"),
            "write conflict on {}",
            "items"
        );
        tracing::trace!("fine detail");
    });
    let records = exporter.records();
    assert_eq!(records.len(), 2);
    let r = &records[0];
    assert_eq!(r.severity, Severity::WARN);
    assert_eq!(r.severity_text, "WARN");
    assert_eq!(r.body, Some(Value::from("write conflict on items")));
    assert_eq!(attr(r, "retries"), Some(&Value::I64(3)));
    assert_eq!(
        attr(r, "big"),
        Some(&Value::from(u64::MAX.to_string())),
        "u64 beyond i64 becomes a string"
    );
    assert_eq!(attr(r, "ratio"), Some(&Value::F64(0.5)));
    assert_eq!(attr(r, "cached"), Some(&Value::Bool(true)));
    assert_eq!(attr(r, "store"), Some(&Value::from("sqlite")));
    assert_eq!(attr(r, "reason"), Some(&Value::from("Some(\"busy\")")));
    assert_eq!(attr(r, "message"), None, "the message is the body");
    assert_eq!(r.attributes.len(), 6);
    assert_eq!(r.scope.name, "logs", "the target is the scope");
    assert_eq!(r.event_name, None, "tracing's generated name isn't kept");
    assert_eq!((r.trace_id, r.span_id), (None, None), "no span, no context");
    assert_eq!(r.time, r.observed_time);
    assert!(r.time.elapsed().unwrap() < Duration::from_secs(5));
    assert_eq!(records[1].severity, Severity::TRACE);
    assert_eq!(records[1].severity_text, "TRACE");
}

#[test]
fn named_events_keep_their_name_and_targets_their_scope() {
    let (provider, exporter) = logs();
    let subscriber = tracing_subscriber::registry().with(provider.layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(name: "user.login", target: "auth::session", user = "u-1");
    });
    let r = &exporter.records()[0];
    assert_eq!(r.event_name.as_deref(), Some("user.login"));
    assert_eq!(r.scope.name, "auth::session");
    assert_eq!(r.body, None, "no message, no body");
    assert_eq!(attr(r, "user"), Some(&Value::from("u-1")));
}

#[test]
fn records_inside_spans_carry_their_trace_context() {
    let (logs, exporter) = logs();
    let traces = traces(Sampler::AlwaysOn);
    let subscriber = tracing_subscriber::registry()
        .with(traces.layer())
        .with(logs.layer());
    let (outer, inner) = tracing::subscriber::with_default(subscriber, || {
        let outer = tracing::info_span!("request");
        let inner = tracing::info_span!(parent: &outer, "load");
        outer.in_scope(|| {
            tracing::info!("in outer");
            inner.in_scope(|| tracing::info!("in inner"));
        });
        tracing::info!(parent: &inner, "explicit parent");
        tracing::info!(parent: None, "explicit root");
        (span_context(&outer).unwrap(), span_context(&inner).unwrap())
    });
    let records = exporter.records();
    let context = |r: &LogRecord| (r.trace_id, r.span_id, r.trace_flags);
    assert_eq!(
        context(&records[0]),
        (Some(outer.trace_id), Some(outer.span_id), outer.flags)
    );
    assert_eq!(
        context(&records[1]),
        (Some(inner.trace_id), Some(inner.span_id), inner.flags)
    );
    assert_eq!(context(&records[2]), context(&records[1]));
    assert_eq!(context(&records[3]), (None, None, TraceFlags::default()));
    assert!(outer.flags.is_sampled());
}

#[test]
fn unsampled_spans_still_lend_their_ids() {
    let (logs, exporter) = logs();
    let traces = traces(Sampler::AlwaysOff);
    let subscriber = tracing_subscriber::registry()
        .with(traces.layer())
        .with(logs.layer());
    let span = tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("request");
        span.in_scope(|| tracing::error!("failed"));
        span_context(&span).unwrap()
    });
    let r = &exporter.records()[0];
    assert_eq!(
        (r.trace_id, r.span_id),
        (Some(span.trace_id), Some(span.span_id))
    );
    assert!(!r.trace_flags.is_sampled());
}

#[test]
fn events_can_be_log_records_instead_of_span_events() {
    let (logs, log_exporter) = logs();
    let span_exporter = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_span_events(false)
        .with_simple_exporter(span_exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(traces.layer())
        .with(logs.layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info_span!("request").in_scope(|| tracing::warn!("slow"));
    });
    let span = &span_exporter.finished_spans()[0];
    assert!(span.events.is_empty());
    assert_eq!(
        log_exporter.records()[0].span_id,
        Some(span.context.span_id)
    );
}

#[test]
fn the_attribute_limit_drops_and_counts() {
    let exporter = InMemoryLogExporter::default();
    let provider = LoggerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_limits(LogLimits { max_attributes: 2 })
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry().with(provider.layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(a = 1, b = 2, c = 3, d = 4, "four fields");
    });
    let r = &exporter.records()[0];
    assert_eq!(
        r.attributes,
        vec![KeyValue::new("a", 1i64), KeyValue::new("b", 2i64)]
    );
    assert_eq!(r.dropped_attributes, 2);
    assert_eq!(
        r.body,
        Some(Value::from("four fields")),
        "the body isn't an attribute"
    );
}

#[test]
fn code_attributes_are_opt_in() {
    let (plain, plain_exporter) = logs();
    let (coded, coded_exporter) = logs_with(LoggerProvider::builder().with_code_attributes(true));
    let subscriber = tracing_subscriber::registry()
        .with(plain.layer())
        .with(coded.layer());
    let line = line!() + 2;
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("here");
    });
    assert!(plain_exporter.records()[0].attributes.is_empty());
    let r = &coded_exporter.records()[0];
    assert_eq!(attr(r, "code.file.path"), Some(&Value::from(file!())));
    assert_eq!(
        attr(r, "code.line.number"),
        Some(&Value::I64(i64::from(line)))
    );
}

#[test]
fn log_crate_records_keep_their_target() {
    tracing_log::LogTracer::init().expect("install the log bridge");
    let (provider, exporter) = logs_with(LoggerProvider::builder().with_code_attributes(true));
    let subscriber = tracing_subscriber::registry().with(provider.layer());
    tracing::subscriber::with_default(subscriber, || {
        log::warn!(target: "legacy::db", "pool exhausted after {} tries", 3);
    });
    let records = exporter.records();
    let r = records
        .iter()
        .find(|r| r.scope.name == "legacy::db")
        .unwrap_or_else(|| panic!("no record from the log crate: {records:?}"));
    assert_eq!(r.severity, Severity::WARN);
    assert_eq!(r.body, Some(Value::from("pool exhausted after 3 tries")));
    assert_eq!(r.event_name, None);
    assert!(
        r.attributes.iter().all(|kv| !kv.key.starts_with("log.")),
        "the bridge's fields are consumed: {:?}",
        r.attributes
    );
    assert_eq!(attr(r, "code.file.path"), Some(&Value::from(file!())));
}

#[test]
fn suppressed_events_are_not_recorded() {
    let (provider, exporter) = logs();
    let subscriber = tracing_subscriber::registry().with(provider.layer());
    tracing::subscriber::with_default(subscriber, || {
        oxtel_sdk::suppress(|| tracing::info!("from an exporter"));
        tracing::info!("from the program");
    });
    let records = exporter.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].body, Some(Value::from("from the program")));
}

#[test]
fn the_batch_processor_exports_and_counts() {
    let exporter = InMemoryLogExporter::default();
    let provider = LoggerProvider::builder()
        .with_resource(
            Resource::builder()
                .without_env()
                .with_service_name("checkout")
                .build(),
        )
        .with_batch_exporter_config(
            exporter.clone(),
            BatchConfig {
                max_queue_size: 100,
                scheduled_delay: Duration::from_secs(60),
                max_export_batch_size: 10,
                export_timeout: Duration::from_secs(5),
            },
        )
        .build();
    let subscriber = tracing_subscriber::registry().with(provider.layer());
    tracing::subscriber::with_default(subscriber, || {
        for i in 0..25 {
            tracing::info!(i, "item");
        }
    });
    provider.force_flush().unwrap();
    assert_eq!(exporter.records().len(), 25);
    assert_eq!(provider.stats().exported, 25);
    assert_eq!(
        exporter.resource().unwrap().get("service.name"),
        Some(&Value::from("checkout"))
    );
    provider.shutdown().unwrap();
    let subscriber = tracing_subscriber::registry().with(provider.layer());
    tracing::subscriber::with_default(subscriber, || tracing::info!("too late"));
    assert_eq!(provider.stats().dropped, 1);
}

#[test]
fn severities_cover_opentelemetrys_range() {
    assert_eq!(Severity::new(0), None);
    assert_eq!(Severity::new(25), None);
    let names: Vec<String> = (1..=24)
        .map(|n| format!("{:?}", Severity::new(n).unwrap()))
        .collect();
    assert_eq!(names[0], "TRACE");
    assert_eq!(names[13], "WARN2");
    assert_eq!(names[23], "FATAL4");
    assert_eq!(Severity::new(14).unwrap().name(), "WARN");
    assert!(Severity::ERROR > Severity::WARN);
    assert_eq!(Severity::from(tracing::Level::DEBUG).number(), 5);
}
