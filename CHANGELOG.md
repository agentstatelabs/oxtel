# Changelog

All notable changes to oxtel are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) from 1.0.

## [Unreleased]

### Added
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
  - A counter's value has its own cache line, and recording skips the exemplar check entirely when exemplars are off. Benchmarking showed cached-handle increments 22% faster and contended ones 24% faster as a result.
  - **Exemplars:** one per histogram bucket, and the latest per counter, linking a measurement to the trace it was made in. `OTEL_METRICS_EXEMPLAR_FILTER` chooses `trace_based` (the default: sampled spans only), `always_on` or `always_off`. Exemplars are skipped, not waited for, under contention.
  - `current_span_context()` is now cheap enough for hot paths, and finds the nearest recorded span when the current one is filtered out of traces.
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
