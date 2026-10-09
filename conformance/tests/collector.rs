//! oxtel's output, as a real OpenTelemetry Collector decodes it. See
//! `README.md` for running a Collector; CI's `collector` job does.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use oxtel_otlp::OtlpHttpExporter;
use oxtel_prometheus::PrometheusExporter;
use oxtel_sdk::context::TraceFlags;
use oxtel_sdk::metrics::{ExemplarFilter, MeterProvider};
use oxtel_sdk::trace::{Sampler, TracerProvider, attach_remote_parent, span_context};
use oxtel_sdk::{KeyValue, Resource, SpanContext, SpanId, TraceId};
use serde_json::Value;
use tracing_subscriber::layer::SubscriberExt;

fn otlp_base() -> String {
    std::env::var("OXTEL_COLLECTOR_OTLP").unwrap_or_else(|_| "http://localhost:4318".into())
}

fn out_dir() -> PathBuf {
    std::env::var("OXTEL_COLLECTOR_OUT").map_or_else(
        |_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("out"),
        PathBuf::from,
    )
}

/// A fresh id to find this run's data among everything the Collector wrote.
fn run_id() -> String {
    TraceId::random().to_string()
}

/// The JSON lines in `file` that mention `marker`, waiting up to `wait`
/// for at least `want` of them.
fn lines_with(file: &str, marker: &str, want: usize, wait: Duration) -> Vec<Value> {
    let path = out_dir().join(file);
    let deadline = Instant::now() + wait;
    loop {
        let found: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(marker))
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        if found.len() >= want || Instant::now() > deadline {
            assert!(
                found.len() >= want,
                "{} line(s) with {marker} in {} after {wait:?}",
                found.len(),
                path.display()
            );
            return found;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// An attribute's OTLP/JSON value, e.g. `{"intValue": "200"}`.
fn attr<'a>(attributes: &'a Value, key: &str) -> &'a Value {
    attributes
        .as_array()
        .and_then(|a| a.iter().find(|kv| kv["key"] == key))
        .map_or(&Value::Null, |kv| &kv["value"])
}

fn spans_named<'a>(requests: &'a [Value], name: &str) -> Vec<&'a Value> {
    requests
        .iter()
        .flat_map(|r| r["resourceSpans"].as_array().into_iter().flatten())
        .flat_map(|rs| rs["scopeSpans"].as_array().into_iter().flatten())
        .flat_map(|ss| ss["spans"].as_array().into_iter().flatten())
        .filter(|s| s["name"] == name)
        .collect()
}

#[test]
#[ignore = "needs a running OpenTelemetry Collector; see conformance/README.md"]
fn traces_reach_the_collector_intact() {
    for gzip in [false, true] {
        let run = run_id();
        let provider = TracerProvider::builder()
            .with_resource(
                Resource::builder()
                    .without_env()
                    .with_service_name("oxtel-conformance")
                    .with_attribute(KeyValue::new("test.run", run.clone()))
                    .build(),
            )
            .with_sampler(Sampler::AlwaysOn)
            .with_batch_exporter(
                OtlpHttpExporter::builder()
                    .with_url(format!("{}/v1/traces", otlp_base()))
                    .with_gzip(gzip)
                    .build(),
            )
            .build();
        let remote = SpanContext {
            trace_id: TraceId::random(),
            span_id: SpanId::random(),
            flags: TraceFlags::SAMPLED.with(TraceFlags::RANDOM),
            is_remote: true,
            ..Default::default()
        };
        let subscriber = tracing_subscriber::registry().with(provider.layer());
        let (server_cx, cause_cx) = tracing::subscriber::with_default(subscriber, || {
            let cause = tracing::info_span!("cause");
            cause.in_scope(|| {});
            let request = {
                let _parent = attach_remote_parent(remote.clone());
                tracing::info_span!(
                    "GET",
                    otel.name = "GET /items",
                    otel.kind = "server",
                    http.request.method = "GET",
                    http.response.status_code = 503,
                    ratio = 0.5,
                    cached = true,
                    otel.status_code = "error",
                    otel.status_description = "store unavailable",
                )
            };
            request.follows_from(&cause);
            request.in_scope(|| {
                tracing::info_span!("load").in_scope(|| {
                    tracing::warn!(retries = 3, "write conflict");
                });
            });
            (
                span_context(&request).unwrap(),
                span_context(&cause).unwrap(),
            )
        });
        provider.force_flush().expect("flush");
        assert_eq!(
            provider.stats().export_failed,
            0,
            "the Collector refused the export"
        );

        let requests = lines_with("traces.json", &run, 1, Duration::from_secs(20));
        let server = spans_named(&requests, "GET /items");
        let load = spans_named(&requests, "load");
        assert_eq!((server.len(), load.len()), (1, 1), "gzip={gzip}");
        let (server, load) = (server[0], load[0]);

        assert_eq!(server["traceId"], remote.trace_id.to_string());
        assert_eq!(server["spanId"], server_cx.span_id.to_string());
        assert_eq!(server["parentSpanId"], remote.span_id.to_string());
        assert_eq!(server["kind"], 2, "SPAN_KIND_SERVER");
        let flags = server["flags"].as_u64().unwrap_or(0);
        assert_eq!(
            flags & 0x3ff,
            0x3 | 0x100 | 0x200,
            "sampled, random, remote parent"
        );
        assert_eq!(server["status"]["code"], 2, "STATUS_CODE_ERROR");
        assert_eq!(server["status"]["message"], "store unavailable");
        let a = &server["attributes"];
        assert_eq!(attr(a, "http.request.method")["stringValue"], "GET");
        assert_eq!(attr(a, "http.response.status_code")["intValue"], "503");
        assert_eq!(attr(a, "ratio")["doubleValue"], 0.5);
        assert_eq!(attr(a, "cached")["boolValue"], true);
        assert_eq!(server["links"][0]["spanId"], cause_cx.span_id.to_string());
        assert_eq!(server["links"][0]["traceId"], cause_cx.trace_id.to_string());

        assert_eq!(load["parentSpanId"], server_cx.span_id.to_string());
        assert_eq!(
            load["flags"].as_u64().unwrap_or(0) & 0x300,
            0x100,
            "local parent"
        );
        let event = &load["events"][0];
        assert_eq!(event["name"], "write conflict");
        assert_eq!(attr(&event["attributes"], "retries")["intValue"], "3");
        let start: u64 = load["startTimeUnixNano"].as_str().unwrap().parse().unwrap();
        let end: u64 = load["endTimeUnixNano"].as_str().unwrap().parse().unwrap();
        assert!(end >= start && start > 0);
    }
}

/// Serve `/metrics` on localhost:9464 for the Collector's Prometheus
/// receiver, negotiating the format from its `Accept` header.
fn serve_metrics(exporter: PrometheusExporter) {
    let listener = TcpListener::bind("0.0.0.0:9464").expect("bind 9464");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut accept = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(':')
                    && k.eq_ignore_ascii_case("accept")
                {
                    accept = Some(v.trim().to_string());
                }
            }
            let (content_type, body) = exporter.render(accept.as_deref());
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
}

fn metrics_named<'a>(batches: &'a [Value], prefix: &str) -> Vec<&'a Value> {
    batches
        .iter()
        .flat_map(|b| b["resourceMetrics"].as_array().into_iter().flatten())
        .flat_map(|rm| rm["scopeMetrics"].as_array().into_iter().flatten())
        .flat_map(|sm| sm["metrics"].as_array().into_iter().flatten())
        .filter(|m| m["name"].as_str().is_some_and(|n| n.starts_with(prefix)))
        .collect()
}

#[test]
#[ignore = "needs a running OpenTelemetry Collector; see conformance/README.md"]
fn the_collector_scrapes_openmetrics_with_exemplars() {
    let run = run_id();
    let tracer = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .build();
    let meters = MeterProvider::builder()
        .with_exemplar_filter(ExemplarFilter::TraceBased)
        .build();
    let subscriber = tracing_subscriber::registry().with(tracer.layer());
    let trace_id = tracing::subscriber::with_default(subscriber, || {
        metrics::with_local_recorder(&meters.recorder(), || {
            metrics::describe_histogram!(
                "http.server.request.duration",
                metrics::Unit::Seconds,
                "Duration of HTTP server requests."
            );
            let span = tracing::info_span!("request");
            let _entered = span.enter();
            let h = metrics::histogram!("http.server.request.duration", "run" => run.clone());
            h.record(0.012);
            h.record(0.2);
            metrics::counter!("jobs.done", "run" => run.clone()).increment(5);
            span_context(&span).unwrap().trace_id
        })
    });
    let resource = Resource::builder()
        .without_env()
        .with_service_name("oxtel-conformance")
        .build();
    serve_metrics(PrometheusExporter::new(meters, resource));

    // The receiver scrapes every second; wait for a scrape carrying both.
    let batches = lines_with("metrics.json", &run, 1, Duration::from_secs(30));
    let histograms = metrics_named(&batches, "http_server_request_duration");
    let histogram = histograms
        .iter()
        .find(|m| m["histogram"].is_object())
        .unwrap_or_else(|| panic!("no histogram in {batches:?}"));
    let point = histogram["histogram"]["dataPoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| attr(&p["attributes"], "run")["stringValue"] == run.as_str())
        .expect("our histogram series");
    assert_eq!(point["count"], "2");
    assert!((point["sum"].as_f64().unwrap() - 0.212).abs() < 1e-9);
    assert_eq!(point["explicitBounds"].as_array().unwrap().len(), 14);
    let exemplars = point["exemplars"].as_array().expect("exemplars");
    assert!(!exemplars.is_empty(), "the Collector kept no exemplars");
    assert!(
        exemplars
            .iter()
            .all(|e| e["traceId"] == trace_id.to_string()),
        "exemplars carry the recording span's trace: {exemplars:?}"
    );

    let counters = metrics_named(&batches, "jobs_done");
    let counter = counters
        .iter()
        .find(|m| m["sum"].is_object())
        .unwrap_or_else(|| panic!("no counter in {batches:?}"));
    assert_eq!(counter["sum"]["isMonotonic"], true);
    let point = counter["sum"]["dataPoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| attr(&p["attributes"], "run")["stringValue"] == run.as_str())
        .expect("our counter series");
    let value = point["asDouble"]
        .as_f64()
        .or_else(|| point["asInt"].as_str().and_then(|v| v.parse::<f64>().ok()));
    assert_eq!(value, Some(5.0));
}
