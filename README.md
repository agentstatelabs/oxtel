# oxtel

OpenTelemetry-compatible telemetry for Rust, native to [`tracing`](https://docs.rs/tracing) and [`metrics`](https://docs.rs/metrics).

Rust code already records spans, logs and metrics with `tracing` and `metrics`. oxtel turns that into standard OpenTelemetry output: OTLP for collectors and backends, and Prometheus/OpenMetrics for scrapes. You don't add a second instrumentation API, and there's no bridge between two.

> **Status: early development (Phase 1 of the [design](DESIGN.md) is under way).** Traces, logs and metrics work end to end and are tested against a real OpenTelemetry Collector. The APIs will change before 0.1; see [What works today](#what-works-today) and the [roadmap](DESIGN.md#6-plan).

## Quick start

oxtel isn't on crates.io yet; 0.1 is planned for the end of Phase 1. The quick start is [`examples/quickstart.rs`](examples/quickstart.rs), which CI compiles:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Traces, metrics and logs, configured by the standard OTEL_* variables.
    // Keep the guard alive: dropping it exports what's pending.
    let _telemetry = oxtel::builder()
        .with_default_service_name("checkout")
        .install()?;

    // Your code, instrumented as usual.
    let span = tracing::info_span!("request", otel.kind = "server", http.route = "/items");
    let _entered = span.enter();
    tracing::info!(items = 3, "listed");
    metrics::histogram!("http.server.request.duration", "http.route" => "/items").record(0.042);
    Ok(())
}
```

By default everything goes over OTLP/HTTP to `localhost:4318`. The environment chooses otherwise:

| Variable | Choices |
|---|---|
| `OTEL_TRACES_EXPORTER`, `OTEL_LOGS_EXPORTER` | `otlp` (default), `console`, `none` |
| `OTEL_METRICS_EXPORTER` | `otlp` (default), `prometheus` (serves `/metrics` on port 9464), `console`, `none` |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `_HEADERS`, `_COMPRESSION`, `_TIMEOUT` | where and how OTLP is sent, for all signals or each one |
| `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES` | the resource |
| `OTEL_SDK_DISABLED` | `true` turns it all off, except local logs |
| `OXTEL_LOG_FORMAT` | local logs on standard error: `text`, `json` (flat, OpenTelemetry field names), `ecs` (Elastic Common Schema) or `none`; overrides the program's `with_local_logs` |
| `RUST_LOG` | which spans and events are recorded (`info` by default) |

Try it without a collector: `OTEL_TRACES_EXPORTER=console OTEL_LOGS_EXPORTER=console OTEL_METRICS_EXPORTER=console cargo run -p oxtel-examples --example quickstart`. To assemble the pipelines yourself, as a distribution might, see [`examples/manual.rs`](examples/manual.rs).

## What works today

| | |
|---|---|
| **Setup** | `oxtel::init()` builds every pipeline from the standard environment and installs it. A builder adds resource attributes, filters, layers of your own, and hooks into each pipeline for extra exporters. Settings oxtel can't honour are logged as warnings rather than failing. Shutdown flushes every pipeline at once, within a deadline. |
| **Traces** | A `tracing` layer: span fields become attributes; `otel.name`, `otel.kind` and `otel.status_*` set the name, kind and status; events become span events. Sampling is decided when a span starts, and unsampled spans keep their ids. Batch export never blocks the program: when it's overloaded, it drops spans and counts them. |
| **Logs** | A `tracing` layer: each event becomes a log record. The level becomes the severity, `message` the body, other fields attributes, and the target the instrumentation scope. Inside a span, the record carries the span's trace and span ids, sampled or not. Events from the `log` crate, forwarded by `tracing-log`, keep their target. |
| **HTTP** | `oxtel-tower`: layers for tower, axum and hyper servers and clients, following the HTTP conventions. Server spans named by route, continuing the caller's W3C trace, with `http.server.request.duration` and `http.server.active_requests`. Client spans that carry the trace onward, with `http.client.request.duration`. Query strings and URL credentials are never recorded. Also a `/metrics` service for an axum router or hyper server. |
| **MCP** | `oxtel-rmcp`: an rmcp server wrapped in one line, following the MCP conventions. Spans named `tools/call {tool}` continue the caller's trace from `params._meta`. Tool errors are recorded as `tool_error`, and JSON-RPC errors are classified as the conventions say. Records `mcp.server.operation.duration` and `mcp.server.session.duration`. Client helpers send the trace onward. Tool arguments and results only on request. |
| **Local logs** | Each event as a line on standard error, beside or instead of export: text for people, flat JSON with fields under their OpenTelemetry names, or ECS JSON for Elastic. Lines carry their span's trace and span ids. |
| **Metrics** | A `metrics` recorder: counters, gauges, histograms, pushed over OTLP every `OTEL_METRIC_EXPORT_INTERVAL` or scraped by Prometheus, or both. Counters adapt to contention. Each metric has a cardinality limit, with an overflow series beyond it. Histograms in seconds get the semantic conventions' duration buckets. |
| **Exemplars** | Measurements made in a sampled span keep its trace and span ids (`OTEL_METRICS_EXEMPLAR_FILTER`). |
| **OTLP** | Traces, metrics and logs over OTLP/HTTP with protobuf bodies, optionally gzipped, with retries as the OTLP spec defines them. One exporter serves every signal, each with its own endpoint, headers, compression and timeout. The types are generated from the official protocol definitions. |
| **Prometheus** | OpenMetrics 1.0, with exemplars, or the classic text format, negotiated per scrape. Names follow OpenTelemetry's Prometheus compatibility rules. Serve it from your own HTTP server, or from oxtel's built-in endpoint. |
| **Console** | Readable lines on standard output for spans, log records and metrics, for development. |
| **Propagation** | W3C Trace Context and Baggage, parsed strictly. |
| **Resources** | Detected at startup, below anything the environment sets: `service.instance.id`, the host's name, architecture and machine id, the OS, the process (but not its command line, which may hold secrets), the container id from cgroup v1 or v2, and the Kubernetes namespace, pod and node. `OTEL_EXPERIMENTAL_RESOURCE_DETECTORS` chooses which run. Distributions add their own detectors. |
| **Configuration** | The standard `OTEL_*` variables: `OTEL_SDK_DISABLED`, each signal's exporter, the resource, sampler, propagators, batching (`OTEL_BSP_*`, `OTEL_BLRP_*`), attribute limits, the metric export interval and timeout, the Prometheus endpoint, and each signal's OTLP endpoint, headers, compression and timeout. |
| **Self-observability** | When turned on, oxtel's own pipelines report what they did as `otel.sdk.*` metrics, named by the semantic conventions: items processed, dropped (and why), exported and rejected, and how long exports and collections take. |

**Coming next (Phase 1):**
- declarative configuration (`OTEL_CONFIG_FILE`);
- semantic conventions generated with Weaver.

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

- **Against a real OpenTelemetry Collector** in CI ([`conformance/`](conformance/)). Traces, metrics and logs sent over OTLP, and metrics scraped by its Prometheus receiver, must arrive intact, exemplars included.
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
