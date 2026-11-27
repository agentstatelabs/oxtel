# Ready once Tempo, Loki, Prometheus and Grafana all say so. Arrived once
# Tempo finds the worker's four traces, Loki its failed job's log line and
# Prometheus its jobs.processed counter, and Grafana reaches all three.
ready() {
    curl -sf http://localhost:3200/ready && curl -sf http://localhost:3100/ready &&
        curl -sf http://localhost:9090/-/ready && curl -sf http://localhost:3000/api/health
}
arrived() {
    curl -sfG http://localhost:3200/api/search --data-urlencode 'q={resource.service.name="worker"}' |
        jq -e '.traces | length >= 4' &&
        curl -sfG http://localhost:3100/loki/api/v1/query_range --data-urlencode 'query={service_name="worker"} |= "failed"' |
        jq -e '.data.result | length > 0' &&
        curl -sfG http://localhost:9090/api/v1/query --data-urlencode 'query=jobs_processed_total{job="worker"}' |
        jq -e '.data.result | length > 0' &&
        for uid in tempo loki prometheus; do
            curl -sf "http://localhost:3000/api/datasources/uid/$uid/health" | jq -e '.status == "OK"' || return 1
        done
}
