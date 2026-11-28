# APM Server takes traces, metrics and logs over OTLP.
export OTEL_SERVICE_NAME=worker
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:8200
# Metrics every 5 seconds rather than every minute, while you try it out.
export OTEL_METRIC_EXPORT_INTERVAL=5000
