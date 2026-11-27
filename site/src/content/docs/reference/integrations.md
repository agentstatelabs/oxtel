---
title: What works with what
description: The libraries, backends and platforms oxtel works with, how each is covered, and which OpenTelemetry conventions name what it records.
---

Whether oxtel covers what your program uses, at a glance. Every row marked **supported** ships today and is tested in CI; **planned** rows are on the [roadmap](https://github.com/agentstatelabs/oxtel/blob/main/DESIGN.md#6-plan).

The **conventions** column says whose names the telemetry uses. Where the OpenTelemetry semantic conventions define a library's spans and metrics, oxtel uses their names, so any backend shows them as it shows other languages'. **Stable** conventions don't change; conventions **in development** can, in a later release of the conventions, and oxtel follows them when they do.

## Your own code

| What | How | Status |
|---|---|---|
| `tracing` spans and events | `oxtel::init()`: spans become OpenTelemetry spans, and events log lines and log records | Supported |
| The `metrics` crate's counters, gauges and histograms | `oxtel::init()` installs the recorder | Supported |
| The `log` crate | Forwarded into `tracing` by oxtel's `log` feature, on by default | Supported |
| A library crate of your own | `tracing` and `metrics` alone, with no oxtel dependency: [a library crate](/scenarios/library/) | Supported |

## Libraries and frameworks

| Library | Crate | Conventions | Status |
|---|---|---|---|
| axum 0.8 | `oxtel-tower`, with its `axum` feature: [a web service](/scenarios/web-service/) | HTTP: stable, except `http.server.active_requests`, in development | Supported |
| tower and hyper servers | `oxtel-tower`'s `HttpServerLayer` | HTTP, as above | Supported |
| tower and hyper clients | `oxtel-tower`'s `HttpClientLayer` | HTTP: stable | Supported |
| reqwest 0.13 | `oxtel-reqwest`: [two services](/scenarios/two-services/) | HTTP: stable | Supported |
| SQLite, through rusqlite | `oxtel-sql`, with its `rusqlite` feature: [database access](/scenarios/database/) | Database: stable | Supported |
| PostgreSQL, through tokio-postgres | `oxtel-sql`, with its `postgres` feature | Database: stable | Supported |
| deadpool-postgres pools | `oxtel-sql`, with its `deadpool-postgres` feature | Database: stable; the pool's metrics in development | Supported |
| sqlx 0.8 and 0.9 | No code, from sqlx's own query events; or `oxtel-sql`'s `sqlx-0.8` or `sqlx-0.9` feature, for failures and the database too: [databases](/guide/databases/) | Database: stable | Supported |
| MCP servers and clients, through rmcp 3 | `oxtel-rmcp`: [over stdio](/scenarios/mcp-stdio/) and [over HTTP](/scenarios/mcp-http/) | MCP: in development | Supported |
| Calls to generative AI models, through any client | `oxtel-genai` | Generative AI: in development | Supported |
| The tokio runtime, 1.45 or later | `oxtel-tokio` | None defined, so named under `oxtel.` | Supported |

[Libraries oxtel instruments](/guide/libraries/) says what each crate records and the line that adds it.

## Where telemetry goes

oxtel speaks the open protocols rather than any backend's own, so it works with anything that accepts them.

| Where | How | Status |
|---|---|---|
| Anything that receives OTLP over HTTP, with protobuf or JSON: the OpenTelemetry Collector, or a backend that accepts OTLP directly | `OTEL_EXPORTER_OTLP_ENDPOINT`: [sending to a collector](/guide/sending/) | Supported. CI sends traces, metrics and logs to a real Collector and checks what arrives. |
| Anything that receives OTLP over gRPC | oxtel's `grpc` feature, and `OTEL_EXPORTER_OTLP_PROTOCOL=grpc` | Supported. CI does the same over gRPC. |
| Prometheus, or anything that scrapes its format | The built-in `/metrics` endpoint, or oxtel-tower's `MetricsService` in your own server: [Prometheus scraping only](/scenarios/prometheus/) | Supported. CI checks the output with Prometheus's own `promtool`. |
| Files, for the Collector to read later | The `otlp_file/development` exporter: OTLP/JSON lines to standard output or a file | Supported. CI checks the Collector's `otlpjsonfile` receiver reads them back. |
| Log pipelines that read JSON lines, including Elastic's | Local log lines as flat JSON, or ECS with `OXTEL_LOG_FORMAT=ecs`: [recipes](/guide/recipes/) | Supported |
| A terminal, while you develop | The `console` exporters, and local log lines as text | Supported |
| Zipkin's or Jaeger's own protocols | Not built in: send OTLP, which Jaeger accepts, or have a Collector convert it | Not planned |

## Where your program runs

What oxtel detects about where it runs, and records with everything it sends.

| Where | What's detected | Status |
|---|---|---|
| Any host | The host's name, architecture and machine id, the OS, and the process, but not its command line, which may hold secrets | Supported |
| Containers | The container's id, from cgroup v1 or v2 | Supported |
| Kubernetes | The namespace and pod, and the node from `K8S_NODE_NAME` when the pod spec passes it in | Supported |
| Cloud providers | The provider, region and instance | Planned |
| Remote management | An OpAMP client, for configuration from a central server, such as sampling and log levels, applied without a restart | Planned |

[Every setting](/reference/settings/) lists what the environment can change, including which detectors run.

## Rust

oxtel needs Rust 1.94 or later, with any edition. It works in async programs on tokio and in programs that aren't async at all, such as [a command-line tool](/scenarios/cli/); gRPC brings a small tokio runtime of its own.
