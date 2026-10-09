//! Matched setups of oxtel and the official OpenTelemetry Rust stack
//! (`tracing-opentelemetry` over `opentelemetry_sdk`), so the benchmarks
//! compare the same work. See `RESULTS.md` for what is measured and how.
//!
//! Both trace pipelines record the same things: span name and fields as
//! attributes, events as span events, parent links. `tracing-opentelemetry`
//! is told not to add what oxtel doesn't record (source locations, thread
//! ids, busy/idle timings), so neither does extra work the other skips.

use std::hint::black_box;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use opentelemetry::trace::TracerProvider as _;
use tracing::Dispatch;
use tracing_subscriber::layer::SubscriberExt;

// ─── Traces ─────────────────────────────────────────────────

/// The registry alone: what `tracing` costs before any layer.
pub fn baseline() -> Dispatch {
    Dispatch::new(tracing_subscriber::registry())
}

/// A span processor that drops each finished span. It isolates the cost on
/// the instrumented thread: recording, building the span, handing it over.
pub struct DiscardSpans;

impl oxtel_sdk::trace::SpanProcessor for DiscardSpans {
    fn on_end(&self, span: oxtel_sdk::trace::SpanData) {
        black_box(span);
    }
    fn force_flush(&self) -> Result<(), oxtel_sdk::trace::ExportError> {
        Ok(())
    }
    fn shutdown(&self) -> Result<(), oxtel_sdk::trace::ExportError> {
        Ok(())
    }
}

/// The same, for the official SDK.
#[derive(Debug)]
pub struct OtelDiscardSpans;

impl opentelemetry_sdk::trace::SpanProcessor for OtelDiscardSpans {
    fn on_start(&self, _span: &mut opentelemetry_sdk::trace::Span, _cx: &opentelemetry::Context) {}
    fn on_end(&self, span: opentelemetry_sdk::trace::SpanData) {
        black_box(span);
    }
    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _timeout: Duration) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }
}

/// oxtel's trace layer, sampling everything or nothing.
pub fn oxtel_traces(sampled: bool) -> (oxtel_sdk::trace::TracerProvider, Dispatch) {
    let sampler = if sampled {
        oxtel_sdk::trace::Sampler::AlwaysOn
    } else {
        oxtel_sdk::trace::Sampler::AlwaysOff
    };
    let provider = oxtel_sdk::trace::TracerProvider::builder()
        .with_resource(oxtel_sdk::Resource::builder().without_env().build())
        .with_sampler(sampler)
        .with_processor(DiscardSpans)
        .build();
    let dispatch = Dispatch::new(tracing_subscriber::registry().with(provider.layer()));
    (provider, dispatch)
}

/// `tracing-opentelemetry` over the official SDK, sampling everything or
/// nothing, matched to what oxtel records.
pub fn otel_traces(sampled: bool) -> (opentelemetry_sdk::trace::SdkTracerProvider, Dispatch) {
    let sampler = if sampled {
        opentelemetry_sdk::trace::Sampler::AlwaysOn
    } else {
        opentelemetry_sdk::trace::Sampler::AlwaysOff
    };
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_sampler(sampler)
        .with_span_processor(OtelDiscardSpans)
        .build();
    let layer = tracing_opentelemetry::layer()
        .with_tracer(provider.tracer("bench"))
        .with_location(false)
        .with_threads(false)
        .with_tracked_inactivity(false);
    let dispatch = Dispatch::new(tracing_subscriber::registry().with(layer));
    (provider, dispatch)
}

// ─── Metrics ────────────────────────────────────────────────

/// oxtel's recorder, with exemplars off unless asked for.
pub fn oxtel_meters(exemplars: bool) -> oxtel_sdk::metrics::MeterProvider {
    let filter = if exemplars {
        oxtel_sdk::metrics::ExemplarFilter::TraceBased
    } else {
        oxtel_sdk::metrics::ExemplarFilter::AlwaysOff
    };
    oxtel_sdk::metrics::MeterProvider::builder()
        .with_exemplar_filter(filter)
        .build()
}

/// `metrics-exporter-prometheus`, the most used `metrics` facade backend.
pub fn prometheus_recorder() -> metrics_exporter_prometheus::PrometheusRecorder {
    metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder()
}

/// A metric exporter that accepts and drops everything.
pub struct OtelDiscardMetrics;

impl opentelemetry_sdk::metrics::exporter::PushMetricExporter for OtelDiscardMetrics {
    async fn export(
        &self,
        _metrics: &opentelemetry_sdk::metrics::data::ResourceMetrics,
    ) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }
    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _timeout: Duration) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }
    fn temporality(&self) -> opentelemetry_sdk::metrics::Temporality {
        opentelemetry_sdk::metrics::Temporality::Cumulative
    }
}

/// The official SDK's meter provider. A reader is attached so instruments
/// aggregate (with none, they are no-ops); it exports hourly, so never
/// during a benchmark.
pub fn otel_meters() -> opentelemetry_sdk::metrics::SdkMeterProvider {
    let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(OtelDiscardMetrics)
        .with_interval(Duration::from_secs(3600))
        .build();
    opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_reader(reader)
        .build()
}

// ─── A local OTLP/HTTP receiver ─────────────────────────────

/// An OTLP/HTTP receiver on localhost that decodes each trace request,
/// counts its spans and replies 200, after `latency` (to model a slow
/// backend). Connections are kept alive, as real collectors do.
pub struct Receiver {
    /// `http://127.0.0.1:<port>`.
    pub base: String,
    /// Spans received.
    pub spans: Arc<AtomicU64>,
    /// Requests received.
    pub requests: Arc<AtomicU64>,
}

impl Receiver {
    /// Start one.
    pub fn start(latency: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let spans = Arc::new(AtomicU64::new(0));
        let requests = Arc::new(AtomicU64::new(0));
        let (s, r) = (Arc::clone(&spans), Arc::clone(&requests));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (s, r) = (Arc::clone(&s), Arc::clone(&r));
                std::thread::spawn(move || serve(stream, latency, &s, &r));
            }
        });
        Receiver {
            base,
            spans,
            requests,
        }
    }
}

fn serve(stream: std::net::TcpStream, latency: Duration, spans: &AtomicU64, requests: &AtomicU64) {
    use oxtel_otlp::proto::collector::trace::v1::ExportTraceServiceRequest;
    use prost::Message;
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut len = 0usize;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((k, v)) = header.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; len];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        if let Ok(request) = ExportTraceServiceRequest::decode(body.as_slice()) {
            let n: usize = request
                .resource_spans
                .iter()
                .flat_map(|rs| &rs.scope_spans)
                .map(|ss| ss.spans.len())
                .sum();
            spans.fetch_add(n as u64, Ordering::Relaxed);
        }
        requests.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(latency);
        let reply =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/x-protobuf\r\nContent-Length: 0\r\n\r\n";
        if writer.write_all(reply).is_err() {
            return;
        }
    }
}
