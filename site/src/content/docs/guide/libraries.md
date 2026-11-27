---
title: Libraries oxtel instruments
description: The crates that add spans and metrics for HTTP servers and clients, databases, MCP, calls to generative AI models and the tokio runtime, each in about one line.
---

`oxtel::init()` records the log lines your code writes and the spans it opens with `tracing`. For the libraries your program uses, these crates add the spans and metrics the OpenTelemetry conventions define for them, so any backend shows them as it shows other programs'. Each crate records through `tracing` and `metrics`, so what it records goes wherever `oxtel::init()` sends the rest. A program builds only the ones it adds.

| Crate | For | What you add |
|---|---|---|
| `oxtel-tower` | HTTP servers and clients built on tower: axum and hyper | A layer: `.layer(oxtel_tower::HttpServerLayer::new())` |
| `oxtel-reqwest` | reqwest clients | One line where the client is made: `oxtel_reqwest::client(reqwest::Client::new())` |
| `oxtel-sql` | SQLite through rusqlite, PostgreSQL through tokio-postgres or deadpool-postgres, and sqlx | One line where the connection or pool is made |
| `oxtel-rmcp` | MCP servers and clients built with rmcp 3 | A wrapper around the server: `oxtel_rmcp::Instrumented::new(server)` |
| `oxtel-genai` | Calls to generative AI models, through any client | A few lines around each call |
| `oxtel-tokio` | The tokio runtime | One line at startup: `oxtel_tokio::record(&Handle::current())` |

They're all at version 0.4, like oxtel: `oxtel-reqwest = "0.4"`, for example. Call `oxtel::init()` before making their layers, clients and wrappers, so their duration metrics get their units and buckets.

## HTTP servers and clients: oxtel-tower

For axum, turn on its `axum` feature: `oxtel-tower = { version = "0.4", features = ["axum"] }`.

- **`HttpServerLayer`** gives every request a span named by its route, such as `GET /orders/{id}`, which continues the caller's trace. It writes a log line when the request finishes, and records `http.server.request.duration` and `http.server.active_requests`.
- **`HttpClientLayer`** gives every request a tower client sends, such as hyper's, a client span. It sends the trace along in the request's headers, and records `http.client.request.duration`.
- **`MetricsService`** serves Prometheus's `/metrics` from your own axum router or hyper server.

Query strings and credentials in URLs are never recorded. [A web service](/scenarios/web-service/) has a whole program.

## reqwest: oxtel-reqwest

`oxtel_reqwest::client` wraps a reqwest client in reqwest-middleware's `ClientWithMiddleware`, whose methods are reqwest's own, so the code that makes requests doesn't change. Every request gets the client span, trace headers and `http.client.request.duration` that oxtel-tower's client layer records. A request that gets no response is an error, with `error.type` saying why, such as `timeout` or `connect`.

For requests that leave your systems, don't send your trace ids along: add `oxtel_reqwest::HttpClientMiddleware::new().with_propagation(false)` to a client of your own instead. Its [documentation](https://docs.rs/oxtel-reqwest) shows how.

## Databases: oxtel-sql

Turn on the feature for your database library: `rusqlite`, `postgres`, `deadpool-postgres`, `sqlx-0.8` or `sqlx-0.9`. Every statement gets a client span named by what it does, such as `SELECT orders`, with its text minus the values, and its duration in `db.client.operation.duration`. A deadpool pool also records its connections in use and idle, and how long callers wait.

sqlx needs no code at all for the basics: `oxtel::init()` turns the event sqlx writes for each query into the same span and metric. [Databases](/guide/databases/) compares that with wrapping sqlx's pool, and [Database access](/scenarios/database/) has a whole program.

## MCP: oxtel-rmcp

`Instrumented` wraps an rmcp server. Every request gets a span named for what it asks, such as `tools/call add`, which continues the client's trace when the client sends one in `params._meta`. It records `mcp.server.operation.duration`, and `mcp.server.session.duration` when a session ends. With the `client` feature, `oxtel_rmcp::client::call_tool` and `send` make a client's requests in spans of their own, and send the trace along.

Tool arguments and results aren't recorded unless you ask, since they may hold personal or secret data.

It serves over standard input and output, or any other stream rmcp can serve over. rmcp 3's streamable HTTP server accepts only a `ServerHandler`, which `Instrumented` isn't yet, so a server served over HTTP can't be wrapped yet. A fix is planned.

## Generative AI: oxtel-genai

oxtel-genai works with any client for a model, local or hosted. Say what the call is when it starts, make the call inside its span, and end it with what came back:

- `oxtel_genai::chat("ollama", "llama3.2").start()` starts the span, named `chat llama3.2`, with the provider, the model and the request's settings.
- `.input_tokens(..).output_tokens(..).finish()` ends it with the tokens used, or `.fail("timeout")` with why it failed.

It records `gen_ai.client.inference.duration`, and token usage as counters, for totals and cost, and as histograms of each call's tokens. For a streamed response, it records the time to the first chunk and between chunks. Prompts and completions are never recorded. Its [documentation](https://docs.rs/oxtel-genai) has a whole example.

## The tokio runtime: oxtel-tokio

`let _runtime = oxtel_tokio::record(&Handle::current());` records, every ten seconds for as long as `_runtime` is kept:

- `oxtel.tokio.worker.count`, the worker threads;
- `oxtel.tokio.task.count`, the tasks spawned and not yet finished;
- `oxtel.tokio.global_queue.length`, the tasks waiting in the global queue: one that keeps growing means the workers can't keep up;
- `oxtel.tokio.utilization`, the share of the workers' time spent running tasks, from 0 to 1. Near 1, the runtime is saturated.

It records from a thread of its own, so it keeps going when the runtime is too busy to run anything else. Built with `--cfg tokio_unstable`, it also records the blocking pool, where `spawn_blocking` work runs. The OpenTelemetry conventions define no metrics for tokio, so these are named under `oxtel.`. It needs tokio 1.45 or later.

## The other crates

A program rarely adds these itself:

- **`oxtel-sdk`**: the tracer, meter and logger providers that `oxtel::init()` builds, for a program that assembles its pipelines by hand.
- **`oxtel-otlp`**: the OTLP exporters, over HTTP or gRPC, or as JSON lines to a file.
- **`oxtel-prometheus`**: the Prometheus exposition format, which oxtel's `/metrics` endpoint and oxtel-tower's `MetricsService` serve.
- **`oxtel-config`**: reads declarative configuration files.
- **`oxtel-semconv`**: the names in the OpenTelemetry semantic conventions, as Rust constants, for instrumentation of your own.
