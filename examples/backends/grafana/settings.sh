# Each kind of telemetry goes straight to the store that keeps it.
export OTEL_SERVICE_NAME=worker
export OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://localhost:4318/v1/traces
export OTEL_EXPORTER_OTLP_LOGS_ENDPOINT=http://localhost:3100/otlp/v1/logs
export OTEL_EXPORTER_OTLP_METRICS_ENDPOINT=http://localhost:9090/api/v1/otlp/v1/metrics
# Metrics every 5 seconds rather than every minute, while you try it out.
export OTEL_METRIC_EXPORT_INTERVAL=5000
