# Conformance against the OpenTelemetry Collector

These tests send oxtel's output to a real OpenTelemetry Collector and check
what it decoded, through the Collector's file exporter:

- **Traces** over OTLP/HTTP, with and without gzip: ids, parents, the
  remote-parent flag bits, kinds, typed attributes, status, events, links.
- **Metrics** scraped by the Collector's Prometheus receiver from oxtel's
  `/metrics` (OpenMetrics): names, types, labels, values, and exemplars that
  carry the trace id.

CI runs them on every change (the `collector` job). To run them locally:

```bash
mkdir -p conformance/out && chmod 777 conformance/out
docker run -d --name otelcol --network host \
  -v "$PWD/conformance/collector.yaml:/etc/otelcol-contrib/config.yaml:ro" \
  -v "$PWD/conformance/out:/out" \
  otel/opentelemetry-collector-contrib:0.161.0
cargo test -p oxtel-conformance -- --ignored --test-threads=1
docker rm -f otelcol
```

Host networking needs Linux. Elsewhere, set `OXTEL_COLLECTOR_OTLP` to the
Collector's OTLP/HTTP address, and point its Prometheus target at your host.
