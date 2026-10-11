# oxtel guide

This guide is for people who want useful logs and, later, traces and metrics, without first learning OpenTelemetry. The [README](../README.md) gets you started in three steps; this page fills in the rest.

- [The words worth knowing](#the-words-worth-knowing)
- [Recipes](#recipes)
- [When something's wrong](#when-somethings-wrong)

## The words worth knowing

You can use oxtel without any of these, but they're the words backends and colleagues will use.

| Word | What it means here |
|---|---|
| **Event**, or log line | One thing that happened: `tracing::info!(user.id = 42, "signed in")`. |
| **Field**, or attribute | A named value on an event or span, such as `user.id = 42`. In JSON, each is a key of its own, so you can search and filter by it. |
| **Span** | A piece of work with a start and an end, such as one request or one database query: `tracing::info_span!("request")`. Events inside it carry its fields. |
| **Trace** | All the spans of one request, across every service it touched. The **trace id** on each log line ties them together. |
| **Metric** | A number tracked over time, such as requests served or how long they took: `metrics::counter!("orders.placed").increment(1)`. |
| **Service name** | What backends call your program. Set it with `OTEL_SERVICE_NAME`, or in code with `oxtel::builder().with_default_service_name("shop")`. |
| **OTLP** | The OpenTelemetry protocol: how traces, metrics and logs travel to a collector or backend. oxtel speaks it over HTTP, usually on port 4318. |
| **Collector** | A program, usually the OpenTelemetry Collector, that receives OTLP and passes it on to one or more backends. Optional: you can send straight to a backend. |
| **Backend** | Where you look at the data: Jaeger, Grafana, Elastic, Honeycomb, Datadog and many more accept OTLP. |

## Recipes

### JSON logs in a container

Nothing to do: when standard error isn't a terminal, oxtel writes one JSON object per line, which every log shipper understands. To be explicit, set `OXTEL_LOG_FORMAT=json`. If your platform only collects standard output, set `OXTEL_LOG_OUTPUT=stdout`.

### Elastic

Either ship the log lines, or send everything over OTLP:

- **Log lines:** `OXTEL_LOG_FORMAT=ecs` writes the Elastic Common Schema, which Elastic Agent and Filebeat read without any mapping.
- **OTLP:** set `OTEL_EXPORTER_OTLP_ENDPOINT` to your deployment's OTLP endpoint, and `OTEL_EXPORTER_OTLP_HEADERS` to the authorization header it gives you, such as `Authorization=ApiKey <key>`.

### A hosted backend

Hosted backends give you an endpoint and a key. Set both, and name your service:

```bash
OTEL_SERVICE_NAME=shop \
OTEL_EXPORTER_OTLP_ENDPOINT=https://otlp.example.com \
OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <token>" \
  ./shop
```

The startup line shows where everything goes; it never shows the headers.

### Prometheus

`OTEL_METRICS_EXPORTER=prometheus` serves your metrics at `http://localhost:9464/metrics` for Prometheus to scrape. In a container, also set `OTEL_EXPORTER_PROMETHEUS_HOST=0.0.0.0` so the scraper can reach it. To serve `/metrics` from your own axum router instead, see `oxtel_tower::MetricsService`.

### A web service

With `oxtel-tower` and its `axum` feature, one layer gives every request a span named by its route (`GET /orders/{id}`), continues the caller's trace if it sent one, logs a `request` line with the status and duration, and records `http.server.request.duration`:

```rust
let _telemetry = oxtel::init()?;
let app = Router::new()
    .route("/orders/{id}", get(order))
    .layer(oxtel_tower::HttpServerLayer::new());
axum::serve(listener, app)
    .with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; })
    .await?;
```

- Call `oxtel::init()` first, so the layer finds the metrics recorder.
- Shut down gracefully, as above. Otherwise Ctrl-C ends the process at once, before oxtel can send what's pending.
- `.with_access_log(false)` drops the `request` lines; `RUST_LOG=info,oxtel_tower=warn` keeps only those for failed requests.

[`examples/web.rs`](../examples/web.rs) is a whole service.

### An MCP server

Wrap the server with `oxtel_rmcp::Instrumented::new(server)` and serve the wrapper. Each tool call gets a span named `tools/call {tool}`, continuing the client's trace if it sent one. Log lines go to standard error by default, which keeps them out of a stdio server's protocol stream; don't set `OXTEL_LOG_OUTPUT=stdout` for one.

### Turning things off

- No log lines: `OXTEL_LOG_FORMAT=none`, or `.without_local_logs()` in code.
- Nothing at all except log lines: `OTEL_SDK_DISABLED=true`.
- One kind of telemetry: `OTEL_TRACES_EXPORTER=none`, `OTEL_METRICS_EXPORTER=none` or `OTEL_LOGS_EXPORTER=none`.

## When something's wrong

Start with what oxtel logs from the target `oxtel`. When it starts, one line says where log lines go and where traces, metrics and logs are sent, and any setting it couldn't use follows as a warning. When sending fails, a warning says where, why and what to check, at most once a minute per kind of telemetry, and another line says when sending works again. These lines appear in your local logs, but are never sent to the collector themselves.

### No log lines at all

- **The guard was dropped.** Write `let _telemetry = oxtel::init()?;`. `let _ = oxtel::init()` drops the guard at once, which shuts telemetry down.
- **The level is too high.** `RUST_LOG=warn` hides `info!` lines. Try `RUST_LOG=info`, or `debug`.
- **Something else set up logging first.** `oxtel::init()` returns an error if another `tracing` subscriber, such as `tracing_subscriber::fmt().init()`, is already installed. Remove the other one.
- **Local logs are off.** Check for `OXTEL_LOG_FORMAT=none` in the environment.

### Nothing arrives at the collector or backend

- **No endpoint is set.** The startup line says `not exported`. Set `OTEL_EXPORTER_OTLP_ENDPOINT`.
- **Wrong port.** oxtel speaks OTLP over HTTP, usually on port 4318. Port 4317 is OTLP over gRPC, which oxtel doesn't speak yet; it warns at startup if you point it there.
- **The receiver refuses it.** A warning such as `refused with status 401` means the credentials are wrong or missing: check `OTEL_EXPORTER_OTLP_HEADERS`. Status 404 means nothing receives data at that path.
- **Wrong path.** With the general endpoint, oxtel adds `/v1/traces`, `/v1/metrics` and `/v1/logs` itself. A signal's own endpoint, such as `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, is used exactly as given, so it needs the full path. The startup line shows the URLs in full.
- **Ctrl-C, or the program exits too soon.** Data is sent in batches, every few seconds. Keep the guard until the end of `main`: dropping it sends what's left, waiting up to 5 seconds. A server stopped with Ctrl-C never reaches the end of `main` unless it shuts down gracefully, as in the [web service recipe](#a-web-service).

### Too much output

Turn down what's noisy: `RUST_LOG=info,hyper=warn,h2=warn` keeps your own `info` lines and only the warnings of those crates.

### Logs in the wrong place, or the wrong shape

- Text in the container, or JSON on the terminal: set `OXTEL_LOG_FORMAT` to `json` or `text`.
- On standard error when you want standard output: `OXTEL_LOG_OUTPUT=stdout`.
- Every line twice: a second logging layer is installed beside oxtel's, or `OTEL_LOGS_EXPORTER=console` is also printing records to standard output.

## Further

- Every setting: the [README](../README.md#settings), and the [`oxtel` crate documentation](https://docs.rs/oxtel).
- A configuration file instead of variables: [`examples/otel-config.yaml`](../examples/otel-config.yaml).
- How it's built and where it's going: [DESIGN.md](../DESIGN.md).
