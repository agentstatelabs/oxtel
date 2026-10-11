---
title: When something's wrong
description: What to check, by symptom, when log lines are missing, nothing arrives at the collector, or there's too much output.
---

Start with what oxtel logs from the target `oxtel`. When it starts, one line says where log lines go and where traces, metrics and logs are sent, and any setting it couldn't use follows as a warning. When sending fails, a warning says where, why and what to check, at most once a minute per kind of telemetry, and another line says when sending works again. These lines appear in your local logs, but are never sent to the collector themselves.

## No log lines at all

- **The guard was dropped.** Write `let _telemetry = oxtel::init()?;`. `let _ = oxtel::init()` drops the guard at once, which shuts telemetry down.
- **The level is too high.** `RUST_LOG=warn` hides `info!` lines. Try `RUST_LOG=info`, or `debug`.
- **Something else set up logging first.** `oxtel::init()` returns an error if another `tracing` subscriber, such as `tracing_subscriber::fmt().init()`, is already installed. Remove the other one.
- **Local logs are off.** Check for `OXTEL_LOG_FORMAT=none` in the environment.

## Nothing arrives at the collector or backend

- **No endpoint is set.** The startup line says `not exported`. Set `OTEL_EXPORTER_OTLP_ENDPOINT`.
- **Wrong port.** oxtel speaks OTLP over HTTP, usually on port 4318. Port 4317 is OTLP over gRPC, which oxtel doesn't speak yet; it warns at startup if you point it there.
- **The receiver refuses it.** A warning such as `refused with status 401` means the credentials are wrong or missing: check `OTEL_EXPORTER_OTLP_HEADERS`. Status 404 means nothing receives data at that path.
- **Wrong path.** With the general endpoint, oxtel adds `/v1/traces`, `/v1/metrics` and `/v1/logs` itself. A signal's own endpoint, such as `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, is used exactly as given, so it needs the full path. The startup line shows the URLs in full.
- **Ctrl-C, or the program exits too soon.** Data is sent in batches, every few seconds. Keep the guard until the end of `main`: dropping it sends what's left, waiting up to 5 seconds. A server stopped with Ctrl-C never reaches the end of `main` unless it shuts down gracefully, as in the [web service recipe](/guide/recipes/#a-web-service).

## Too much output

Turn down what's noisy: `RUST_LOG=info,hyper=warn,h2=warn` keeps your own `info` lines and only the warnings of those crates.

## Logs in the wrong place, or the wrong shape

- Text in the container, or JSON on the terminal: set `OXTEL_LOG_FORMAT` to `json` or `text`.
- On standard error when you want standard output: `OXTEL_LOG_OUTPUT=stdout`.
- Every line twice: a second logging layer is installed beside oxtel's, or `OTEL_LOGS_EXPORTER=console` is also printing records to standard output.
