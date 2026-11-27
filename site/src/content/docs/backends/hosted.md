---
title: Hosted backends
description: Sending to a hosted observability service that takes OTLP, with its endpoint and its key, and what to check when nothing arrives.
---

Most hosted observability services take OTLP, so oxtel sends to them as it would to a Collector. Each gives you two things: an endpoint, and a key to send in a header. Set both, and name your service:

```bash
export OTEL_SERVICE_NAME=shop
export OTEL_EXPORTER_OTLP_ENDPOINT=https://otlp.example.com
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <token>"
```

The service's documentation gives the endpoint and the header's name and form. Common ones are `Authorization=Bearer <token>`, `Authorization=Basic <encoded id and token>`, and a header of the service's own, such as `x-api-key=<key>`. Several headers are separated by commas.

## What to check

- **The startup line.** When the program starts, oxtel logs where each kind of telemetry goes, with every URL in full. It never shows the headers.
- **The endpoint's path.** With `OTEL_EXPORTER_OTLP_ENDPOINT`, oxtel adds `/v1/traces`, `/v1/metrics` and `/v1/logs` itself, so give it the base the service names. A service that gives one URL per kind goes in `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and the others, each used exactly as given.
- **HTTP or gRPC.** oxtel sends OTLP over HTTP with protobuf by default, which most services take. For one that asks for gRPC, build with oxtel's `grpc` feature and set `OTEL_EXPORTER_OTLP_PROTOCOL=grpc`. [Sending to a collector](/guide/sending/) has the details, and TLS with your own certificates.
- **A refusal.** A warning that sending was refused with status 401 or 403 means the key or its header is wrong. [When something's wrong](/guide/troubleshooting/#nothing-arrives-at-the-collector-or-backend) goes through the rest.

## Metrics a service may prefer

Some services ask for metrics in a particular shape:

- **Deltas** instead of totals: `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE=delta`.
- **Exponential histograms**, which need no buckets chosen in advance: `OTEL_EXPORTER_OTLP_METRICS_DEFAULT_HISTOGRAM_AGGREGATION=base2_exponential_bucket_histogram`.

[Metrics for your backend](/guide/metrics/) explains both.

## Keeping the key safe

The key is a secret: keep it out of your code and your repository, and give it to the program from the environment, as your platform passes secrets. Or run an [OpenTelemetry Collector](/backends/collector/) beside your programs, holding the key in one place, and point the programs at the Collector instead.

## How it's checked

There's no stack to run here, since each service is its own. CI checks oxtel's OTLP against a real [Collector](/backends/collector/), the way any OTLP receiver reads it, and the [unit tests](https://github.com/agentstatelabs/oxtel/tree/main/crates/oxtel-otlp) check headers, compression, retries and TLS.
