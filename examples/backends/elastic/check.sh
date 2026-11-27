# Ready once APM Server can write to Elasticsearch. Arrived once
# Elasticsearch has the worker's four job transactions, the failed job's
# log record and its jobs.processed counter, and Kibana is available.
ready() { curl -sf http://localhost:8200/ | jq -e '.publish_ready == true'; }
count() {
    curl -sfG "http://localhost:9200/$1/_count" --data-urlencode "q=$2" | jq -e '.count > 0'
}
arrived() {
    curl -sfG "http://localhost:9200/traces-apm*/_count" --data-urlencode 'q=service.name:worker AND transaction.name:job' |
        jq -e '.count >= 4' &&
        count 'logs-*' 'service.name:worker AND message:failed' &&
        count 'metrics-*' 'service.name:worker AND _exists_:jobs.processed' &&
        curl -sf http://localhost:5601/api/status | jq -e '.status.overall.level == "available"'
}
