# oxtel

OpenTelemetry-compatible telemetry for Rust, native to [`tracing`](https://docs.rs/tracing) and [`metrics`](https://docs.rs/metrics).

> **Status: early development.** Nothing here is ready to use yet. The [design](DESIGN.md) describes what is being built and in what order.

## What it is for

Rust libraries and services already record spans, logs and metrics with `tracing` and `metrics`. oxtel turns that data into standard OpenTelemetry output, without a second instrumentation API and without a bridge:

- **Export:** traces, metrics and logs over OTLP, plus Prometheus and OpenMetrics with exemplars.
- **Propagation:** W3C Trace Context and Baggage.
- **Conventions:** OpenTelemetry semantic conventions, generated from the official registry.
- **Configuration:** the standard `OTEL_*` environment variables and the OpenTelemetry declarative configuration file.
- **Remote management:** an OpAMP client.
- **Ready-made instrumentation:** HTTP (tower, axum, hyper, reqwest), MCP, database clients, the tokio runtime and GenAI clients.

## Principles

- **No new instrumentation API.** Code instrumented with `tracing` and `metrics` works unchanged.
- **Standards on the wire.** Output mixes freely with any OpenTelemetry SDK, Collector or backend.
- **Never harm the host program.** Memory is bounded and nothing blocks the caller. When full, data is dropped and counted. No unsafe code.
- **Private by default.** No message content or request bodies are recorded unless you opt in.
- **Lean by default.** Network and TLS dependencies are optional features.
- **Proven, not promised.** Every release ships benchmarks and conformance results.

oxtel is not affiliated with the OpenTelemetry project. It implements the OpenTelemetry specifications and protocols.

## License

Apache License, Version 2.0. See [LICENSE](LICENSE).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Contributions are accepted under the Developer Certificate of Origin.
