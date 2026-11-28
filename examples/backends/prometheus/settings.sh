# Push metrics to Prometheus over OTLP. It takes metrics only, so send just
# those. The service's name becomes the `job` label.
export OTEL_SERVICE_NAME=worker
export OTEL_EXPORTER_OTLP_METRICS_ENDPOINT=http://localhost:9090/api/v1/otlp/v1/metrics
# Metrics every 5 seconds rather than every minute, while you try it out.
export OTEL_METRIC_EXPORT_INTERVAL=5000
