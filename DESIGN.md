# oxtel: OpenTelemetry-compatible telemetry for Rust, native to `tracing` and `metrics`

_Written 2026-10-08. Phases 0 and 1 are built; 0.1 and 0.2 were released on 2026-10-10._

## 1. Why

Rust code is already instrumented. Download counts over the last 90 days:

- `tracing`: about 213 million, for spans and structured logs. tokio, hyper, axum, tower-http, sqlx and rmcp all emit through it.
- `metrics`: about 22 million, the common metrics facade.

Getting that telemetry out in OpenTelemetry's standard formats still takes a second API, `opentelemetry`, plus a bridge from `tracing` to it. The official SDK has done careful work, but in October 2026:

- every crate is pre-1.0, and each release breaks compatibility;
- traces are still Beta in the API, the SDK and the exporter;
- there is no implementation of declarative configuration, even though its spec has been stable since March 2026;
- Rust has little ready-made instrumentation that follows the semantic conventions.

oxtel takes a different route. It keeps the instrumentation crates already use and adds no new API. It turns `tracing` and `metrics` directly into standard OpenTelemetry output, so no bridge is needed.

## 2. Principles

1. **No new instrumentation API.**
   - Spans and logs come from `tracing`; metrics come from `metrics`.
   - A crate instrumented today works with oxtel unchanged.
2. **Standards on the wire, exactly:**
   - OTLP 1.x for traces, metrics and logs;
   - W3C Trace Context and Baggage;
   - OpenTelemetry semantic conventions, at a pinned version;
   - OpenMetrics and the Prometheus exposition formats;
   - OpenTelemetry declarative configuration 1.0;
   - OpAMP.
3. **Interoperable.** Our output can be mixed with any other OpenTelemetry SDK or collector. Nothing locks users in.
4. **Built for distributions.**
   - A vendor or platform can ship its own build without forking: defaults, processors, resource detectors and exporters plug in through public builder hooks.
5. **Never harm the host program.**
   - Memory is bounded and work happens off the hot path. When full, data is dropped (and counted), never blocking the caller.
   - Telemetry failures never surface as errors in the host.
   - `#![forbid(unsafe_code)]` in every crate that doesn't need FFI.
6. **Private by default.** No message content, request bodies or tool arguments are captured unless the user opts in, as the GenAI conventions require.
7. **Lean by default.** Network, TLS and gRPC dependencies sit behind cargo features. The core has no network crates.
8. **Prove it.** Benchmarks and conformance results are published with each release.

## 3. What it covers

| Area | Content |
|---|---|
| Traces | `tracing` spans become OpenTelemetry spans. Covers kind, status and links; span events from `tracing` events inside a span; and parents from remote context. |
| Logs | `tracing` events become OpenTelemetry log records, with trace context attached. Local formats: text, flat JSON with OpenTelemetry field names, and ECS JSON. |
| Metrics | `metrics` counters, gauges and histograms become OpenTelemetry instruments. Covers cumulative and delta temporality; explicit-bucket and exponential histograms; views; cardinality limits with an overflow series; and exemplars taken from the active span. |
| Context | W3C `traceparent`, `tracestate` and `baggage`; trace and span id generation; parent-based, ratio and always-on/off samplers. Unsampled spans still carry ids, so logs can always quote a trace id. |
| Export | OTLP over HTTP with protobuf, and with JSON. OTLP over gRPC as a feature. Prometheus/OpenMetrics pull endpoint, as a framework-independent handler plus axum and hyper helpers. stdout and in-memory exporters for development and tests. |
| Configuration | The standard `OTEL_*` variables, including `OTEL_SDK_DISABLED` and the `OTEL_{TRACES,METRICS,LOGS}_EXPORTER` selectors. A declarative config file via `OTEL_CONFIG_FILE`, using schema 1.0. A programmatic builder. Precedence follows the spec. One default differs on purpose: nothing is exported until an endpoint or exporter is configured, where the spec sends to `localhost:4318`. |
| Remote management | An OpAMP client. It reports the agent's description and health, and accepts remote configuration such as sampling, log level and enabled signals, applied live through reload handles. |
| Resources | Service, host, process, container and Kubernetes detection, then cloud. |
| Semantic conventions | A generated crate of attribute and metric definitions, built with OpenTelemetry Weaver from a pinned registry release. |
| Self-observability | Counts of dropped, queued and exported items and of export errors, available as metrics. |

### Ready-made instrumentation

Each follows the semantic conventions.

| Crate | Covers |
|---|---|
| `oxtel-tower` | HTTP server and client spans and metrics for tower, axum and hyper, with context extraction and injection. |
| `oxtel-reqwest` | HTTP client spans, with context injection. |
| `oxtel-rmcp` | MCP server and client spans and `mcp.*.operation.duration`, per the MCP conventions. Context travels through `params._meta`. |
| `oxtel-sql` | Database client spans and `db.client.*` metrics for rusqlite (through SQLite's own trace hooks, so call sites don't change), tokio-postgres and sqlx. Spans are named by the query summary the conventions define (`SELECT users`). The query text is recorded with every literal replaced by `?`, as the conventions advise, so values never reach telemetry; it can be turned off. |
| `oxtel-tokio` | tokio runtime metrics: workers, queue depth and blocking pool. |
| `oxtel-genai` | Helpers that record GenAI token usage and operation duration per the GenAI conventions, with no message content by default. |

Beyond these, oxtel may recognise popular crates' own `tracing` output and record it under the conventions' names, so their telemetry becomes standard with no code change. Library output isn't a stable interface, so each mapping would be tested against the versions it supports and pass events through unchanged when they don't match. The work starts with a survey of what crates emit, and includes proposing conventional names to the crates themselves.

## 4. Architecture

```
 tracing spans/events ──► oxtel Layer ──┬─► span processor (batch) ──► OTLP traces
                                        └─► log processor (batch)  ──► OTLP logs
                                                                   └─► local formatter (text/JSON/ECS)
 metrics facade ────────► oxtel Recorder ─► aggregation store ──┬─► periodic reader ──► OTLP metrics
                              ▲                                 └─► pull reader ──────► /metrics (OpenMetrics)
                              └── exemplars from the active span
 config: env + OTEL_CONFIG_FILE + builder ─► Pipeline ◄── OpAMP client (remote config, health)
```

- **The `tracing` layer.**
  - Keeps each span's data in the span's extensions.
  - Takes the sampling decision when the span starts; a span that isn't recorded keeps its ids but stores no data.
  - Builds the record when the span closes.
  - Filters are per layer, so the log level never switches off traces or metrics.
- **The `metrics` recorder.**
  - Keys instruments by name and attributes, in a lock-sharded store.
  - Records on the caller's thread, without locks on the hot path where possible.
  - Readers collect from the same store, so OTLP and Prometheus can never disagree.
- **Processors.**
  - Bounded queues and batch export run on a dedicated thread. Exporting doesn't depend on any async runtime.
  - A shutdown guard flushes on exit, which matters for short-lived and stdio programs.
- **Wire types.** Generated with prost from a pinned `opentelemetry-proto` 1.x release, since the protocol is stable. We don't depend on a crate that changes with every SDK release.
- **Prometheus output.**
  - Translates OpenTelemetry names: units and `_total` suffixes, `target_info`, scope labels.
  - Serves OpenMetrics 1.0 and the classic text format.
  - Supports Prometheus 3's negotiation for non-escaped (UTF-8) names.

### Crates

| Crate | Purpose | Network dependencies |
|---|---|---|
| `oxtel` | Setup facade: one `init()`, the builder, the shutdown guard | none |
| `oxtel-sdk` | Data model, layer, recorder, processors, samplers, resources | none |
| `oxtel-otlp` | OTLP exporters (`http-proto`, `http-json`, `grpc` features) | HTTP client and TLS, optional |
| `oxtel-prometheus` | OpenMetrics and Prometheus exposition | none (handler only) |
| `oxtel-config` | Environment and declarative config parsing, validated against schema 1.0 | none |
| `oxtel-opamp` | OpAMP client | WebSocket or HTTP, optional |
| `oxtel-semconv` | Generated convention constants | none |
| `oxtel-tower`, `-reqwest`, `-rmcp`, `-sql`, `-tokio`, `-genai` | Ready-made instrumentation | as needed |

## 5. Proof

- **Conformance.** CI runs against a real OpenTelemetry Collector in a container:
  - OTLP round trips for every signal and encoding;
  - `promtool check metrics` on the Prometheus output;
  - Weaver live-check of attributes against the conventions;
  - declarative config files validated against the published schema;
  - OpAMP against the reference server.
- **Benchmarks.** Published for each release, against `tracing-opentelemetry` plus the official SDK, on the same workloads:
  - span creation and close;
  - events inside spans;
  - metric recording, contended and uncontended;
  - export throughput;
  - memory under backpressure.
- **Fuzzing** of every parser that reads outside input: `traceparent`, `tracestate`, `baggage`, config files and OpAMP messages.
- **Policies.** An MSRV policy; semver from 1.0; a changelog that marks breaking changes.

## 6. Plan

| Phase | Scope | Gate |
|---|---|---|
| **0: spike** (about 2 weeks) | `tracing` → OTLP/HTTP traces with batching and backpressure. `metrics` → OpenMetrics with exemplars. W3C propagation. The first benchmark against the official SDK. | Overhead equal or better; design holds up. |
| **1: the first real user** (0.1–0.2) | All three signals over OTLP/HTTP. Prometheus/OpenMetrics. Environment config and declarative config. Resources and samplers. Generated conventions. `oxtel-tower` and `oxtel-rmcp`. Local formats (text, flat JSON, ECS). Collector conformance in CI. AgentStateGraph adopts it, with AgentStateDeveloper and CTXone following. | Running in production in those projects. |
| **2: breadth** (0.3–0.5) | OpAMP client. OTLP over gRPC. Exponential histograms, views and delta temporality. `oxtel-sql`, `oxtel-reqwest`, `oxtel-tokio`, `oxtel-genai`. Cloud resource detectors. Fuzzing. | Outside adopters. |
| **3: 1.0** | API freeze. Conformance suite and benchmarks published. Documentation site. Governance documents. Readiness review for donation. | Stable API. |

**Phase 0 results** (benchmarks and the gate) are in [`bench/RESULTS.md`](bench/RESULTS.md).

## 7. Governance

- **License and contributions.** Apache-2.0, with contributions signed off under the DCO.
- **Documents.** `CODE_OF_CONDUCT.md`, `SECURITY.md`, and a public roadmap.
- **Clean IP.** All code is written for this project. Nothing is copied from other SDKs.
- **Name.** Not "OpenTelemetry", which is a CNCF trademark. The project describes itself as OpenTelemetry-compatible.
- **Roles.** A contributor ladder modelled on OpenTelemetry's, from member to approver to maintainer.
- **Product-neutral.** Nothing in it is specific to AgentStateLabs' products.

## 8. Non-goals

- New instrumentation APIs. `tracing` and `metrics` are the APIs.
- A collector. Use the OpenTelemetry Collector or a vendor build of it.
- eBPF instrumentation. OpenTelemetry's OBI provides it, and it complements oxtel: it sees programs from outside, oxtel from inside. See the [guide](https://agentstatelabs.github.io/oxtel/guide/ebpf/).
- Profiling, until OpenTelemetry's profiles signal is stable.
- Browser or WASM telemetry, at first.

## 9. Open questions

1. **The name.**
2. **The MSRV.** Probably the latest stable release minus two.
3. **Events inside spans.** *Decided in Phase 1:* each event becomes a log record that carries the span's context, the OpenTelemetry direction. Span events stay available for backends that need them. The `oxtel` facade turns them off when it sets up a log pipeline, and leaves them on when there is none, so events aren't lost.
4. **The metrics API.** Is the `metrics` facade enough, or do we need an optional typed API for instruments it can't express, such as observable callbacks and units on every record?
5. **OpAMP.** Write our own client, or build on New Relic's `newrelic-opamp-rs` (small and active)?
