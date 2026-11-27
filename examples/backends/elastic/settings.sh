# APM Server takes traces, metrics and logs over OTLP.
export OTEL_SERVICE_NAME=worker
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:8200
