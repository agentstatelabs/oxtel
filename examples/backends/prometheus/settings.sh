# Push metrics to Prometheus over OTLP. It takes metrics only, so send just
# those. The service's name becomes the `job` label.
export OTEL_SERVICE_NAME=worker
export OTEL_EXPORTER_OTLP_METRICS_ENDPOINT=http://localhost:9090/api/v1/otlp/v1/metrics
