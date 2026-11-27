# Ready once the health check answers; arrived once the debug exporter has
# printed a job span, the jobs.processed metric and the failed job's log
# record.
ready() { curl -sf http://localhost:13133/; }
arrived() {
    local printed
    printed=$(docker compose logs collector)
    grep -Eq 'Name +: job$' <<<"$printed" &&
        grep -q 'Name: jobs.processed' <<<"$printed" &&
        grep -q 'Body: Str(failed)' <<<"$printed"
}
