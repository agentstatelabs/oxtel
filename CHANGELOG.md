# Changelog

All notable changes to oxtel are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) from 1.0.

## [Unreleased]

### Changed
- **One OTLP exporter serves every signal.** `OtlpHttpExporter` is cheap to clone, and clones share one connection pool. Each signal resolves its own endpoint, headers, compression and timeout from the specification's `OTEL_EXPORTER_OTLP_*` and `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_*` variables. `with_endpoint(base)` sends every signal to `{base}/v1/{signal}`, and `with_url(signal, url)` sets one signal's URL exactly; these replace `with_url(url)`. `rejected(signal)` replaces `rejected_spans()`.
- Each OTLP request's timeout is now the time left in the export, so retries can't overrun the export timeout.
- `ExportError`, `ProcessorStats` and `BatchConfig` live in `oxtel_sdk::export`, shared by every signal. `oxtel_sdk::trace` still re-exports them.

### Fixed
- **A `traceparent` containing multi-byte characters made the parser panic.** It sliced the header at fixed byte offsets, which can fall inside a character. It now rejects non-ASCII input, as the spec requires, before slicing. Extraction through `http::HeaderMap` was never affected, because its non-ASCII values were already skipped. Other carriers, such as a `HashMap` or MCP `params._meta`, could reach the panic. Found by the new property tests.

### Added
- **`oxtel-sdk`: logs from `tracing` events.**
  - `LogLayer`, from `LoggerProvider::layer`. Each event becomes an OpenTelemetry log record: the level becomes the severity, `message` the body, other fields attributes, and the target the instrumentation scope. A name given with `name:` becomes the event name.
  - Inside a span, a record carries the span's trace and span ids, even when the span isn't sampled.
  - Events forwarded from the `log` crate by `tracing-log` keep their original target.
  - Attribute limits from `OTEL_LOGRECORD_ATTRIBUTE_COUNT_LIMIT`; source file and line as `code.*` attributes on request.
  - Batch, simple and no-op processors. The batch processor shares the span processor's worker, configured from `OTEL_BLRP_*`.
  - **Events inside spans:** a tracer provider's `with_span_events(false)` sends events out as log records only, as OpenTelemetry now recommends.
- **`oxtel-otlp`: logs over OTLP/HTTP**, checked against a real Collector in CI.
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
- **Ready to be public:**
  - a README that shows what works, a quick start (`examples/quickstart.rs`, compiled in CI) and the benchmark summary;
  - the Code of Conduct (Contributor Covenant 2.1, adopted by reference);
  - issue and pull request templates, and a DCO sign-off check on pull requests;
  - Dependabot, and CODEOWNERS.
- **Fuzzing and property tests for every parser of outside input:** `traceparent`, `tracestate`, `baggage`, extraction from carriers, `OTEL_RESOURCE_ATTRIBUTES` and `OTEL_EXPORTER_OTLP_HEADERS`.
  - Property tests (proptest, in `cargo test`) check that no input panics; that valid headers parse and round-trip; that versions above `00` read as `00` and `ff` is rejected; that uppercase and all-zero ids are rejected; that the 32-member and 180-entry limits hold; and that baggage values survive percent-encoding.
  - Coverage-guided fuzz targets (`fuzz/`, cargo-fuzz) check the same invariants, 60 s per target in CI.
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
- **`oxtel-otlp`: OTLP over HTTP with protobuf.**
  - Types generated from the official OTLP definitions, v1.11.1, with no `protoc` needed.
  - Endpoint, headers, compression and timeout from the `OTEL_EXPORTER_OTLP_*` variables.
  - gzip.
  - Retries with backoff for 429, 502, 503, 504 and connection failures, honouring `Retry-After`. Other failures aren't retried.
  - Rejected spans from partial-success responses are counted.
