# Ready once the UI answers; arrived once Jaeger's query API finds the
# worker's four job spans.
ready() { curl -sf http://localhost:16686/; }
arrived() {
    curl -sf "http://localhost:16686/api/v3/traces?query.service_name=worker&query.start_time_min=2000-01-01T00:00:00Z&query.start_time_max=2100-01-01T00:00:00Z" |
        jq -se '[.[].result.resourceSpans[].scopeSpans[].spans[] | select(.name == "job")] | length >= 4'
}
