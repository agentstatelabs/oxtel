# oxtel

OpenTelemetry-compatible telemetry for Rust, native to [`tracing`](https://docs.rs/tracing) and [`metrics`](https://docs.rs/metrics).

Structured logs with one line of setup, and OpenTelemetry traces, metrics and logs when you want them, without changing your code. Rust code already records spans, logs and metrics with `tracing` and `metrics`; oxtel turns that into JSON or text log lines, OTLP for collectors and backends, and Prometheus scrapes. There's no second instrumentation API to learn, and no bridge between two.

**[The documentation site](https://agentstatelabs.github.io/oxtel/)** has the rest: scenarios to copy, from a command-line tool to an MCP server; a guide; and every setting, span and metric.

> **Status: 0.4, early.** Traces, logs and metrics work end to end, and are tested against a real OpenTelemetry Collector. The API will change before 1.0; the [roadmap](DESIGN.md#6-plan) says what comes next.

## Three steps

You don't need to know OpenTelemetry to use oxtel. Start with logs, and add the rest when you want it.

**1. Logs.** Add oxtel and `tracing`, and call `oxtel::init()` first thing in `main`:

```toml
[dependencies]
oxtel = "0.4"
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

On a terminal you get text. Anywhere else, such as a container whose output goes to a log shipper, you get one JSON object per line, with each field a key of its own:

```json
{"timestamp":"2026-10-10T22:52:59.743752Z","level":"INFO","target":"app","message":"signed in","user.id":42,"plan":"team"}
```

`RUST_LOG` chooses how much is logged: `info` by default.

**2. Requests.** For a web service, add `oxtel-tower = { version = "0.4", features = ["axum"] }`, and one line on an axum router gives every request a span, a log line and duration metrics:

```rust
let app = Router::new()
    .route("/orders/{id}", get(order))
    .layer(oxtel_tower::HttpServerLayer::new());
```

**3. Send it somewhere.** Point oxtel at an OpenTelemetry Collector, or at any backend that accepts OTLP, and traces, metrics and logs go there. The code doesn't change:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 OTEL_SERVICE_NAME=shop cargo run
```

Nothing is sent anywhere until an endpoint is set. [Get started](https://agentstatelabs.github.io/oxtel/start/) goes through each step, with programs you can run.

## What it works with

- **Frameworks and libraries:** axum, tower and hyper; reqwest; SQLite, PostgreSQL and sqlx; MCP servers and clients built with rmcp; calls to generative AI models; the tokio runtime. Each records what the OpenTelemetry semantic conventions define, under their names.
- **Destinations:** anything that receives OTLP over HTTP or gRPC, such as the OpenTelemetry Collector, Jaeger or a hosted backend; Prometheus scrapes; and JSON log lines for log pipelines, Elastic's included.
- **Programs of every kind,** async or not: services, background workers, command-line tools, and library crates that use `tracing` and `metrics` alone.

[What works with what](https://agentstatelabs.github.io/oxtel/reference/integrations/) has the whole list. [Every setting](https://agentstatelabs.github.io/oxtel/reference/settings/) says what the environment can change, and [every span and metric](https://agentstatelabs.github.io/oxtel/reference/telemetry/) what each crate records.

## Faster than the official stack

Measured against the official OpenTelemetry Rust SDK with `tracing-opentelemetry`, doing the same work on the same machine:

| | oxtel | Official stack |
|---|---|---|
| A span with 3 fields | 258 ns | 671 ns |
| A span that isn't sampled | 84 ns | 589 ns |
| A counter, with attributes on each call | 13 ns | 23 ns |
| A counter, 8 threads on one series | 0.4–0.6 ns | 20–65 ns |

[The benchmarks](bench/RESULTS.md) say how each was measured and made fair, with every number and how to reproduce them.

## Principles

- **No new instrumentation API.** Code instrumented with `tracing` and `metrics` works unchanged.
- **Standards on the wire.** Output mixes freely with any OpenTelemetry SDK, Collector or backend.
- **Never harm the host program.** Memory is bounded, nothing blocks the caller, and when full, data is dropped and counted. No unsafe code.
- **Private by default.** No message content or request bodies are recorded unless you opt in.
- **Lean by default.** Network and TLS dependencies are optional features.
- **Proven, not promised.** Benchmarks are published, and CI checks oxtel's output with a real [OpenTelemetry Collector](conformance/) and with Prometheus's own tools.

oxtel is not affiliated with the OpenTelemetry project. It implements the OpenTelemetry specifications and protocols.

## Contributing

Contributions are welcome under the Developer Certificate of Origin; see [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md). Report security issues privately; see [SECURITY.md](SECURITY.md).

## License

Apache License, Version 2.0. See [LICENSE](LICENSE).

---

Built by [AgentStateLabs](https://github.com/agentstatelabs), initially for [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph).
