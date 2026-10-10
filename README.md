# oxtel

OpenTelemetry-compatible telemetry for Rust, native to [`tracing`](https://docs.rs/tracing) and [`metrics`](https://docs.rs/metrics).

Rust code already records spans, logs and metrics with `tracing` and `metrics`. oxtel turns that into standard OpenTelemetry output: OTLP for collectors and backends, and Prometheus/OpenMetrics for scrapes. You don't add a second instrumentation API, and there's no bridge between two.

> **Status: early development (Phase 1 of the [design](DESIGN.md) is under way).** Traces, logs and metrics work end to end and are tested against a real OpenTelemetry Collector. The APIs will change before 0.1; see [What works today](#what-works-today) and the [roadmap](DESIGN.md#6-plan).

## Quick start

Abridged from [`examples/quickstart.rs`](examples/quickstart.rs), which CI compiles. Run it with `cargo run -p oxtel-examples --example quickstart`.

```rust
use oxtel_otlp::OtlpHttpExporter;
use oxtel_prometheus::PrometheusExporter;
use oxtel_sdk::Resource;
use oxtel_sdk::logs::LoggerProvider;
use oxtel_sdk::metrics::MeterProvider;
use oxtel_sdk::trace::TracerProvider;
use tracing_subscriber::layer::SubscriberExt;

// Service name, resource attributes and sampler come from the standard
// OTEL_* variables; this name applies unless OTEL_SERVICE_NAME is set.
let resource = Resource::builder().with_default_service_name("quickstart").build();

// One OTLP/HTTP exporter, configured by OTEL_EXPORTER_OTLP_*, serves
// traces and logs. Events go out as log records carrying their span's ids.
let otlp = OtlpHttpExporter::builder().build();
let traces = TracerProvider::builder()
    .with_resource(resource.clone())
    .with_span_events(false)
    .with_batch_exporter(otlp.clone())
    .build();
let logs = LoggerProvider::builder()
    .with_resource(resource.clone())
    .with_batch_exporter(otlp)
    .build();
tracing::subscriber::set_global_default(
    tracing_subscriber::registry().with(traces.layer()).with(logs.layer()),
)?;

// Metrics: the `metrics` macros, recorded by oxtel.
let meters = MeterProvider::builder().build();
metrics::set_global_recorder(meters.recorder())?;

// Your code, instrumented as usual.
let span = tracing::info_span!("request", otel.kind = "server", http.route = "/items");
let _entered = span.enter();
tracing::info!(items = 3, "listed");
metrics::histogram!("http.server.request.duration", "http.route" => "/items").record(0.042);

// What a Prometheus scrape gets: OpenMetrics, with exemplars linking each
// histogram bucket to the trace that recorded it.
let (content_type, body) = PrometheusExporter::new(meters, resource)
    .render(Some("application/openmetrics-text"));
```

## What works today

| | |
|---|---|
| **Traces** | A `tracing` layer: span fields become attributes; `otel.name`, `otel.kind` and `otel.status_*` set the name, kind and status; events become span events. Sampling is decided when a span starts, and unsampled spans keep their ids. Batch export never blocks the program: when it's overloaded, it drops spans and counts them. |
| **Logs** | A `tracing` layer: each event becomes a log record. The level becomes the severity, `message` the body, other fields attributes, and the target the instrumentation scope. Inside a span, the record carries the span's trace and span ids, sampled or not. Events from the `log` crate, forwarded by `tracing-log`, keep their target. |
| **Metrics** | A `metrics` recorder: counters, gauges, histograms. Counters adapt to contention. Each metric has a cardinality limit, with an overflow series beyond it. Histograms in seconds get the semantic conventions' duration buckets. |
| **Exemplars** | Measurements made in a sampled span keep its trace and span ids (`OTEL_METRICS_EXEMPLAR_FILTER`). |
| **OTLP** | Traces and logs over OTLP/HTTP with protobuf bodies, optionally gzipped, with retries as the OTLP spec defines them. One exporter serves every signal, each with its own endpoint, headers, compression and timeout. The types are generated from the official protocol definitions. |
| **Prometheus** | OpenMetrics 1.0, with exemplars, or the classic text format, negotiated per scrape. Names follow OpenTelemetry's Prometheus compatibility rules. |
| **Propagation** | W3C Trace Context and Baggage, parsed strictly. |
| **Configuration** | The standard `OTEL_*` variables for the resource, sampler, batching (`OTEL_BSP_*`, `OTEL_BLRP_*`), attribute limits, and each signal's OTLP endpoint, headers, compression and timeout. |

**Coming next (Phase 1):**
- one-call setup;
- OTLP metrics;
- declarative configuration (`OTEL_CONFIG_FILE`);
- semantic conventions generated with Weaver;
- ready-made instrumentation for tower/axum and MCP (rmcp).

After that: OpAMP, gRPC, more instrumentation. See the [design](DESIGN.md).

## How it compares

Against `tracing-opentelemetry` with the official `opentelemetry_sdk` 0.33, and `metrics-exporter-prometheus`. The setups are matched, the method is published, and the full results are in [`bench/RESULTS.md`](bench/RESULTS.md).

| | oxtel | Official stack |
|---|---|---|
| Span with 3 fields | 258 ns | 671 ns |
| Same, not sampled | 84 ns | 589 ns |
| Counter, attributes on each call | 13 ns | 23 ns |
| Counter, 8 threads on one series | 0.4 ns | 20–58 ns |
| End to end, spans exported per second | 158k | 83k |

## How it's checked

- **Against a real OpenTelemetry Collector** in CI ([`conformance/`](conformance/)). Traces and logs sent over OTLP, and metrics scraped by its Prometheus receiver, must arrive intact, exemplars included.
- **Against Prometheus's own tools.** `promtool` checks the text format, and a strict OpenMetrics parser checks OpenMetrics.
- **Fuzzing and property tests** cover every parser of outside input ([`fuzz/`](fuzz/)).

## Principles

- **No new instrumentation API.** Code instrumented with `tracing` and `metrics` works unchanged.
- **Standards on the wire.** Output mixes freely with any OpenTelemetry SDK, Collector or backend.
- **Never harm the host program.** Memory is bounded, nothing blocks the caller, and when full, data is dropped and counted. No unsafe code.
- **Private by default.** No message content or request bodies are recorded unless you opt in.
- **Lean by default.** Network and TLS dependencies are optional features.
- **Proven, not promised.** Benchmarks and conformance results are published.

oxtel is not affiliated with the OpenTelemetry project. It implements the OpenTelemetry specifications and protocols.

## Contributing

Contributions are welcome under the Developer Certificate of Origin; see [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md). Report security issues privately; see [SECURITY.md](SECURITY.md).

## License

Apache License, Version 2.0. See [LICENSE](LICENSE).

---

Built by [AgentStateLabs](https://github.com/agentstatelabs), initially for [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph).
