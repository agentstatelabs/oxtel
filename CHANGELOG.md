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
- **`oxtel-otlp`: OTLP over HTTP with protobuf.**
  - Types generated from the official OTLP definitions, v1.11.1, with no `protoc` needed.
  - Endpoint, headers, compression and timeout from the `OTEL_EXPORTER_OTLP_*` variables.
  - gzip.
  - Retries with backoff for 429, 502, 503, 504 and connection failures, honouring `Retry-After`. Other failures aren't retried.
  - Rejected spans from partial-success responses are counted.
