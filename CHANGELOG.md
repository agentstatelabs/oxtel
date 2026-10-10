# Changelog

All notable changes to oxtel are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) from 1.0.

## [Unreleased]

### Changed
- **Nothing is exported until it's configured.** `oxtel::init()` used to send traces, metrics and logs to `localhost:4318` when no variable was set, as the specification's defaults say. Now a signal exports over OTLP once `OTEL_EXPORTER_OTLP_ENDPOINT` or its own endpoint is set, or when `OTEL_{TRACES,METRICS,LOGS}_EXPORTER` names `otlp`, which still means `localhost:4318` by default. A program that relied on the old default sets `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318`. Configuration files are unchanged.
- With no log pipeline and no span exporter, events are no longer kept as span events that nothing would export.
- **Logs are written locally by default**, to standard error: readable text when that's a terminal, flat JSON otherwise, so containers and log shippers get JSON with nothing to set. `OXTEL_LOG_FORMAT=none` or `Builder::without_local_logs` turns them off. A writer given to `with_local_logs_writer` gets flat JSON unless a format is chosen.

### Added
- **A startup line that says what was set up**, in plain words: where log lines go and in what format, and where traces, metrics and logs are sent, or that they aren't and which variable changes that. `install` logs it at `info` with the target `oxtel`, and `Guard::summary` and `Telemetry::summary` return it. Endpoint URLs are shown without credentials or query strings.
- `OtlpHttpExporter::protocol` and `Protocol::name`.
- **A logs-only build.** `oxtel` with `default-features = false` writes local text, JSON or ECS logs and pulls in no HTTP or TLS crates; CI checks the dependency tree. [`examples/logs.rs`](examples/logs.rs) shows structured logs with no collector and no configuration.
- `OXTEL_LOG_FORMAT=auto`, the new default.
- `OXTEL_LOG_OUTPUT=stderr|stdout` and `Builder::with_local_logs_output(LogOutput::Stdout)`, to move local logs to standard output. Standard error stays the default, since standard output is the protocol channel for MCP servers over stdio.
- A bad `OXTEL_LOG_FORMAT` or `OXTEL_LOG_OUTPUT` is now reported when a configuration file is in use, too.

## [0.1.0] - 2026-10-10

The first release. oxtel turns what Rust code already records with `tracing` and `metrics` into standard OpenTelemetry output: traces, metrics and logs over OTLP/HTTP, Prometheus and OpenMetrics scrapes, and local text or JSON logs. One call, `oxtel::init()`, sets it up from the standard `OTEL_*` variables or a declarative configuration file. Ready-made instrumentation covers HTTP (tower, axum, hyper) and MCP (rmcp).

What the release claims is checked in CI:
- output against a real OpenTelemetry Collector, in protobuf and JSON;
- Prometheus output against Prometheus's own tools;
- configuration files against the published schema;
- names against the semantic conventions, generated with OpenTelemetry Weaver;
- every parser of outside input, with property tests and fuzzing.

Benchmarks against the official OpenTelemetry Rust stack are in `bench/RESULTS.md`. The API will change before 1.0. The MSRV is Rust 1.94.

| Crate | What it is |
|---|---|
| `oxtel` | One-call setup from the environment or a configuration file |
| `oxtel-sdk` | The data model, `tracing` layers, `metrics` recorder, processors, samplers, resources |
| `oxtel-otlp` | OTLP/HTTP export, protobuf or JSON |
| `oxtel-prometheus` | Prometheus and OpenMetrics exposition, and a `/metrics` endpoint |
| `oxtel-config` | Declarative configuration (`OTEL_CONFIG_FILE`) |
| `oxtel-semconv` | The semantic conventions as constants |
| `oxtel-tower` | HTTP server and client instrumentation |
| `oxtel-rmcp` | MCP server and client instrumentation |

### Added
- **`oxtel-semconv`: the semantic conventions as Rust constants**, generated with OpenTelemetry Weaver 0.26.1 from semantic-conventions v1.44.0 and, for `genai`, the GenAI and MCP conventions at a pinned commit of semantic-conventions-genai.
  - `attribute::HTTP_REQUEST_METHOD` and the rest are documented from the conventions themselves, including their well-known values and stability.
  - `metric::HTTP_SERVER_REQUEST_DURATION` and the rest are `Metric` values: name, instrument, unit, description.
  - `attribute::is_defined` and `metric::by_name` check names at run time.
  - Stable conventions are always present; those in development need the `experimental` feature. Deprecated ones are left out. The templates are oxtel's own.
  - `cargo run -p xtask -- gen-semconv` regenerates the crate. A new CI job, `semconv`, does so with the checksum-verified Weaver release and fails if the committed code differs.
  - New tests in `oxtel-tower` and `oxtel-rmcp` run real requests and check every attribute and metric label they record against the conventions, and every metric's instrument and unit.
- **Declarative configuration** (`oxtel-config`, and the facade's `config` feature, on by default). When `OTEL_CONFIG_FILE` names a file, oxtel builds every pipeline from it, and other `OTEL_*` variables count only where the file substitutes them.
  - Schema 1.x: `disabled`; `resource` (typed attributes, `attributes_list`, `schema_url`, `detection/development` detectors); `attribute_limits`; `propagator`; and per signal, limits, samplers (`always_on`, `always_off`, `trace_id_ratio_based`, `parent_based`), `batch` and `simple` processors, OTLP/HTTP and console exporters, periodic readers, the `prometheus/development` pull exporter, `exemplar_filter` and cardinality limits.
  - Environment substitution follows the spec: `${NAME}`, `${env:NAME}`, `${NAME:-default}` and `$$`, in values only. Unquoted values are typed by the YAML 1.2 core schema after substitution; quoted ones stay strings. A substituted value is never read as YAML structure.
  - Unknown keys and unsupported parts (views, gRPC, TLS files, delta temporality, non-default parent-based children) are reported as warnings, logged with the target `oxtel`. Wrong types, a missing or 2.x `file_format`, and malformed YAML are errors that name the field or line. `init()` returns `Error::Config` rather than starting misconfigured.
  - The configuration project's v1.2.0 examples are vendored and parsed in tests. CI checks `examples/otel-config.yaml` and those examples against the published schema. A fuzz target covers the parser.
- **OTLP/JSON** (`http/json`) for every signal, chosen by `OTEL_EXPORTER_OTLP_[SIGNAL_]PROTOCOL` or `with_protocol`. It follows the OTLP specification's JSON rules:
  - lowerCamelCase field names;
  - trace and span ids in hex, not base64;
  - enums as integers, and 64-bit integers as strings;
  - `NaN` and infinities as the protobuf mapping's strings;
  - default values left out.

  The encoder is oxtel's own, built from the same request structs as protobuf, so there's no new dependency. Partial-success counts are read from JSON replies too. Collector conformance runs every signal in both encodings.
- **Resource detection** (`oxtel_sdk::resource`). The default resource now describes where the process runs. What's detected ranks below `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_SERVICE_NAME` and the builder.
  - `service.instance.id`: a random UUID per process start.
  - `host.name`, `host.arch` and, on Linux, `host.id` (the machine id).
  - `os.type` and, on Linux, `os.name`, `os.version` and `os.description` from os-release.
  - `process.pid`, `process.parent_pid`, `process.executable.name` and `.path`. The command line isn't recorded; it may carry secrets.
  - `container.id` from `/proc/self/cgroup`, as Docker, containerd, CRI-O and Podman name it under cgroup v1 or v2, or from `/proc/self/mountinfo` when a cgroup namespace hides it.
  - Inside Kubernetes: `k8s.namespace.name` from the service account, `k8s.pod.name`, `k8s.pod.uid` from the cgroup path, and `k8s.node.name` from `K8S_NODE_NAME`.
  - Detectors read files and the environment only, never spawning processes or calling the C library. `OTEL_EXPERIMENTAL_RESOURCE_DETECTORS` (`service`, `host`, `os`, `process`, `container`, `k8s`, or `none`) chooses which run.
  - `ResourceBuilder::with_detector` and the facade's `with_resource_detector` add a detector of your own, such as a cloud provider's.
  - `without_env()` now also skips detection, so a resource built that way is exactly what the code says.
- **`oxtel-rmcp`: MCP instrumentation for rmcp**, following the OpenTelemetry MCP conventions (development status).
  - `Instrumented::new(server)` wraps any rmcp server. Each request and notification gets a span named `{mcp.method.name} {target}` (`tools/call commit`), continuing the caller's trace from W3C keys in `params._meta`.
  - Span attributes: `mcp.method.name`, `jsonrpc.request.id`, `mcp.protocol.version`, `gen_ai.tool.name` with `gen_ai.operation.name` `execute_tool`, `gen_ai.prompt.name`, `mcp.resource.uri` and `network.transport`. Over HTTP, also `mcp.session.id` and `network.protocol.*`.
  - A tool result with `isError` sets `error.type` `tool_error`. JSON-RPC error codes go in `rpc.response.status_code`. The caller's mistakes (parse errors, unknown methods or tools, invalid params, unknown resources) aren't marked as server failures; other codes set `error.type` and status Error with the message.
  - `mcp.server.operation.duration` per request, and `mcp.server.session.duration` when a session ends.
  - With the `client` feature, `client::send` and `client::call_tool` send requests in client spans, writing `traceparent` into `params._meta`, and record `mcp.client.operation.duration`.
  - Tool arguments and results (`gen_ai.tool.call.arguments`, `.result`) are recorded only with `with_payloads(true)`, cut at 16 KiB.
- **`oxtel-tower`: HTTP instrumentation** for tower services, and so axum and hyper, following the semantic conventions.
  - `HttpServerLayer`:
    - a span per request named `{method} {route}`, continuing the caller's W3C trace (switchable off at trust boundaries);
    - `http.request.method` (`_OTHER` for unknown methods, with the original), `url.path`, `url.scheme`, `http.route`, `server.address`/`port`, `network.protocol.version`, `user_agent.original` and `http.response.status_code`;
    - `error.type` and status Error for 5xx;
    - `http.server.request.duration` and `http.server.active_requests`, labelled by route rather than path so series stay bounded. Cancelled requests are uncounted too.
    - With the `axum` feature, the route comes from `MatchedPath`. `with_route` serves other routers. `with_access_log` adds an event per request, at `error` for 5xx. `with_span` with `server_span!` lets a program declare fields of its own for later middleware to record.
  - `HttpClientLayer`: client spans, `traceparent` sent with each request, `error.type` for 4xx, 5xx and failures, and `http.client.request.duration`. `url.full` leaves out credentials and the query.
  - `MetricsService`: Prometheus `/metrics` as a tower service, with per-scrape format negotiation.
- **Local log output** (`oxtel_sdk::logs::LocalLayer`): each event as a line, on standard error or a writer of your choice, carrying its span's trace and span ids.
  - `text`: like `tracing`'s own output: time, level, the span chain with its fields, target, message and fields.
  - `json`: one flat object per line: `timestamp`, `level`, `target`, `message`, `trace_id`, `span_id`, then every enclosing span's fields (inner wins) and the event's, under their names as written. A field named like a fixed key is written as `fields.<name>`, so lines never repeat a key.
  - `ecs`: the ecs-logging layout (`@timestamp`, `log.level`, `message`, `ecs.version` first), with `log.logger`, `log.origin.*`, `trace.id`, `span.id` and the resource's `service.name`, `service.version` and `service.environment`.
  - JSON is written by oxtel itself, with no serde dependency; it stays valid for any value, with control characters escaped and non-finite numbers written as strings.
  - The facade adds it with `with_local_logs` (and `_filter`, `_writer`), or `OXTEL_LOG_FORMAT`. It keeps working when `OTEL_SDK_DISABLED` turns everything else off.
- **`oxtel`: one call sets everything up.** `oxtel::init()` builds traces, metrics and logs from the standard environment and installs them as the global `tracing` subscriber and `metrics` recorder. With the `log` feature (on by default), it also forwards the `log` crate's records.
  - `OTEL_SDK_DISABLED`, `OTEL_{TRACES,METRICS,LOGS}_EXPORTER` (`otlp`, `console`, `prometheus` for metrics, `none`), `OTEL_PROPAGATORS` and `OTEL_EXPORTER_PROMETHEUS_HOST`/`_PORT` are honoured. Unsupported values, such as `zipkin`, `b3` or the `grpc` protocol, are logged as warnings with the target `oxtel`, not errors.
  - Spans and log records are filtered by `RUST_LOG`-style directives (`info` by default), separately for each. The log layer sees the spans the trace layer records, so every record carries its span's ids.
  - With a log pipeline, events become log records; without one, span events.
  - `oxtel::builder()` adds a default service name, resource attributes, a sampler, filters, layers of your own, and hooks into each pipeline for extra exporters and processors, for distributions. `build()` returns the pipelines without installing them.
  - The guard shuts every pipeline down at once and waits at most 5 s (configurable), so an unreachable collector no longer delays exit by each pipeline's timeout in turn. Without a collector, the quick start used to take 14 s to exit; now it takes 5.
  - `oxtel::propagation` injects the current span's context and extracts a caller's, as `OTEL_PROPAGATORS` allows.
- **`oxtel-sdk`: the `console` exporter**, one readable line per span, log record or metric series, for development.
- **`oxtel-prometheus`: `PrometheusServer`**, a `/metrics` endpoint on its own thread using only the standard library, for programs without an HTTP server. It negotiates the format per scrape, answers `HEAD`, limits request heads to 8 KiB, and times out silent clients.
- **`oxtel-sdk`: periodic metric export.** `MeterProviderBuilder::with_periodic_exporter` exports through a `MetricExporter` from a background thread every `OTEL_METRIC_EXPORT_INTERVAL` (60 s), and once more when the last clone of the provider is dropped or `shutdown` is called. Each export carries cumulative values; nothing is sent when nothing has been recorded. `force_flush`, `shutdown` and `stats` (data points exported and failed) work as for the other signals. A provider can have several exporters, and Prometheus scrapes still read the same store.
- **`oxtel-otlp`: metrics over OTLP/HTTP.** Counters become monotonic cumulative sums (totals beyond `i64` become doubles), gauges become gauges, and histograms keep their buckets, min, max and one exemplar per bucket. The overflow series' `otel.metric.overflow` attribute is a boolean, as the specification defines. Checked against a real Collector in CI.
- **`oxtel-sdk`: oxtel's own metrics** (`oxtel_sdk::sdk_metrics`), off by default. When turned on, batch processors, the periodic reader and the OTLP exporter record `otel.sdk.*` metrics named by the semantic conventions: items processed and dropped (`error.type` = `queue_full` or `shutdown`), items exported, failed (`error.type` = the HTTP status, `timeout` or `transport`) or rejected, and the duration of each export and collection. Exporters report their server's address and port, without any credentials in the URL. Other exporters can record the same metrics with `ExporterMetrics`.
- **`oxtel-sdk`: logs from `tracing` events.**
  - `LogLayer`, from `LoggerProvider::layer`. Each event becomes an OpenTelemetry log record: the level becomes the severity, `message` the body, other fields attributes, and the target the instrumentation scope. A name given with `name:` becomes the event name.
  - Inside a span, a record carries the span's trace and span ids, even when the span isn't sampled.
  - Events forwarded from the `log` crate by `tracing-log` keep their original target.
  - Attribute limits from `OTEL_LOGRECORD_ATTRIBUTE_COUNT_LIMIT`; source file and line as `code.*` attributes on request.
  - Batch, simple and no-op processors. The batch processor shares the span processor's worker, configured from `OTEL_BLRP_*`.
  - **Events inside spans:** a tracer provider's `with_span_events(false)` sends events out as log records only, as OpenTelemetry now recommends.
- **`oxtel-otlp`: logs over OTLP/HTTP**, checked against a real Collector in CI.
- **Log records keep up to four attributes inline**, so a log event makes no heap allocation beyond copying its message and string values. That's the same as the official stack: a bare event costs the same, and an event inside a span is 1.6× faster (`bench/RESULTS.md`).
- **`oxtel-sdk`: traces from `tracing` spans.**
  - `TraceLayer`, a `tracing-subscriber` layer. Span fields become attributes. `otel.name`, `otel.kind`, `otel.status_code` and `otel.status_description` set the name, kind and status. Events inside a span become span events, and `follows_from` becomes a link.
  - A sampler decides when a span starts. Unsampled spans keep their ids, so logs and outgoing requests can still carry them.
  - Samplers: always on, always off, consistent trace-id ratio (using the trace id's 56 random bits) and parent-based, configured from `OTEL_TRACES_SAMPLER` and `OTEL_TRACES_SAMPLER_ARG`.
  - Processors:
    - a batch processor with a bounded queue, which drops and counts spans when full instead of blocking;
    - a simple processor;
    - a no-op processor.
  - Span limits from `OTEL_SPAN_*_COUNT_LIMIT`.
  - The resource reads `OTEL_SERVICE_NAME` and `OTEL_RESOURCE_ATTRIBUTES`. A program can set a default service name that operators can still override.
  - `oxtel_sdk::suppress` keeps export work from tracing itself.
- **`oxtel-sdk`: W3C Trace Context and Baggage.**
  - Parsing and injection of `traceparent`, `tracestate` and `baggage`, strict where the specs require.
  - Carriers for `HashMap` and, with the `http` feature, `http::HeaderMap`.
  - `attach_remote_parent` makes an extracted context the parent of the next span.
- **`oxtel-sdk`: metrics from the `metrics` facade.**
  - `MeterProvider`'s recorder stores counters, gauges and explicit-bucket histograms. Registration uses the facade's precomputed key hashes across 16 shards; recording through a handle is an atomic operation.
  - Labels given in any order land in the same series.
  - Each metric keeps at most `cardinality_limit` series (default 2000), counting the `otel.metric.overflow` series that takes the rest.
  - Histograms in seconds get the semantic conventions' duration buckets, others the specification's defaults. Buckets can be set per metric.
  - **Counters adapt to contention.** A counter stays one atomic until a compare-and-swap fails, which shows another thread is updating it at that moment. It then spreads across per-thread slots, each on its own cache line, and reading sums them. Eight threads on one series cost 0.4 ns per increment, against 20–58 ns for the official SDK. An uncontended counter keeps a single atomic.
  - A counter's value has its own cache line, and recording skips the exemplar check entirely when exemplars are off. Benchmarking showed cached-handle increments 22% faster and contended ones 24% faster as a result.
  - **Exemplars:** one per histogram bucket, and the latest per counter, linking a measurement to the trace it was made in. `OTEL_METRICS_EXEMPLAR_FILTER` chooses `trace_based` (the default: sampled spans only), `always_on` or `always_off`. Exemplars are skipped, not waited for, under contention.
  - `current_span_context()` is now cheap enough for hot paths, and finds the nearest recorded span when the current one is filtered out of traces.
- **Fuzzing and property tests for every parser of outside input:** `traceparent`, `tracestate`, `baggage`, extraction from carriers, `OTEL_RESOURCE_ATTRIBUTES` and `OTEL_EXPORTER_OTLP_HEADERS`.
  - Property tests (proptest, in `cargo test`) check that no input panics; that valid headers parse and round-trip; that versions above `00` read as `00` and `ff` is rejected; that uppercase and all-zero ids are rejected; that the 32-member and 180-entry limits hold; and that baggage values survive percent-encoding.
  - Coverage-guided fuzz targets (`fuzz/`, cargo-fuzz) check the same invariants, 60 s per target in CI.
  - They found one bug before this release: a `traceparent` with multi-byte characters made the parser panic, because it sliced the header at byte offsets. It now rejects non-ASCII input first, as the spec requires.
- **Conformance against a real OpenTelemetry Collector** (`conformance/`, run in CI). Spans exported over OTLP/HTTP, plain and gzipped, are checked as the Collector decodes them: ids, parents, remote-parent flags, kinds, typed attributes, status, events and links. `/metrics` is checked as the Collector's Prometheus receiver scrapes it: names, types, labels, values, and exemplars carrying the trace id.
- **Benchmarks** (`bench/`, not published) against `tracing-opentelemetry` with `opentelemetry_sdk` 0.33, and against `metrics-exporter-prometheus`. The setups are matched, and end-to-end runs use isolated processes. Results and method are in `bench/RESULTS.md`.
  - Traces: 1.5–7× less overhead per operation.
  - Metrics: 1.8–2.8× faster with attributes on each call, and at parity or faster with cached handles.
  - End to end: 1.9× the export throughput at 2.5× less CPU per span, with memory bounded.
- **`oxtel-prometheus`: Prometheus and OpenMetrics exposition.**
  - OpenMetrics 1.0, with exemplars, `# UNIT` and `# EOF`; or the classic text format. Chosen from the scrape's `Accept` header.
  - Names follow OpenTelemetry's Prometheus compatibility rules:
    - unit suffixes from UCUM (`_seconds`, `_bytes`, `_bytes_per_second`);
    - `_total` on counters, `_ratio` on dimensionless gauges;
    - sanitised names; label keys that collide have their values joined with `;`.
  - The resource becomes `target_info`, and series carry `otel_scope_name`; both can be turned off.
  - CI checks the output with `promtool check metrics` and with prometheus-client's strict OpenMetrics parser.
- **`oxtel-otlp`: one exporter for every signal.** `OtlpHttpExporter` is cheap to clone, and clones share one connection pool. Each signal takes its own endpoint, headers, compression, timeout and encoding from `OTEL_EXPORTER_OTLP_*` and `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_*`. Each request's timeout is the time left in the export, so retries never overrun it.
- **`oxtel-otlp`: OTLP over HTTP with protobuf.**
  - Types generated from the official OTLP definitions, v1.11.1, with no `protoc` needed.
  - Endpoint, headers, compression and timeout from the `OTEL_EXPORTER_OTLP_*` variables.
  - gzip.
  - Retries with backoff for 429, 502, 503, 504 and connection failures, honouring `Retry-After`. Other failures aren't retried.
  - Rejected spans from partial-success responses are counted.

[Unreleased]: https://github.com/agentstatelabs/oxtel/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/agentstatelabs/oxtel/releases/tag/v0.1.0
