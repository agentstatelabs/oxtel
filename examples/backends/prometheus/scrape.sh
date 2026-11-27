# Or serve /metrics for Prometheus to scrape. 0.0.0.0 listens on every
# interface, so Prometheus can reach it from its container.
export OTEL_METRICS_EXPORTER=prometheus
export OTEL_EXPORTER_PROMETHEUS_HOST=0.0.0.0
