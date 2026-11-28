# Ready once APM Server can write to Elasticsearch and Kibana is
# available: Kibana's start keeps Elasticsearch busy, and telemetry sent
# during it can take minutes to be searchable. Arrived once Elasticsearch
# has the worker's four job transactions, the failed job's log record and
# its jobs.processed counter. Three services starting on a shared runner
# take longer than the other stacks, so it waits up to 5 minutes for each.
patience=300
ready() {
    curl -sf http://localhost:8200/ | jq -e '.publish_ready == true' &&
        curl -sf http://localhost:5601/api/status | jq -e '.status.overall.level == "available"'
}
count() {
    curl -sfG "http://localhost:9200/$1/_count" --data-urlencode "q=$2" | jq -e '.count > 0'
}
arrived() {
    curl -sfG "http://localhost:9200/traces-apm*/_count" --data-urlencode 'q=service.name:worker AND transaction.name:job' |
        jq -e '.count >= 4' &&
        count 'logs-*' 'service.name:worker AND message:failed' &&
        count 'metrics-*' 'service.name:worker AND _exists_:jobs.processed'
}
