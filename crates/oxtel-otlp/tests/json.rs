//! OTLP/JSON: real spans, log records and metrics, encoded and parsed back
//! to check the specification's rules: lowerCamelCase names, hex ids,
//! integer enums, 64-bit integers as strings.

use oxtel_otlp::json;
use oxtel_otlp::transform::{logs_request, metrics_request, traces_request};
use oxtel_sdk::logs::{InMemoryLogExporter, LoggerProvider};
use oxtel_sdk::metrics::{ExemplarFilter, MeterProvider};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, TracerProvider, span_context};
use oxtel_sdk::{Resource, SpanContext};
use serde_json::Value;
use tracing_subscriber::layer::SubscriberExt;

fn resource() -> Resource {
    Resource::builder()
        .without_env()
        .with_service_name("checkout")
        .build()
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn is_hex(v: &Value, len: usize) -> bool {
    v.as_str().is_some_and(|s| {
        s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Everything recorded in one sampled span: a span, a log record, metrics.
fn record() -> (
    InMemorySpanExporter,
    InMemoryLogExporter,
    MeterProvider,
    SpanContext,
) {
    let spans = InMemorySpanExporter::default();
    let logs = InMemoryLogExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(resource())
        .with_sampler(Sampler::AlwaysOn)
        .with_span_events(false)
        .with_simple_exporter(spans.clone())
        .build();
    let log_provider = LoggerProvider::builder()
        .with_resource(resource())
        .with_simple_exporter(logs.clone())
        .build();
    let meters = MeterProvider::builder()
        .with_resource(resource())
        .with_exemplar_filter(ExemplarFilter::TraceBased)
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(traces.layer())
        .with(log_provider.layer());
    let cx = tracing::subscriber::with_default(subscriber, || {
        metrics::with_local_recorder(&meters.recorder(), || {
            metrics::describe_histogram!("job.duration", metrics::Unit::Seconds, "How long.");
            let span = tracing::info_span!(
                "GET /items",
                otel.kind = "server",
                http.response.status_code = 200,
                ratio = 0.5,
                otel.status_code = "error",
                otel.status_description = "broken \"quote\"\n"
            );
            span.in_scope(|| {
                tracing::warn!(retries = 3, "slow");
                metrics::counter!("jobs.done", "queue" => "email").increment(5);
                metrics::histogram!("job.duration").record(0.25);
                metrics::gauge!("queue.depth").set(-1.5);
            });
            span_context(&span).unwrap()
        })
    });
    (spans, logs, meters, cx)
}

fn attr<'a>(attributes: &'a Value, key: &str) -> &'a Value {
    attributes
        .as_array()
        .and_then(|a| a.iter().find(|kv| kv["key"] == key))
        .map_or(&Value::Null, |kv| &kv["value"])
}

#[test]
fn spans_follow_the_otlp_json_rules() {
    let (spans, _, _, cx) = record();
    let text = json::traces(&traces_request(&spans.finished_spans(), &resource()));
    let doc = parse(&text);
    let rs = &doc["resourceSpans"][0];
    assert_eq!(
        attr(&rs["resource"]["attributes"], "service.name")["stringValue"],
        "checkout"
    );
    let span = &rs["scopeSpans"][0]["spans"][0];
    assert!(
        is_hex(&span["traceId"], 32),
        "hex, not base64: {}",
        span["traceId"]
    );
    assert_eq!(span["traceId"], cx.trace_id.to_string());
    assert!(is_hex(&span["spanId"], 16));
    assert_eq!(span["kind"], 2, "enums are integers");
    assert!(
        span["startTimeUnixNano"].is_string(),
        "64-bit integers are strings"
    );
    assert!(span.get("parentSpanId").is_none(), "defaults are left out");
    assert_eq!(
        attr(&span["attributes"], "http.response.status_code")["intValue"],
        "200"
    );
    assert_eq!(attr(&span["attributes"], "ratio")["doubleValue"], 0.5);
    assert_eq!(span["status"]["code"], 2);
    assert_eq!(
        span["status"]["message"], "broken \"quote\"\n",
        "strings escaped"
    );
    assert_eq!(
        span["flags"], 0x03,
        "sampled and random; a root has no parent to be remote"
    );
}

#[test]
fn log_records_follow_the_otlp_json_rules() {
    let (_, logs, _, cx) = record();
    let doc = parse(&json::logs(&logs_request(&logs.records(), &resource())));
    let record = &doc["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0];
    assert_eq!(record["severityNumber"], 13);
    assert_eq!(record["severityText"], "WARN");
    assert_eq!(record["body"]["stringValue"], "slow");
    assert_eq!(attr(&record["attributes"], "retries")["intValue"], "3");
    assert_eq!(record["traceId"], cx.trace_id.to_string());
    assert_eq!(record["spanId"], cx.span_id.to_string());
    assert!(record["timeUnixNano"].is_string());
}

#[test]
fn metrics_follow_the_otlp_json_rules() {
    let (_, _, meters, cx) = record();
    let doc = parse(&json::metrics(&metrics_request(
        &meters.collect(),
        &resource(),
    )));
    let metrics = doc["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
        .as_array()
        .unwrap();
    let named = |name: &str| metrics.iter().find(|m| m["name"] == name).unwrap();

    let sum = &named("jobs.done")["sum"];
    assert_eq!(sum["aggregationTemporality"], 2);
    assert_eq!(sum["isMonotonic"], true);
    let point = &sum["dataPoints"][0];
    assert_eq!(point["asInt"], "5");
    assert_eq!(attr(&point["attributes"], "queue")["stringValue"], "email");
    assert_eq!(point["exemplars"][0]["traceId"], cx.trace_id.to_string());

    let histogram = named("job.duration");
    assert_eq!(histogram["unit"], "s");
    let point = &histogram["histogram"]["dataPoints"][0];
    assert_eq!(point["count"], "1");
    assert_eq!(point["sum"], 0.25);
    assert!(
        point["bucketCounts"]
            .as_array()
            .unwrap()
            .iter()
            .all(Value::is_string)
    );
    assert!(
        point["explicitBounds"]
            .as_array()
            .unwrap()
            .iter()
            .all(Value::is_f64)
    );
    assert_eq!(
        (point["min"].as_f64(), point["max"].as_f64()),
        (Some(0.25), Some(0.25))
    );

    assert_eq!(
        named("queue.depth")["gauge"]["dataPoints"][0]["asDouble"],
        -1.5
    );
}
