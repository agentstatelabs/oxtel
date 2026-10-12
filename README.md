# oxtel

OpenTelemetry-compatible telemetry for Rust, native to [`tracing`](https://docs.rs/tracing) and [`metrics`](https://docs.rs/metrics).

Structured logs with one line of setup, and OpenTelemetry traces, metrics and logs when you want them, without changing your code. Rust code already records spans, logs and metrics with `tracing` and `metrics`; oxtel turns that into JSON or text log lines, OTLP for collectors and backends, and Prometheus/OpenMetrics for scrapes. You don't add a second instrumentation API, and there's no bridge between two.

> **Status: 0.2, early.** Traces, logs and metrics work end to end, and are tested against a real OpenTelemetry Collector. The API will change before 1.0; see [What works today](#what-works-today) and the [roadmap](DESIGN.md#6-plan).

## Get started

You don't need to know OpenTelemetry to use oxtel. Start with logs, and add the rest when you want it.

### 1. Logs

```toml
[dependencies]
oxtel = "0.2"
tracing = "0.1"
```

```rust
fn main() -> Result<(), oxtel::Error> {
    // Keep this alive for as long as the program runs.
    let _telemetry = oxtel::init()?;

    tracing::info!(user.id = 42, plan = "team", "signed in");
    Ok(())
}
```

On a terminal you get text. Anywhere else, such as a container whose output goes to a log shipper, you get one JSON object per line, with each field as a key of its own:

```text
2026-10-10T22:53:13.416077Z  INFO app: signed in user.id=42 plan=team
```

```json
{"timestamp":"2026-10-10T22:52:59.743752Z","level":"INFO","target":"app","message":"signed in","user.id":42,"plan":"team"}
```

`RUST_LOG` chooses how much is logged (`info` by default). If logs are all you want, `default-features = false` leaves out oxtel's export code and every HTTP and TLS crate with it. [`examples/logs.rs`](examples/logs.rs) is this step, runnable.

### 2. Requests

Wrap work in a span, and every line inside it carries the span's fields and a trace id, so you can find everything one request did:

```rust
let request = tracing::info_span!("request", order.id = 7);
let _entered = request.enter();
tracing::warn!(attempt = 2, "payment provider slow; retrying");
// {..., "message":"payment provider slow; retrying","trace_id":"a98f5438…","span_id":"86faebde…","order.id":7,"attempt":2}
```

For a web service, add `oxtel-tower = { version = "0.2", features = ["axum"] }`. One line on an axum router gives every request a span, a log line, and duration metrics:

```rust
let app = Router::new()
    .route("/orders/{id}", get(order))
    .layer(oxtel_tower::HttpServerLayer::new());
axum::serve(listener, app)
    // On Ctrl-C, let main return, so what's pending is sent.
    .with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; })
    .await?;
```

```text
INFO GET /orders/{id}: shop: looking up order order.id=7 trace_id=bcbb1bb3…
INFO GET /orders/{id}: oxtel_tower::server: request http.response.status_code=200 http.server.request.duration=0.000041 trace_id=bcbb1bb3…
```

[`examples/web.rs`](examples/web.rs) is the whole service, runnable.

For an MCP server, wrap it: `oxtel_rmcp::Instrumented::new(server)`. Logs stay on standard error, out of the protocol's way.

### 3. Send it somewhere

Point oxtel at an OpenTelemetry Collector, or at any backend that accepts OTLP, and traces, metrics and logs go there. No code changes:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 OTEL_SERVICE_NAME=shop cargo run
```

To see traces on your own machine, run Jaeger and open http://localhost:16686:

```bash
docker run --rm -p 16686:16686 -p 4318:4318 jaegertracing/jaeger:latest
```

Nothing is sent anywhere until you set an endpoint. When oxtel starts, it logs one line saying what it set up, and if sending fails, it says so and what to check, so there's no guessing:

```text
INFO oxtel: log lines: standard error, as text; traces: http://localhost:4318/v1/traces (OTLP); metrics: http://localhost:4318/v1/metrics (OTLP); logs: http://localhost:4318/v1/logs (OTLP)
```

The [guide](https://agentstatelabs.github.io/oxtel/guide/) has [the few terms worth knowing](https://agentstatelabs.github.io/oxtel/guide/words/), [recipes](https://agentstatelabs.github.io/oxtel/guide/recipes/) (Prometheus, Elastic, hosted backends, MCP servers) and [what to do when nothing shows up](https://agentstatelabs.github.io/oxtel/guide/troubleshooting/).

## Settings

Everything is set from the environment, using the standard OpenTelemetry variables, so operators can change it without a rebuild. The ones you're most likely to need:

| Variable | What it does |
|---|---|
| `RUST_LOG` | how much is recorded: `info` (the default), `debug`, `warn`, or per module, such as `info,hyper=warn` |
| `OTEL_SERVICE_NAME` | your service's name, as backends will show it |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | where to send traces, metrics and logs, such as `http://localhost:4318`; nothing is sent until it's set |
| `OTEL_EXPORTER_OTLP_HEADERS` | headers to send, such as `Authorization=Bearer <token>` for a hosted backend |
| `OXTEL_LOG_FORMAT` | how local log lines look: `auto` (the default: text on a terminal, JSON otherwise), `text`, `json` (flat, OpenTelemetry field names), `ecs` (Elastic Common Schema) or `none` |
| `OXTEL_LOG_OUTPUT` | where local log lines go: `stderr` (the default) or `stdout` |

And the rest:

| Variable | What it does |
|---|---|
| `OTEL_TRACES_EXPORTER`, `OTEL_LOGS_EXPORTER` | `otlp` (the default once an endpoint is set), `console`, `none` |
| `OTEL_METRICS_EXPORTER` | `otlp` (the default once an endpoint is set), `prometheus` (serves `/metrics` on port 9464), `console`, `none` |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `_COMPRESSION`, `_TIMEOUT` | how OTLP is sent (`http/protobuf` or `http/json`), for all signals or each one (`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and so on) |
| `OTEL_RESOURCE_ATTRIBUTES` | more about the service, such as `deployment.environment.name=prod` |
| `OTEL_CONFIG_FILE` | a declarative configuration file (schema 1.x) to use instead of the variables; see [`examples/otel-config.yaml`](examples/otel-config.yaml) |
| `OTEL_SDK_DISABLED` | `true` turns it all off, except local logs |
| `OXTEL_MAPPINGS` | `none` turns off the spans and metrics oxtel makes from what libraries already log, such as sqlx's queries; `all` is the default |

oxtel follows the OpenTelemetry specification's variables and defaults with one deliberate exception: the specification sends to `localhost:4318` even when nothing is set, and oxtel sends nothing until an endpoint is set. Naming the exporter, as in `OTEL_TRACES_EXPORTER=otlp`, still gets the specification's default.

A program can also make these choices in code, with `oxtel::builder()`; its documentation says which ones the environment overrides. [`examples/quickstart.rs`](examples/quickstart.rs) uses the builder, and [`examples/manual.rs`](examples/manual.rs) assembles the pipelines by hand, as a distribution might.

## What works today

| | |
|---|---|
| **Setup** | `oxtel::init()` builds every pipeline from the standard environment and installs it. A builder adds resource attributes, filters, layers of your own, and hooks into each pipeline for extra exporters. Settings oxtel can't honour are logged as warnings rather than failing. Shutdown flushes every pipeline at once, within a deadline. |
| **Traces** | A `tracing` layer: span fields become attributes; `otel.name`, `otel.kind` and `otel.status_*` set the name, kind and status; events become span events. Sampling is decided when a span starts, and unsampled spans keep their ids. Batch export never blocks the program: when it's overloaded, it drops spans and counts them. |
| **Logs** | A `tracing` layer: each event becomes a log record. The level becomes the severity, `message` the body, other fields attributes, and the target the instrumentation scope. Inside a span, the record carries the span's trace and span ids, sampled or not. Events from the `log` crate, forwarded by `tracing-log`, keep their target. |
| **HTTP** | `oxtel-tower`: layers for tower, axum and hyper servers and clients, following the HTTP conventions. Server spans named by route, continuing the caller's W3C trace, with `http.server.request.duration` and `http.server.active_requests`. Client spans that carry the trace onward, with `http.client.request.duration`. Query strings and URL credentials are never recorded. Also a `/metrics` service for an axum router or hyper server. |
| **Databases** | `oxtel-sql`: a SQLite connection (rusqlite) instrumented in one line, through SQLite's own trace hooks, and a PostgreSQL client (tokio-postgres, or a deadpool-postgres pool) wrapped in one line where the connection is made. Every statement gets a client span named by its query summary (`SELECT users`, `INSERT orders SELECT carts`) and `db.client.operation.duration`, with the conventions' bucket bounds. The query text is recorded with every literal replaced by `?`. A failed PostgreSQL call carries its SQLSTATE, and a deadpool pool records the conventions' connection pool metrics: connections in use and idle, waiting callers, wait and use times. sqlx can be instrumented either way: with no code at all, as `oxtel::init()` turns the event sqlx writes for each query into the same span and metric (though without failures or which database it was), or completely, by wrapping sqlx 0.8's or 0.9's pool. The [databases guide](https://agentstatelabs.github.io/oxtel/guide/databases/) compares the two. |
| **MCP** | `oxtel-rmcp`: an rmcp server wrapped in one line, following the MCP conventions. Spans named `tools/call {tool}` continue the caller's trace from `params._meta`. Tool errors are recorded as `tool_error`, and JSON-RPC errors are classified as the conventions say. Records `mcp.server.operation.duration` and `mcp.server.session.duration`. Client helpers send the trace onward. Tool arguments and results only on request. |
| **Local logs** | On by default: each event as a line on standard error, beside or instead of export. Text when that's a terminal, flat JSON with fields under their OpenTelemetry names otherwise, or ECS JSON for Elastic. Lines carry their span's trace and span ids. |
| **Metrics** | A `metrics` recorder: counters, gauges, histograms, pushed over OTLP every `OTEL_METRIC_EXPORT_INTERVAL` or scraped by Prometheus, or both. Counters adapt to contention. Each metric has a cardinality limit, with an overflow series beyond it. Histograms in seconds get the semantic conventions' duration buckets. |
| **Exemplars** | Measurements made in a sampled span keep its trace and span ids (`OTEL_METRICS_EXEMPLAR_FILTER`). |
| **OTLP** | Traces, metrics and logs over OTLP/HTTP with protobuf or JSON bodies (`OTEL_EXPORTER_OTLP_PROTOCOL`), optionally gzipped, with retries as the OTLP spec defines them. One exporter serves every signal, each with its own endpoint, headers, compression and timeout. The types are generated from the official protocol definitions. |
| **Prometheus** | OpenMetrics 1.0, with exemplars, or the classic text format, negotiated per scrape. Names follow OpenTelemetry's Prometheus compatibility rules. Serve it from your own HTTP server, or from oxtel's built-in endpoint. |
| **Console** | Readable lines on standard output for spans, log records and metrics, for development. |
| **Propagation** | W3C Trace Context and Baggage, parsed strictly. |
| **Conventions** | `oxtel-semconv`: every attribute name and metric definition in the OpenTelemetry semantic conventions v1.44.0, plus the GenAI and MCP conventions, as Rust constants. Generated with OpenTelemetry Weaver; CI regenerates the code and checks it hasn't drifted. oxtel-tower's and oxtel-rmcp's tests check every attribute and metric they emit against these definitions. |
| **Config file** | OpenTelemetry declarative configuration (`OTEL_CONFIG_FILE`, schema 1.x): resource and detectors, propagators, sampler, limits, batch and simple processors, OTLP/HTTP and console exporters, periodic readers and the Prometheus endpoint. `${VAR:-default}` substitution is typed the way the spec says. Unsupported parts, such as views or gRPC, are reported as warnings. A file that can't be read stops `init()` with an error that names the field. |
| **Resources** | Detected at startup, below anything the environment sets: `service.instance.id`, the host's name, architecture and machine id, the OS, the process (but not its command line, which may hold secrets), the container id from cgroup v1 or v2, and the Kubernetes namespace, pod and node. `OTEL_EXPERIMENTAL_RESOURCE_DETECTORS` chooses which run. Distributions add their own detectors. |
| **Configuration** | The standard `OTEL_*` variables: `OTEL_SDK_DISABLED`, each signal's exporter, the resource, sampler, propagators, batching (`OTEL_BSP_*`, `OTEL_BLRP_*`), attribute limits, the metric export interval and timeout, the Prometheus endpoint, and each signal's OTLP endpoint, headers, compression and timeout. |
| **Self-observability** | When turned on, oxtel's own pipelines report what they did as `otel.sdk.*` metrics, named by the semantic conventions: items processed, dropped (and why), exported and rejected, and how long exports and collections take. |

**Next:** adoption in AgentStateGraph, then OpAMP, gRPC and more instrumentation. See the [design](DESIGN.md).

## How it compares

Against `tracing-opentelemetry` with the official `opentelemetry_sdk` 0.33, and `metrics-exporter-prometheus`. The setups are matched, the method is published, and the full results are in [`bench/RESULTS.md`](bench/RESULTS.md).

| | oxtel | Official stack |
|---|---|---|
| Span with 3 fields | 258 ns | 671 ns |
| Same, not sampled | 84 ns | 589 ns |
| Counter, attributes on each call | 13 ns | 23 ns |
| Counter, 8 threads on one series | 0.4 ns | 20–58 ns |
| Log event inside a span | 523 ns | 857 ns |
| End to end, spans exported per second | 158k | 83k |

## oxtel and eBPF

eBPF tools such as OpenTelemetry's [OBI](https://opentelemetry.io/docs/zero-code/obi/) trace requests from the Linux kernel, with no code changes, for every program on a machine. They see a program from the outside, where requests arrive and leave. oxtel works inside it: your spans and fields, log lines with trace ids, and the work between requests. The two complement each other. The [guide](https://agentstatelabs.github.io/oxtel/guide/ebpf/) explains how eBPF instrumentation works and how to run both.

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
