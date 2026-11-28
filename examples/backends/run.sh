#!/usr/bin/env bash
# Starts each backend's stack, sends it the worker example's telemetry with
# the settings its page shows, and checks the backend has it. CI's
# `backends` job runs it; so can you, with Docker running:
#
#   examples/backends/run.sh           # every backend
#   examples/backends/run.sh jaeger    # one
#
# Each backend's directory holds what its page shows, and a check:
#
#   compose.yaml  the stack
#   settings.sh   the program's settings, as `export` lines
#   check.sh      defines `ready`, which succeeds once the stack takes
#                 telemetry, and `arrived`, once the backend has it. It may
#                 also define `send`, to send more than the worker's,
#                 `stop`, to stop what `send` left running, and `patience`,
#                 how many seconds to wait for each, 120 unless it says.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
cd "$here"
if [ $# -gt 0 ]; then backends=("$@"); else backends=(*/); fi

# Retries a command every 2 seconds, for up to `patience` seconds.
wait_for() {
    local what=$1
    shift
    for _ in $(seq 1 $((patience / 2))); do
        if "$@" >/dev/null 2>&1; then return 0; fi
        sleep 2
    done
    echo "$what: gave up after $patience seconds" >&2
    return 1
}

# The worker example, with the backend's settings.
send() {
    (source ./settings.sh && cd "$root" && cargo run -q -p oxtel-examples --example worker)
}

check() {
    local name=${1%/}
    cd "$here/$name"
    docker compose up -d --quiet-pull
    # On failure, show what the stack logged; then remove it either way.
    trap 'status=$?; if declare -F stop >/dev/null; then stop; fi; if [ $status -ne 0 ]; then docker compose logs --tail 60; fi; docker compose down -v --remove-orphans >/dev/null 2>&1; exit $status' EXIT
    patience=120
    # shellcheck source=/dev/null
    source ./check.sh
    wait_for "$name isn't ready" ready
    send
    wait_for "$name doesn't have the telemetry" arrived
}

(cd "$root" && cargo build -q -p oxtel-examples --example worker)
failed=()
for backend in "${backends[@]}"; do
    name=${backend%/}
    [ -n "${GITHUB_ACTIONS:-}" ] && echo "::group::$name"
    if (check "$name"); then result=ok; else result=FAILED; failed+=("$name"); fi
    [ -n "${GITHUB_ACTIONS:-}" ] && echo "::endgroup::"
    echo "$name: $result"
done
if [ ${#failed[@]} -gt 0 ]; then
    echo "failed: ${failed[*]}" >&2
    exit 1
fi
