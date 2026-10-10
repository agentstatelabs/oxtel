//! OTLP/HTTP export against a local receiver that records each request.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oxtel_otlp::proto::collector::logs::v1::{
    ExportLogsPartialSuccess, ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use oxtel_otlp::proto::collector::metrics::v1::{
    ExportMetricsPartialSuccess, ExportMetricsServiceRequest, ExportMetricsServiceResponse,
};
use oxtel_otlp::proto::collector::trace::v1::{
    ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use oxtel_otlp::proto::common::v1::any_value::Value as AnyValue;
use oxtel_otlp::{OtlpHttpExporter, Protocol, Signal};
use oxtel_sdk::context::TraceFlags;
use oxtel_sdk::logs::{InMemoryLogExporter, LogExporter, LogRecord, LoggerProvider};
use oxtel_sdk::metrics::{
    ExemplarFilter, MeterProvider, MetricExporter, MetricFamily, PeriodicReaderConfig,
};
use oxtel_sdk::trace::{
    ExportError, InMemorySpanExporter, Sampler, SpanData, SpanExporter, TracerProvider,
    attach_remote_parent, span_context,
};
use oxtel_sdk::{KeyValue, Resource, SpanContext, SpanId, TraceId};
use prost::Message;
use tracing_subscriber::layer::SubscriberExt;

struct Request {
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A scripted reply: status, extra headers, body.
type Reply = (u16, Vec<(&'static str, String)>, Vec<u8>);

/// A receiver that answers each connection with the next scripted reply,
/// `(status, extra headers, body)`, and records what it was sent.
fn receiver(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<Request>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    std::thread::spawn(move || {
        for (status, extra, reply) in replies {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let mut headers = Vec::new();
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            let len = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            record.lock().unwrap().push(Request {
                path,
                headers,
                body,
            });
            let mut stream = stream;
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                reply.len()
            );
            for (k, v) in extra {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&reply).unwrap();
        }
    });
    (base, seen)
}

/// Spans recorded through the real layer.
fn spans() -> (Vec<SpanData>, SpanContext) {
    let exporter = InMemorySpanExporter::default();
    let provider = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(exporter.clone())
        .build();
    let remote = SpanContext {
        trace_id: TraceId::random(),
        span_id: SpanId::random(),
        flags: TraceFlags::SAMPLED.with(TraceFlags::RANDOM),
        is_remote: true,
        ..Default::default()
    };
    let subscriber = tracing_subscriber::registry().with(provider.layer());
    tracing::subscriber::with_default(subscriber, || {
        let request = {
            let _parent = attach_remote_parent(remote.clone());
            tracing::info_span!(
                "GET /items",
                otel.kind = "server",
                http.response.status_code = 200
            )
        };
        request.in_scope(|| tracing::info_span!("load").in_scope(|| {}));
    });
    (exporter.finished_spans(), remote)
}

fn resource() -> Resource {
    Resource::builder()
        .without_env()
        .with_service_name("checkout")
        .build()
}

#[test]
fn spans_arrive_as_protobuf_at_v1_traces() {
    let (base, seen) = receiver(vec![(200, vec![], vec![])]);
    let exporter = OtlpHttpExporter::builder()
        .with_endpoint(&base)
        .with_header("api-key", "k")
        .build();
    let (batch, remote) = spans();
    SpanExporter::export(&exporter, &batch, &resource()).unwrap();

    let seen = seen.lock().unwrap();
    let request = &seen[0];
    assert_eq!(request.path, "/v1/traces");
    assert_eq!(
        request.header("content-type"),
        Some("application/x-protobuf")
    );
    assert_eq!(request.header("api-key"), Some("k"));
    let decoded = ExportTraceServiceRequest::decode(request.body.as_slice()).unwrap();
    let rs = &decoded.resource_spans[0];
    let service = rs
        .resource
        .as_ref()
        .unwrap()
        .attributes
        .iter()
        .find(|kv| kv.key == "service.name");
    assert_eq!(
        service
            .and_then(|kv| kv.value.as_ref())
            .and_then(|v| v.value.clone()),
        Some(AnyValue::StringValue("checkout".into()))
    );
    let scope = &rs.scope_spans[0];
    assert_eq!(scope.scope.as_ref().unwrap().name, "http_export");
    let by_name = |n: &str| scope.spans.iter().find(|s| s.name == n).unwrap();
    let (server, load) = (by_name("GET /items"), by_name("load"));
    assert_eq!(server.trace_id, remote.trace_id.to_bytes());
    assert_eq!(server.parent_span_id, remote.span_id.to_bytes());
    assert_eq!(server.kind, 2, "SPAN_KIND_SERVER");
    // Sampled and random flags, plus "parent is remote".
    assert_eq!(server.flags, 0x03 | 0x100 | 0x200);
    assert_eq!(load.parent_span_id, server.span_id);
    assert_eq!(load.flags, 0x03 | 0x100, "local parent");
    assert!(load.end_time_unix_nano >= load.start_time_unix_nano);
    let status = server
        .attributes
        .iter()
        .find(|kv| kv.key == "http.response.status_code");
    assert_eq!(
        status
            .and_then(|kv| kv.value.as_ref())
            .and_then(|v| v.value.clone()),
        Some(AnyValue::IntValue(200))
    );
}

#[test]
fn gzip_bodies_decompress_to_the_same_request() {
    let (base, seen) = receiver(vec![(200, vec![], vec![])]);
    let exporter = OtlpHttpExporter::builder()
        .with_endpoint(&base)
        .with_gzip(true)
        .build();
    let (batch, _) = spans();
    SpanExporter::export(&exporter, &batch, &resource()).unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].header("content-encoding"), Some("gzip"));
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(seen[0].body.as_slice())
        .read_to_end(&mut raw)
        .unwrap();
    let decoded = ExportTraceServiceRequest::decode(raw.as_slice()).unwrap();
    assert_eq!(decoded.resource_spans[0].scope_spans[0].spans.len(), 2);
}

#[test]
fn retryable_statuses_are_retried_and_others_are_not() {
    let (base, seen) = receiver(vec![
        (503, vec![("Retry-After", "0".into())], vec![]),
        (200, vec![], vec![]),
    ]);
    let exporter = OtlpHttpExporter::builder()
        .with_endpoint(&base)
        .with_initial_backoff(Duration::from_millis(10))
        .build();
    let (batch, _) = spans();
    SpanExporter::export(&exporter, &batch, &resource()).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);

    let (base, seen) = receiver(vec![
        (400, vec![], b"bad request".to_vec()),
        (200, vec![], vec![]),
    ]);
    let exporter = OtlpHttpExporter::builder()
        .with_endpoint(&base)
        .with_initial_backoff(Duration::from_millis(10))
        .build();
    let err = SpanExporter::export(&exporter, &batch, &resource()).unwrap_err();
    assert_eq!(
        err,
        ExportError::Rejected {
            status: 400,
            message: "bad request".into()
        }
    );
    assert_eq!(seen.lock().unwrap().len(), 1, "a 400 isn't retried");
}

#[test]
fn partial_success_counts_rejected_spans() {
    let reply = ExportTraceServiceResponse {
        partial_success: Some(ExportTracePartialSuccess {
            rejected_spans: 2,
            error_message: "too old".into(),
        }),
    }
    .encode_to_vec();
    let (base, _) = receiver(vec![(200, vec![], reply)]);
    let exporter = OtlpHttpExporter::builder().with_endpoint(&base).build();
    let (batch, _) = spans();
    SpanExporter::export(&exporter, &batch, &resource()).unwrap();
    assert_eq!(exporter.rejected(Signal::Traces), 2);
}

#[test]
fn an_unreachable_receiver_fails_within_the_timeout() {
    // Bind and drop, so the port refuses connections.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let exporter = OtlpHttpExporter::builder()
        .with_endpoint(&format!("http://127.0.0.1:{port}"))
        .with_timeout(Duration::from_millis(400))
        .with_initial_backoff(Duration::from_millis(50))
        .build();
    let (batch, _) = spans();
    let started = Instant::now();
    let err = SpanExporter::export(&exporter, &batch, &resource()).unwrap_err();
    assert!(matches!(err, ExportError::Transport(_)), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn the_whole_pipeline_exports_on_flush() {
    let (base, seen) = receiver(vec![(200, vec![], vec![])]);
    let provider = TracerProvider::builder()
        .with_resource(resource())
        .with_sampler(Sampler::AlwaysOn)
        .with_batch_exporter(OtlpHttpExporter::builder().with_endpoint(&base).build())
        .build();
    let subscriber = tracing_subscriber::registry().with(provider.layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info_span!("job", items = 3).in_scope(|| {
            tracing::info!(processed = 3, "done");
        });
    });
    provider.force_flush().unwrap();
    let seen = seen.lock().unwrap();
    let decoded = ExportTraceServiceRequest::decode(seen[0].body.as_slice()).unwrap();
    let span = &decoded.resource_spans[0].scope_spans[0].spans[0];
    assert_eq!(span.name, "job");
    assert_eq!(span.events[0].name, "done");
    assert!(
        span.attributes
            .contains(&oxtel_otlp::transform::key_value(&KeyValue::new(
                "items", 3i64
            )))
    );
    assert_eq!(provider.stats().exported, 1);
}

/// Log records recorded through the real layers: one inside a sampled
/// span, one outside any span.
fn log_records() -> (Vec<LogRecord>, SpanContext) {
    let exporter = InMemoryLogExporter::default();
    let logs = LoggerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_simple_exporter(exporter.clone())
        .build();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(traces.layer())
        .with(logs.layer());
    let cx = tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("request");
        span.in_scope(|| tracing::warn!(name: "cache.miss", key = "items", "slow lookup"));
        tracing::info!("outside");
        span_context(&span).unwrap()
    });
    (exporter.records(), cx)
}

fn string_attr(
    attributes: &[oxtel_otlp::proto::common::v1::KeyValue],
    key: &str,
) -> Option<AnyValue> {
    attributes
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref())
        .and_then(|v| v.value.clone())
}

#[test]
fn log_records_arrive_as_protobuf_at_v1_logs() {
    let (base, seen) = receiver(vec![(200, vec![], vec![])]);
    let exporter = OtlpHttpExporter::builder().with_endpoint(&base).build();
    let (batch, cx) = log_records();
    LogExporter::export(&exporter, &batch, &resource()).unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].path, "/v1/logs");
    assert_eq!(
        seen[0].header("content-type"),
        Some("application/x-protobuf")
    );
    let decoded = ExportLogsServiceRequest::decode(seen[0].body.as_slice()).unwrap();
    let rl = &decoded.resource_logs[0];
    assert_eq!(
        string_attr(&rl.resource.as_ref().unwrap().attributes, "service.name"),
        Some(AnyValue::StringValue("checkout".into()))
    );
    let scope = &rl.scope_logs[0];
    assert_eq!(scope.scope.as_ref().unwrap().name, "http_export");
    let (inside, outside) = (&scope.log_records[0], &scope.log_records[1]);
    assert_eq!(inside.severity_number, 13, "SEVERITY_NUMBER_WARN");
    assert_eq!(inside.severity_text, "WARN");
    assert_eq!(
        inside.body.as_ref().and_then(|b| b.value.clone()),
        Some(AnyValue::StringValue("slow lookup".into()))
    );
    assert_eq!(
        string_attr(&inside.attributes, "key"),
        Some(AnyValue::StringValue("items".into()))
    );
    assert_eq!(inside.event_name, "cache.miss");
    assert_eq!(inside.trace_id, cx.trace_id.to_bytes());
    assert_eq!(inside.span_id, cx.span_id.to_bytes());
    assert_eq!(inside.flags, 0x03, "sampled and random");
    assert!(inside.time_unix_nano > 0);
    assert_eq!(inside.observed_time_unix_nano, inside.time_unix_nano);
    assert_eq!(outside.severity_number, 9, "SEVERITY_NUMBER_INFO");
    assert!(outside.trace_id.is_empty() && outside.span_id.is_empty());
    assert_eq!((outside.flags, outside.event_name.as_str()), (0, ""));
}

#[test]
fn one_exporter_serves_traces_and_logs() {
    let (base, seen) = receiver(vec![(200, vec![], vec![]), (200, vec![], vec![])]);
    let exporter = OtlpHttpExporter::builder().with_endpoint(&base).build();
    let traces = TracerProvider::builder()
        .with_resource(resource())
        .with_sampler(Sampler::AlwaysOn)
        .with_span_events(false)
        .with_simple_exporter(exporter.clone())
        .build();
    let logs = LoggerProvider::builder()
        .with_resource(resource())
        .with_simple_exporter(exporter)
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(traces.layer())
        .with(logs.layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info_span!("job").in_scope(|| tracing::info!("done"));
    });
    let seen = seen.lock().unwrap();
    let paths: Vec<&str> = seen.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, ["/v1/logs", "/v1/traces"]);
    let span = &ExportTraceServiceRequest::decode(seen[1].body.as_slice())
        .unwrap()
        .resource_spans[0]
        .scope_spans[0]
        .spans[0];
    let record = &ExportLogsServiceRequest::decode(seen[0].body.as_slice())
        .unwrap()
        .resource_logs[0]
        .scope_logs[0]
        .log_records[0];
    assert_eq!(record.span_id, span.span_id);
    assert!(
        span.events.is_empty(),
        "the event went out as a log record only"
    );
}

#[test]
fn partial_success_counts_rejected_log_records() {
    let reply = ExportLogsServiceResponse {
        partial_success: Some(ExportLogsPartialSuccess {
            rejected_log_records: 1,
            error_message: "too large".into(),
        }),
    }
    .encode_to_vec();
    let (base, _) = receiver(vec![(200, vec![], reply)]);
    let exporter = OtlpHttpExporter::builder().with_endpoint(&base).build();
    let (batch, _) = log_records();
    LogExporter::export(&exporter, &batch, &resource()).unwrap();
    assert_eq!(exporter.rejected(Signal::Logs), 1);
    assert_eq!(exporter.rejected(Signal::Traces), 0);
}

/// Runs in a child process (see the test below), with the environment it
/// sets: exports a span and a log record, each to its own settings.
fn export_from_the_environment() {
    let exporter = OtlpHttpExporter::builder().build();
    let base = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap();
    assert_eq!(exporter.url(Signal::Traces), format!("{base}/v1/traces"));
    assert_eq!(exporter.url(Signal::Metrics), format!("{base}/v1/metrics"));
    assert_eq!(exporter.url(Signal::Logs), format!("{base}/custom/logs"));
    let (spans, _) = spans();
    SpanExporter::export(&exporter, &spans, &resource()).unwrap();
    let (records, _) = log_records();
    LogExporter::export(&exporter, &records, &resource()).unwrap();
}

#[test]
fn each_signal_takes_its_settings_from_the_environment() {
    const CHILD: &str = "OXTEL_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        export_from_the_environment();
        return;
    }
    let (base, seen) = receiver(vec![(200, vec![], vec![]), (200, vec![], vec![])]);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "each_signal_takes_its_settings_from_the_environment",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", &base)
        .env(
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
            format!("{base}/custom/logs"),
        )
        .env("OTEL_EXPORTER_OTLP_HEADERS", "api-key=shared,x-team=core")
        .env("OTEL_EXPORTER_OTLP_LOGS_HEADERS", "API-KEY=logs%20only")
        .env("OTEL_EXPORTER_OTLP_LOGS_COMPRESSION", "gzip")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let seen = seen.lock().unwrap();
    let (traces, logs) = (&seen[0], &seen[1]);
    assert_eq!(traces.path, "/v1/traces");
    assert_eq!(traces.header("api-key"), Some("shared"));
    assert_eq!(traces.header("x-team"), Some("core"));
    assert_eq!(traces.header("content-encoding"), None);
    assert_eq!(logs.path, "/custom/logs");
    assert_eq!(
        logs.header("api-key"),
        Some("logs only"),
        "the signal's own header wins"
    );
    assert_eq!(logs.header("x-team"), Some("core"));
    assert_eq!(logs.header("content-encoding"), Some("gzip"));
}

/// Metrics recorded through the real recorder, inside a sampled span, so
/// they carry exemplars.
fn metric_families(meters: &MeterProvider) -> (Vec<MetricFamily>, SpanContext) {
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .build();
    let subscriber = tracing_subscriber::registry().with(traces.layer());
    let cx = tracing::subscriber::with_default(subscriber, || {
        metrics::with_local_recorder(&meters.recorder(), || {
            metrics::describe_histogram!(
                "http.server.request.duration",
                metrics::Unit::Seconds,
                "Duration of HTTP server requests."
            );
            let span = tracing::info_span!("request");
            let _entered = span.enter();
            let h = metrics::histogram!("http.server.request.duration", "http.route" => "/items");
            h.record(0.012);
            h.record(0.2);
            metrics::counter!("jobs.done", "queue" => "email").increment(5);
            metrics::gauge!("queue.depth").set(7.5);
            span_context(&span).unwrap()
        })
    });
    (meters.collect(), cx)
}

fn meter_provider() -> MeterProvider {
    MeterProvider::builder()
        .with_resource(resource())
        .with_exemplar_filter(ExemplarFilter::TraceBased)
        .build()
}

fn metric<'a>(
    request: &'a ExportMetricsServiceRequest,
    name: &str,
) -> &'a oxtel_otlp::proto::metrics::v1::Metric {
    request.resource_metrics[0]
        .scope_metrics
        .iter()
        .flat_map(|s| &s.metrics)
        .find(|m| m.name == name)
        .unwrap_or_else(|| panic!("no {name} in {request:?}"))
}

#[test]
fn metrics_arrive_as_protobuf_at_v1_metrics() {
    use oxtel_otlp::proto::metrics::v1::metric::Data;
    use oxtel_otlp::proto::metrics::v1::number_data_point::Value as Number;

    let (base, seen) = receiver(vec![(200, vec![], vec![])]);
    let exporter = OtlpHttpExporter::builder().with_endpoint(&base).build();
    let meters = meter_provider();
    let (families, cx) = metric_families(&meters);
    MetricExporter::export(&exporter, &families, &resource()).unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].path, "/v1/metrics");
    let request = ExportMetricsServiceRequest::decode(seen[0].body.as_slice()).unwrap();
    let rm = &request.resource_metrics[0];
    assert_eq!(
        string_attr(&rm.resource.as_ref().unwrap().attributes, "service.name"),
        Some(AnyValue::StringValue("checkout".into()))
    );
    assert_eq!(
        rm.scope_metrics[0].scope.as_ref().unwrap().name,
        "http_export"
    );

    let Some(Data::Sum(sum)) = &metric(&request, "jobs.done").data else {
        panic!("jobs.done isn't a sum");
    };
    assert!(sum.is_monotonic);
    assert_eq!(sum.aggregation_temporality, 2, "CUMULATIVE");
    let point = &sum.data_points[0];
    assert_eq!(point.value, Some(Number::AsInt(5)));
    assert_eq!(
        string_attr(&point.attributes, "queue"),
        Some(AnyValue::StringValue("email".into()))
    );
    assert!(point.start_time_unix_nano > 0 && point.start_time_unix_nano <= point.time_unix_nano);
    assert_eq!(point.exemplars[0].trace_id, cx.trace_id.to_bytes());
    assert_eq!(point.exemplars[0].span_id, cx.span_id.to_bytes());

    let Some(Data::Gauge(gauge)) = &metric(&request, "queue.depth").data else {
        panic!("queue.depth isn't a gauge");
    };
    assert_eq!(gauge.data_points[0].value, Some(Number::AsDouble(7.5)));

    let duration = metric(&request, "http.server.request.duration");
    assert_eq!(
        (duration.unit.as_str(), duration.description.as_str()),
        ("s", "Duration of HTTP server requests.")
    );
    let Some(Data::Histogram(histogram)) = &duration.data else {
        panic!("not a histogram");
    };
    assert_eq!(histogram.aggregation_temporality, 2, "CUMULATIVE");
    let point = &histogram.data_points[0];
    assert_eq!(point.count, 2);
    assert!((point.sum.unwrap() - 0.212).abs() < 1e-9);
    assert_eq!((point.min, point.max), (Some(0.012), Some(0.2)));
    assert_eq!(
        point.explicit_bounds.len(),
        14,
        "the conventions' duration buckets"
    );
    assert_eq!(point.bucket_counts.len(), 15);
    assert_eq!(point.bucket_counts.iter().sum::<u64>(), 2);
    assert_eq!(point.exemplars.len(), 2, "one per bucket used");
    assert!(
        point
            .exemplars
            .iter()
            .all(|e| e.trace_id == cx.trace_id.to_bytes())
    );
}

#[test]
fn the_overflow_series_is_marked_with_a_boolean() {
    let meters = MeterProvider::builder()
        .with_resource(resource())
        .with_cardinality_limit(2)
        .build();
    metrics::with_local_recorder(&meters.recorder(), || {
        for user in ["a", "b", "c"] {
            metrics::counter!("logins", "user" => user).increment(1);
        }
    });
    let request = oxtel_otlp::transform::metrics_request(&meters.collect(), &resource());
    let Some(oxtel_otlp::proto::metrics::v1::metric::Data::Sum(sum)) =
        &metric(&request, "logins").data
    else {
        panic!("not a sum");
    };
    let overflow: Vec<_> = sum
        .data_points
        .iter()
        .filter_map(|p| string_attr(&p.attributes, "otel.metric.overflow"))
        .collect();
    assert_eq!(overflow, vec![AnyValue::BoolValue(true)]);
}

#[test]
fn counters_beyond_i64_become_doubles() {
    use oxtel_otlp::proto::metrics::v1::number_data_point::Value as Number;
    let meters = meter_provider();
    metrics::with_local_recorder(&meters.recorder(), || {
        metrics::counter!("bytes.total").absolute(u64::MAX);
    });
    let request = oxtel_otlp::transform::metrics_request(&meters.collect(), &resource());
    let Some(oxtel_otlp::proto::metrics::v1::metric::Data::Sum(sum)) =
        &metric(&request, "bytes.total").data
    else {
        panic!("not a sum");
    };
    assert_eq!(
        sum.data_points[0].value,
        Some(Number::AsDouble(u64::MAX as f64))
    );
}

#[test]
fn partial_success_counts_rejected_data_points() {
    let reply = ExportMetricsServiceResponse {
        partial_success: Some(ExportMetricsPartialSuccess {
            rejected_data_points: 3,
            error_message: "out of order".into(),
        }),
    }
    .encode_to_vec();
    let (base, _) = receiver(vec![(200, vec![], reply)]);
    let exporter = OtlpHttpExporter::builder().with_endpoint(&base).build();
    let (families, _) = metric_families(&meter_provider());
    MetricExporter::export(&exporter, &families, &resource()).unwrap();
    assert_eq!(exporter.rejected(Signal::Metrics), 3);
}

#[test]
fn the_meter_provider_pushes_over_otlp() {
    let (base, seen) = receiver(vec![(200, vec![], vec![]), (200, vec![], vec![])]);
    let meters = MeterProvider::builder()
        .with_resource(resource())
        .with_periodic_exporter_config(
            OtlpHttpExporter::builder().with_endpoint(&base).build(),
            PeriodicReaderConfig {
                interval: Duration::from_secs(3600),
                timeout: Duration::from_secs(5),
            },
        )
        .build();
    metrics::with_local_recorder(&meters.recorder(), || {
        metrics::counter!("jobs.done").increment(1);
    });
    meters.force_flush().unwrap();
    metrics::with_local_recorder(&meters.recorder(), || {
        metrics::counter!("jobs.done").increment(1);
    });
    drop(meters);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "a flush, then a final export when dropped");
    let last = ExportMetricsServiceRequest::decode(seen[1].body.as_slice()).unwrap();
    let Some(oxtel_otlp::proto::metrics::v1::metric::Data::Sum(sum)) =
        &metric(&last, "jobs.done").data
    else {
        panic!("not a sum");
    };
    assert_eq!(
        sum.data_points[0].value,
        Some(oxtel_otlp::proto::metrics::v1::number_data_point::Value::AsInt(2))
    );
}

#[test]
fn json_bodies_carry_the_same_request() {
    let reply = br#"{"partialSuccess":{"rejectedSpans":"1","errorMessage":"late"}}"#.to_vec();
    let (base, seen) = receiver(vec![(
        200,
        vec![("Content-Type", "application/json".into())],
        reply,
    )]);
    let exporter = OtlpHttpExporter::builder()
        .with_endpoint(&base)
        .with_protocol(Protocol::HttpJson)
        .build();
    let (batch, remote) = spans();
    SpanExporter::export(&exporter, &batch, &resource()).unwrap();
    assert_eq!(
        exporter.rejected(Signal::Traces),
        1,
        "read from the JSON reply"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].path, "/v1/traces");
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    let doc: serde_json::Value = serde_json::from_slice(&seen[0].body).unwrap();
    let spans = doc["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .unwrap();
    assert_eq!(spans.len(), 2);
    let server = spans.iter().find(|s| s["name"] == "GET /items").unwrap();
    assert_eq!(server["traceId"], remote.trace_id.to_string());
    assert_eq!(server["parentSpanId"], remote.span_id.to_string());
}

/// Runs in a child process with the logs protocol set to JSON.
fn export_with_a_json_logs_protocol() {
    let exporter = OtlpHttpExporter::builder().build();
    let (spans, _) = spans();
    SpanExporter::export(&exporter, &spans, &resource()).unwrap();
    let (records, _) = log_records();
    LogExporter::export(&exporter, &records, &resource()).unwrap();
}

#[test]
fn each_signal_takes_its_protocol_from_the_environment() {
    const CHILD: &str = "OXTEL_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        export_with_a_json_logs_protocol();
        return;
    }
    let (base, seen) = receiver(vec![(200, vec![], vec![]), (200, vec![], vec![])]);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "each_signal_takes_its_protocol_from_the_environment",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", &base)
        .env("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/json")
        .env_remove("OTEL_EXPORTER_OTLP_PROTOCOL")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0].header("content-type"),
        Some("application/x-protobuf")
    );
    assert_eq!(seen[1].path, "/v1/logs");
    assert_eq!(seen[1].header("content-type"), Some("application/json"));
    assert!(serde_json::from_slice::<serde_json::Value>(&seen[1].body).is_ok());
}
