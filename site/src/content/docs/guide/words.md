---
title: The words worth knowing
description: The few OpenTelemetry terms worth knowing, in plain words, from events and spans to collectors and backends.
---

You can use oxtel without any of these, but they're the words backends and colleagues will use.

| Word | What it means here |
|---|---|
| **Event**, or log line | One thing that happened: `tracing::info!(user.id = 42, "signed in")`. |
| **Field**, or attribute | A named value on an event or span, such as `user.id = 42`. In JSON, each is a key of its own, so you can search and filter by it. |
| **Span** | A piece of work with a start and an end, such as one request or one database query: `tracing::info_span!("request")`. Events inside it carry its fields. |
| **Trace** | All the spans of one request, across every service it touched. The **trace id** on each log line ties them together. |
| **Metric** | A number tracked over time, such as requests served or how long they took: `metrics::counter!("orders.placed").increment(1)`. |
| **Guard** | What `oxtel::init()` returns. Keep it until the end of `main`: dropping it sends whatever is still waiting and shuts telemetry down. |
| **Service name** | What backends call your program. Set it with `OTEL_SERVICE_NAME`, or in code with `oxtel::builder().with_default_service_name("shop")`. |
| **Semantic conventions** | OpenTelemetry's agreed names for common things, such as `http.response.status_code` or `http.server.request.duration`, so that backends recognise them. oxtel's layers use them. |
| **OTLP** | The OpenTelemetry protocol: how traces, metrics and logs travel to a collector or backend. oxtel speaks it over HTTP, usually on port 4318. |
| **Exporter** | The part that sends one kind of telemetry somewhere: `otlp` to a collector or backend, `console` to standard output, `prometheus` for scraping. The `OTEL_TRACES_EXPORTER`, `OTEL_METRICS_EXPORTER` and `OTEL_LOGS_EXPORTER` settings choose them. |
| **Collector** | A program, usually the OpenTelemetry Collector, that receives OTLP and passes it on to one or more backends. Optional: you can send straight to a backend. |
| **Backend** | Where you look at the data: Jaeger, Grafana, Elastic, Honeycomb, Datadog and many more accept OTLP. |
