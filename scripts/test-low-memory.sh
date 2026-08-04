#!/usr/bin/env bash
set -uo pipefail

readonly ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly DOCKERFILE="$ROOT_DIR/tests/low-memory/Dockerfile"
readonly BROKER_CONFIG="$ROOT_DIR/tests/low-memory/mosquitto.conf"
readonly BROKER_IMAGE="eclipse-mosquitto:2.0.22@sha256:212f89e1eaeb2c322d6441b64396e3346026674db8fa9c27beac293405c32b3c"

memory_mib=10
results_dir=""

usage() {
    echo "Usage: $0 [--memory-mib N] [--results-dir PATH]"
}

while (($# > 0)); do
    case "$1" in
        --memory-mib)
            if (($# < 2)); then
                echo "--memory-mib requires a value" >&2
                usage >&2
                exit 2
            fi
            memory_mib="$2"
            shift 2
            ;;
        --results-dir)
            if (($# < 2)); then
                echo "--results-dir requires a value" >&2
                usage >&2
                exit 2
            fi
            results_dir="$2"
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

if [[ ! "$memory_mib" =~ ^[1-9][0-9]*$ ]]; then
    echo "--memory-mib must be a positive integer" >&2
    exit 2
fi

readonly memory_bytes=$((memory_mib * 1024 * 1024))
if [[ -z "$results_dir" ]]; then
    results_dir="$ROOT_DIR/target/low-memory/results/$(date -u +%Y%m%dT%H%M%SZ)"
fi
mkdir -p "$results_dir"
results_dir="$(cd "$results_dir" && pwd)"

readonly run_token="$(date -u +%H%M%S)$$"
readonly network_name="rumqtt-low-memory-$run_token"
readonly broker_name="rumqtt-low-memory-broker-$run_token"
readonly v4_image="rumqtt-low-memory-v4:$run_token"
readonly v5_image="rumqtt-low-memory-v5:$run_token"

client_containers=()
cleanup() {
    for container in "${client_containers[@]}"; do
        docker rm -f "$container" >/dev/null 2>&1 || true
    done
    docker rm -f "$broker_name" >/dev/null 2>&1 || true
    docker network rm "$network_name" >/dev/null 2>&1 || true
    docker image rm "$v4_image" "$v5_image" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

fail_inconclusive() {
    echo "INCONCLUSIVE: $*" >&2
    exit 2
}

restart_broker() {
    if ! docker restart --time 1 "$broker_name" >/dev/null; then
        echo "Failed to restart the broker" >&2
        return 1
    fi

    for _ in $(seq 1 30); do
        if docker exec "$broker_name" mosquitto_pub \
            --host 127.0.0.1 --port 1883 --topic rumqtt/low-memory/health --message restarted \
            >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.2
    done

    echo "Broker did not become ready after restart" >&2
    return 1
}

command -v docker >/dev/null || fail_inconclusive "Docker is not installed"
[[ "$(uname -s)" == "Linux" ]] || fail_inconclusive "the profile requires Linux"
[[ "$(uname -m)" == "x86_64" ]] || fail_inconclusive "the profile requires x86_64"
[[ "$(stat -fc %T /sys/fs/cgroup 2>/dev/null)" == "cgroup2fs" ]] ||
    fail_inconclusive "cgroup v2 is not mounted at /sys/fs/cgroup"
docker info >/dev/null 2>&1 || fail_inconclusive "the Docker daemon is unavailable"

echo "Building static low-memory client images with Rust 1.88.0 (x86_64-unknown-linux-musl)..."
if ! docker build --file "$DOCKERFILE" --target v4 --tag "$v4_image" "$ROOT_DIR"; then
    echo "Failed to build the v4 low-memory image" >&2
    exit 1
fi
if ! docker build --file "$DOCKERFILE" --target v5 --tag "$v5_image" "$ROOT_DIR"; then
    echo "Failed to build the v5 low-memory image" >&2
    exit 1
fi

docker network create "$network_name" >/dev/null
docker run --detach \
    --name "$broker_name" \
    --network "$network_name" \
    --network-alias broker \
    --volume "$BROKER_CONFIG:/mosquitto/config/mosquitto.conf:ro" \
    "$BROKER_IMAGE" >/dev/null

broker_ready=false
for _ in $(seq 1 30); do
    if docker exec "$broker_name" mosquitto_pub \
        --host 127.0.0.1 --port 1883 --topic rumqtt/low-memory/health --message ready \
        >/dev/null 2>&1; then
        broker_ready=true
        break
    fi
    sleep 0.2
done
if [[ "$broker_ready" != true ]]; then
    docker logs "$broker_name" >&2 || true
    echo "Broker did not become ready" >&2
    exit 1
fi

printf 'client\tresult\tpeak_bytes\toom_killed\texit_code\tduration_ms\n' >"$results_dir/summary.tsv"

run_client() {
    local protocol="$1"
    local image="$2"
    local container="rumqtt-low-memory-$protocol-$run_token"
    local log_file="$results_dir/$protocol.log"
    local metrics_file="$results_dir/$protocol-cgroup.txt"
    local start_ms
    local end_ms
    local pid=""
    local cgroup_path=""
    local peak=""
    local current=""
    local events=""
    local disrupted=false
    local running=true
    local exit_code
    local oom_killed
    local oom_events=0
    local oom_kill_events=0
    local result
    local restart_pid=""
    local restart_status=0
    local restart_log="$results_dir/$protocol-broker-restart.log"

    client_containers+=("$container")
    docker create \
        --name "$container" \
        --network "$network_name" \
        --memory "${memory_bytes}b" \
        --memory-swap "${memory_bytes}b" \
        --env MQTT_HOST=broker \
        --env MQTT_PORT=1883 \
        --env "RUN_ID=$run_token" \
        "$image" >/dev/null

    start_ms="$(date +%s%3N)"
    docker start "$container" >/dev/null

    while [[ "$running" == true ]]; do
        if [[ -z "$cgroup_path" ]]; then
            pid="$(docker inspect --format '{{.State.Pid}}' "$container")"
            if [[ "$pid" != 0 && -r "/proc/$pid/cgroup" ]]; then
                cgroup_path="$(sed -n 's/^0:://p' "/proc/$pid/cgroup")"
                cgroup_path="/sys/fs/cgroup$cgroup_path"
                if [[ ! -r "$cgroup_path/memory.peak" ]]; then
                    fail_inconclusive "cannot read the client cgroup memory counters"
                fi
                local actual_max
                local actual_swap_max
                actual_max="$(<"$cgroup_path/memory.max")"
                actual_swap_max="$(<"$cgroup_path/memory.swap.max")"
                [[ "$actual_max" == "$memory_bytes" ]] ||
                    fail_inconclusive "memory.max is $actual_max, expected $memory_bytes"
                [[ "$actual_swap_max" == 0 ]] ||
                    fail_inconclusive "memory.swap.max is $actual_swap_max, expected 0"
            fi
        fi

        if [[ -n "$cgroup_path" && -r "$cgroup_path/memory.peak" ]]; then
            peak="$(<"$cgroup_path/memory.peak")"
            current="$(<"$cgroup_path/memory.current")"
            events="$(<"$cgroup_path/memory.events")"
            {
                echo "memory.max=$memory_bytes"
                echo "memory.swap.max=0"
                echo "memory.current=$current"
                echo "memory.peak=$peak"
                echo "$events"
            } >"$metrics_file"
        fi

        if [[ "$disrupted" == false ]] &&
            docker logs "$container" 2>&1 | grep -q 'phase=ready-for-network-loss'; then
            restart_broker >"$restart_log" 2>&1 &
            restart_pid=$!
            disrupted=true
        fi

        running="$(docker inspect --format '{{.State.Running}}' "$container")"
        [[ "$running" == true ]] && sleep 0.05
    done

    if [[ -n "$restart_pid" ]]; then
        wait "$restart_pid" || restart_status=$?
    fi

    end_ms="$(date +%s%3N)"
    docker logs "$container" >"$log_file" 2>&1 || true
    exit_code="$(docker inspect --format '{{.State.ExitCode}}' "$container")"
    oom_killed="$(docker inspect --format '{{.State.OOMKilled}}' "$container")"

    if [[ -n "$events" ]]; then
        oom_events="$(awk '$1 == "oom" { print $2 }' <<<"$events")"
        oom_kill_events="$(awk '$1 == "oom_kill" { print $2 }' <<<"$events")"
    fi

    if [[ "$oom_killed" == true || "$exit_code" -eq 137 || "$oom_events" -gt 0 ||
        "$oom_kill_events" -gt 0 || (-n "$peak" && "$peak" -gt "$memory_bytes") ]]; then
        result="FAIL_MEMORY"
    elif [[ "$exit_code" -ne 0 || "$disrupted" != true || "$restart_status" -ne 0 ]]; then
        result="FAIL_FUNCTIONAL"
    elif [[ -z "$peak" ]]; then
        result="INCONCLUSIVE"
    else
        result="PASS"
    fi

    printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$protocol" "$result" "${peak:-unavailable}" "$oom_killed" "$exit_code" "$((end_ms - start_ms))" \
        >>"$results_dir/summary.tsv"
    printf '%-3s %-15s peak=%s bytes oom_killed=%s exit=%s duration=%sms\n' \
        "$protocol" "$result" "${peak:-unavailable}" "$oom_killed" "$exit_code" "$((end_ms - start_ms))"

    [[ "$result" == PASS ]]
}

echo "Running clients with memory.max=$memory_bytes bytes and memory.swap.max=0..."
overall=0
run_client v4 "$v4_image" || overall=1
run_client v5 "$v5_image" || overall=1

echo
echo "Results: $results_dir/summary.tsv"
column -t -s $'\t' "$results_dir/summary.tsv" 2>/dev/null || cat "$results_dir/summary.tsv"
exit "$overall"
