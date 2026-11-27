# Jaeger takes traces only, so send just those.
export OTEL_SERVICE_NAME=worker
export OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://localhost:4318/v1/traces
