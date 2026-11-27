# Ready once Prometheus says so. Besides the worker's pushed metrics, runs
# the Prometheus example, which serves /metrics until it's stopped, for
# Prometheus to scrape. Arrived once a query finds both.
ready() { curl -sf http://localhost:9090/-/ready; }
send() {
    (source ./settings.sh && cd "$root" && cargo run -q -p oxtel-examples --example worker)
    (cd "$root" && cargo build -q -p oxtel-examples --example prometheus)
    local target
    target=$(cd "$root" && cargo metadata -q --format-version 1 --no-deps | jq -r .target_directory)
    (source ./scrape.sh && exec "$target/debug/examples/prometheus") &
    scraped=$!
}
stop() {
    if [ -n "${scraped:-}" ]; then
        kill "$scraped"
        wait "$scraped" 2>/dev/null || true
    fi
}
query() {
    curl -sf http://localhost:9090/api/v1/query --data-urlencode "query=$1" |
        jq -e '.data.result | length > 0'
}
arrived() {
    query 'jobs_processed_total{job="worker"}' && query 'orders_placed_total{job="app"}'
}
