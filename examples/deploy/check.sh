#!/usr/bin/env bash
# Checks the deployment guides' files by using them, as CI's `deploy` job
# does:
#
#   examples/deploy/check.sh               # docker, kubernetes and systemd
#   examples/deploy/check.sh docker        # one of them
#
# - docker: runs the service and a Collector with docker/compose.yaml,
#   sends a request, and stops the service as Docker does, with SIGTERM.
#   It must exit at once and cleanly, and its span must reach the Collector.
# - kubernetes: starts a one-node Kubernetes (k3s) in a container of its
#   own, and applies the manifests there; it never uses another cluster.
#   The request's span must reach the Collector with the pod's attributes.
# - systemd: installs the service on this machine with systemd/, which
#   needs Linux with systemd, sudo and Docker, as CI's runners have.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
image=oxtel-service:example
if [ $# -gt 0 ]; then checks=("$@"); else checks=(docker kubernetes systemd); fi

# Retries a command every 2 seconds, for up to 3 minutes.
wait_for() {
    local what=$1
    shift
    for _ in $(seq 1 90); do
        if "$@" >/dev/null 2>&1; then return 0; fi
        sleep 2
    done
    echo "$what: gave up after 3 minutes" >&2
    return 1
}

build_image() {
    (cd "$root" && docker build -q -f examples/deploy/Dockerfile -t "$image" .) >/dev/null
}

docker_check() {
    cd "$here/docker"
    trap 'status=$?; if [ $status -ne 0 ]; then docker compose logs --tail 40; fi; docker compose down -v >/dev/null 2>&1; exit $status' EXIT
    build_image
    docker compose up -d --quiet-pull
    wait_for "the service doesn't answer" curl -sf http://localhost:8080/healthz
    curl -sf http://localhost:8080/orders/7
    local started
    started=$(date +%s)
    docker compose stop shop
    # Stopped by SIGTERM well before Docker would kill it, and cleanly.
    [ $(($(date +%s) - started)) -lt 8 ]
    [ "$(docker inspect -f '{{.State.ExitCode}}' "$(docker compose ps -aq shop)")" = 0 ]
    # Its lines are JSON, since its output isn't a terminal, and the last
    # says it stopped.
    docker compose logs --no-log-prefix shop | tail -1 | jq -e '.message == "stopped"'
    # The request's span, sent as the service stopped, reached the Collector,
    # with the container's id.
    local id
    id=$(docker inspect -f '{{.Id}}' "$(docker compose ps -aq shop)")
    wait_for "the Collector doesn't have the request's span" \
        bash -c "docker compose logs collector | grep -Eq 'Name +: GET /orders/\{id\}'"
    local printed
    printed=$(docker compose logs collector)
    grep -q "container.id: Str($id)" <<<"$printed"
}

kubernetes_check() {
    cd "$here/kubernetes"
    local work
    work=$(mktemp -d)
    # Only the cluster started here, whatever else this machine knows.
    export KUBECONFIG=$work/kubeconfig
    # shellcheck disable=SC2064 # $work is expanded now, while it's set.
    trap "status=\$?; if [ \$status -ne 0 ]; then kubectl get pods -o wide; kubectl logs deployment/otel-collector --tail 40; fi; docker rm -f oxtel-k3s >/dev/null 2>&1; rm -rf '$work'; exit \$status" EXIT
    docker image inspect "$image" >/dev/null 2>&1 || build_image
    docker run -d --name oxtel-k3s --privileged -p 127.0.0.1:16443:6443 \
        rancher/k3s:v1.37.2-k3s1 server --disable=traefik --disable=metrics-server >/dev/null
    wait_for "k3s didn't start" docker exec oxtel-k3s test -s /etc/rancher/k3s/k3s.yaml
    docker exec oxtel-k3s cat /etc/rancher/k3s/k3s.yaml |
        sed 's#https://127.0.0.1:6443#https://127.0.0.1:16443#' >"$KUBECONFIG"
    wait_for "the node isn't ready" kubectl wait --for=condition=Ready node --all --timeout=5s
    docker save "$image" | docker exec -i oxtel-k3s ctr images import - >/dev/null
    kubectl apply -f collector.yaml -f service.yaml
    # The agent isn't run here, but the cluster must accept it.
    kubectl apply --dry-run=server -f collector-agent.yaml
    kubectl rollout status deployment/otel-collector --timeout=180s
    kubectl rollout status deployment/shop --timeout=180s
    kubectl port-forward service/shop 18080:80 >/dev/null 2>&1 &
    local forward=$!
    wait_for "the service doesn't answer" curl -sf http://localhost:18080/orders/7
    kill "$forward"
    # The span arrives within the batch delay, with the pod's attributes.
    arrived() {
        local printed
        printed=$(kubectl logs deployment/otel-collector)
        grep -Eq 'Name +: GET /orders/\{id\}' <<<"$printed" &&
            grep -q 'k8s.namespace.name: Str(default)' <<<"$printed" &&
            grep -Eq 'k8s.pod.name: Str\(shop-' <<<"$printed" &&
            grep -q 'k8s.node.name: Str(' <<<"$printed" &&
            grep -q 'k8s.pod.uid: Str(' <<<"$printed" &&
            grep -q 'k8s.deployment.name: Str(shop)' <<<"$printed"
    }
    wait_for "the Collector doesn't have the span with the pod's attributes" arrived
}

systemd_check() {
    cd "$here/systemd"
    trap 'status=$?; if [ $status -ne 0 ]; then sudo journalctl -u shop --no-pager | tail -40; fi; sudo systemctl stop shop >/dev/null 2>&1; sudo rm -f /etc/systemd/system/shop.service /usr/local/bin/service; sudo rm -rf /etc/shop; sudo systemctl daemon-reload; docker compose -f "$root/examples/backends/collector/compose.yaml" down -v >/dev/null 2>&1; exit $status' EXIT
    (cd "$root" && cargo build -q --release -p oxtel-examples --example service)
    local target
    target=$(cd "$root" && cargo metadata -q --format-version 1 --no-deps | jq -r .target_directory)
    # A Collector on localhost:4318, where telemetry.env sends.
    docker compose -f "$root/examples/backends/collector/compose.yaml" up -d --quiet-pull
    wait_for "the Collector isn't ready" curl -sf http://localhost:13133/
    sudo install -m 0755 "$target/release/examples/service" /usr/local/bin/service
    sudo install -D -m 0600 telemetry.env /etc/shop/telemetry.env
    sudo install -m 0644 shop.service /etc/systemd/system/shop.service
    systemd-analyze verify /etc/systemd/system/shop.service
    sudo systemctl daemon-reload
    sudo systemctl start shop
    wait_for "the service doesn't answer" curl -sf http://localhost:8080/healthz
    curl -sf http://localhost:8080/orders/7
    local started
    started=$(date +%s)
    sudo systemctl stop shop
    # Stopped by SIGTERM at once, and cleanly.
    [ $(($(date +%s) - started)) -lt 8 ]
    [ "$(systemctl show -p Result --value shop)" = success ]
    # The service's own last line, after systemd's lines about it.
    sudo journalctl _SYSTEMD_UNIT=shop.service --no-pager -o cat | tail -1 | jq -e '.message == "stopped"'
    wait_for "the Collector doesn't have the request's span" \
        bash -c "docker compose -f '$root/examples/backends/collector/compose.yaml' logs collector | grep -Eq 'Name +: GET /orders/\{id\}'"
}

failed=()
for check in "${checks[@]}"; do
    [ -n "${GITHUB_ACTIONS:-}" ] && echo "::group::$check"
    if ("${check}_check"); then result=ok; else result=FAILED; failed+=("$check"); fi
    [ -n "${GITHUB_ACTIONS:-}" ] && echo "::endgroup::"
    echo "$check: $result"
done
if [ ${#failed[@]} -gt 0 ]; then
    echo "failed: ${failed[*]}" >&2
    exit 1
fi
