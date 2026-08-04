#!/usr/bin/env bash
set -uo pipefail

readonly ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly DOCKERFILE="$ROOT_DIR/tests/low-memory/Dockerfile"
readonly BROKER_CONFIG="$ROOT_DIR/tests/low-memory/mosquitto.conf"
readonly REPORT_TOOL="$ROOT_DIR/tests/low-memory/memory_stability_report.py"
readonly BROKER_IMAGE="eclipse-mosquitto:2.0.22@sha256:212f89e1eaeb2c322d6441b64396e3346026674db8fa9c27beac293405c32b3c"
readonly WARMUP_ROUNDS=5

client_selection="both"
repeat=3
measured_rounds=20
messages_per_round=5000
memory_mib=16
results_dir=""

usage() {
    cat <<'EOF'
Usage: scripts/test-memory-stability.sh [OPTIONS]

Options:
  --client v4|v5|both
  --repeat N
  --rounds N                 Measured rounds (minimum 10)
  --messages-per-round N
  --memory-mib N
  --results-dir PATH
  -h, --help
EOF
}

require_value() {
    if (($# < 2)); then
        echo "$1 requires a value" >&2
        usage >&2
        exit 2
    fi
}

while (($# > 0)); do
    case "$1" in
        --client)
            require_value "$@"
            client_selection="$2"
            shift 2
            ;;
        --repeat)
            require_value "$@"
            repeat="$2"
            shift 2
            ;;
        --rounds)
            require_value "$@"
            measured_rounds="$2"
            shift 2
            ;;
        --messages-per-round)
            require_value "$@"
            messages_per_round="$2"
            shift 2
            ;;
        --memory-mib)
            require_value "$@"
            memory_mib="$2"
            shift 2
            ;;
        --results-dir)
            require_value "$@"
            results_dir="$2"
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ "$client_selection" != v4 && "$client_selection" != v5 && "$client_selection" != both ]]; then
    echo "--client must be v4, v5, or both" >&2
    exit 2
fi
for value_name in repeat measured_rounds messages_per_round memory_mib; do
    value="${!value_name}"
    if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
        echo "${value_name//_/-} must be a positive integer" >&2
        exit 2
    fi
done
if ((measured_rounds < 10)); then
    echo "--rounds must be at least 10" >&2
    exit 2
fi

profile="official"
if [[ "$client_selection" != both || "$repeat" -ne 3 || "$measured_rounds" -ne 20 ||
    "$messages_per_round" -ne 5000 || "$memory_mib" -ne 16 ]]; then
    profile="diagnostic"
fi

readonly memory_bytes=$((memory_mib * 1024 * 1024))
readonly expected_messages=$(((WARMUP_ROUNDS + measured_rounds) * messages_per_round))
readonly expected_reconnects=$((1 + measured_rounds / 5))
readonly expected_cycles=$((WARMUP_ROUNDS + measured_rounds))
readonly expected_idle_boundaries=$((WARMUP_ROUNDS + measured_rounds))

if [[ -z "$results_dir" ]]; then
    results_dir="$ROOT_DIR/target/memory-stability/results/$(date -u +%Y%m%dT%H%M%SZ)"
fi
mkdir -p "$results_dir"
results_dir="$(cd "$results_dir" && pwd)"

readonly run_token="$(date -u +%H%M%S)$$"
readonly v4_image="rumqtt-memory-stability-v4:$run_token"
readonly v5_image="rumqtt-memory-stability-v5:$run_token"
active_containers=()
active_networks=()

cleanup() {
    for container in "${active_containers[@]}"; do
        docker rm -f "$container" >/dev/null 2>&1 || true
    done
    for network in "${active_networks[@]}"; do
        docker network rm "$network" >/dev/null 2>&1 || true
    done
    docker image rm "$v4_image" "$v5_image" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM

fail_inconclusive() {
    echo "INCONCLUSIVE: $*" >&2
    exit 2
}

command -v docker >/dev/null || fail_inconclusive "Docker is not installed"
command -v python3 >/dev/null || fail_inconclusive "Python 3 is not installed"
[[ "$(uname -s)" == Linux ]] || fail_inconclusive "the profile requires Linux"
[[ "$(uname -m)" == x86_64 ]] || fail_inconclusive "the profile requires x86_64"
[[ "$(stat -fc %T /sys/fs/cgroup 2>/dev/null)" == cgroup2fs ]] ||
    fail_inconclusive "cgroup v2 is not mounted at /sys/fs/cgroup"
docker info >/dev/null 2>&1 || fail_inconclusive "the Docker daemon is unavailable"
[[ -r "$REPORT_TOOL" ]] || fail_inconclusive "the result calculator is unavailable"

{
    echo "recorded_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "kernel=$(uname -srmo)"
    echo "architecture=$(uname -m)"
    echo "docker_client=$(docker version --format '{{.Client.Version}}')"
    echo "docker_server=$(docker version --format '{{.Server.Version}}')"
    docker info --format 'cgroup_version={{.CgroupVersion}} cgroup_driver={{.CgroupDriver}} docker_os={{.OperatingSystem}} docker_arch={{.Architecture}}'
    echo "host_rustc=$(rustc --version 2>/dev/null || echo unavailable)"
    echo "host_cargo=$(cargo --version 2>/dev/null || echo unavailable)"
    echo "builder=rust:1.88.0-alpine3.21"
    echo "target=x86_64-unknown-linux-musl"
    echo "musl=1.2.5"
    echo "broker_image=$BROKER_IMAGE"
} >"$results_dir/environment.txt"

printf '%s\n' \
    '{' \
    "  \"profile\": \"$profile\"," \
    "  \"client_selection\": \"$client_selection\"," \
    "  \"repeat\": $repeat," \
    "  \"warmup_rounds\": $WARMUP_ROUNDS," \
    "  \"measured_rounds\": $measured_rounds," \
    "  \"messages_per_round\": $messages_per_round," \
    "  \"memory_limit_bytes\": $memory_bytes," \
    '  "memory_swap_limit_bytes": 0,' \
    '  "sample_interval_ms": 100,' \
    '  "round_settle_ms": 250,' \
    '  "round_sample_count": 5' \
    '}' >"$results_dir/config.json"

if [[ "$profile" == official ]]; then
    echo "Running OFFICIAL memory-stability profile (3 runs per client)."
else
    echo "Running DIAGNOSTIC memory-stability profile; results do not replace the official profile."
fi
echo "Results will be preserved in $results_dir"

echo "Building static memory-stability client images with Rust 1.88.0..."
if ! docker build --file "$DOCKERFILE" --target stability-v4 --tag "$v4_image" "$ROOT_DIR"; then
    echo "Failed to build the v4 memory-stability image" >&2
    exit 1
fi
if ! docker build --file "$DOCKERFILE" --target stability-v5 --tag "$v5_image" "$ROOT_DIR"; then
    echo "Failed to build the v5 memory-stability image" >&2
    exit 1
fi

restart_broker() {
    local broker="$1"
    if ! docker restart --time 1 "$broker" >/dev/null; then
        return 1
    fi
    for _ in $(seq 1 50); do
        if docker exec "$broker" mosquitto_pub \
            --host 127.0.0.1 --port 1883 \
            --topic rumqtt/memory-stability/health --message restarted >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

wait_for_broker() {
    local broker="$1"
    for _ in $(seq 1 50); do
        if docker exec "$broker" mosquitto_pub \
            --host 127.0.0.1 --port 1883 \
            --topic rumqtt/memory-stability/health --message ready >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

event_value() {
    local path="$1"
    local key="$2"
    if [[ -r "$path" ]]; then
        awk -v key="$key" '$1 == key { print $2 }' "$path"
    else
        echo 0
    fi
}

log_field() {
    local line="$1"
    local key="$2"
    awk -v key="$key" '{
        for (i = 1; i <= NF; i++) {
            split($i, field, "=")
            if (field[1] == key) {
                print field[2]
                exit
            }
        }
    }' <<<"$line"
}

run_client() {
    local protocol="$1"
    local image="$2"
    local run="$3"
    local run_dir="$results_dir/$protocol/run-$run"
    local suffix="$run_token-$protocol-$run"
    local network="rumqtt-memory-stability-network-$suffix"
    local broker="rumqtt-memory-stability-broker-$suffix"
    local container="rumqtt-memory-stability-client-$suffix"
    local mqtt_run_id="${run_token}${run}"
    local client_log="$run_dir/client.log"
    local broker_log="$run_dir/broker.log"
    local samples="$run_dir/memory-current.csv"
    local boundaries="$run_dir/round-boundaries.csv"
    local rounds="$run_dir/rounds.csv"
    local cgroup_path=""
    local pid=""
    local start_ms
    local end_ms
    local elapsed
    local unix_ms
    local current
    local peak=0
    local final_bytes=0
    local exit_code=1
    local oom_killed=false
    local restart_seen=0
    local restart_processed=0
    local boundary_seen=0
    local boundary_processed=0
    local log_pid=""
    local wait_pid=""
    local restart_pids=()
    local result_line=""
    local scenario_success=false
    local restart_success=true
    local completed_messages=0
    local completed_reconnects=0
    local completed_cycles=0
    local idle_boundaries=0
    local oom_events=0
    local oom_kill_events=0
    local max_events=0

    mkdir -p "$run_dir/cgroup-rounds" "$run_dir/restarts"
    printf 'elapsed_ms,unix_ms,memory_current_bytes\n' >"$samples"
    printf 'kind,round,detected_elapsed_ms,client_elapsed_ms\n' >"$boundaries"

    active_networks+=("$network")
    active_containers+=("$broker" "$container")
    docker network create "$network" >/dev/null
    docker run --detach \
        --name "$broker" \
        --network "$network" \
        --network-alias broker \
        --volume "$BROKER_CONFIG:/mosquitto/config/mosquitto.conf:ro" \
        "$BROKER_IMAGE" >/dev/null
    if ! wait_for_broker "$broker"; then
        docker logs "$broker" >"$broker_log" 2>&1 || true
        echo "$protocol run $run: broker did not become ready" >&2
        return 1
    fi

    docker create \
        --name "$container" \
        --network "$network" \
        --memory "${memory_bytes}b" \
        --memory-swap "${memory_bytes}b" \
        --env MQTT_HOST=broker \
        --env MQTT_PORT=1883 \
        --env "RUN_ID=$mqtt_run_id" \
        --env "WARMUP_ROUNDS=$WARMUP_ROUNDS" \
        --env "MEASURED_ROUNDS=$measured_rounds" \
        --env "MESSAGES_PER_ROUND=$messages_per_round" \
        "$image" >/dev/null

    start_ms="$(date +%s%3N)"
    docker start "$container" >/dev/null
    docker logs --follow "$container" >"$client_log" 2>&1 &
    log_pid=$!
    docker wait "$container" >"$run_dir/exit-code.txt" &
    wait_pid=$!

    for _ in $(seq 1 100); do
        pid="$(docker inspect --format '{{.State.Pid}}' "$container")"
        if [[ "$pid" != 0 && -r "/proc/$pid/cgroup" ]]; then
            cgroup_path="$(sed -n 's/^0:://p' "/proc/$pid/cgroup")"
            cgroup_path="/sys/fs/cgroup$cgroup_path"
            [[ -r "$cgroup_path/memory.peak" ]] && break
        fi
        sleep 0.02
    done
    if [[ -z "$cgroup_path" || ! -r "$cgroup_path/memory.peak" ]]; then
        echo "$protocol run $run: cannot read client cgroup counters" >&2
        cgroup_path=""
    else
        actual_max="$(<"$cgroup_path/memory.max")"
        actual_swap_max="$(<"$cgroup_path/memory.swap.max")"
        if [[ "$actual_max" != "$memory_bytes" || "$actual_swap_max" != 0 ]]; then
            echo "$protocol run $run: cgroup limits are not enforceable" >&2
            cgroup_path=""
        fi
    fi

    while [[ -n "$pid" && -d "/proc/$pid" ]]; do
        unix_ms="$(date +%s%3N)"
        elapsed=$((unix_ms - start_ms))
        if [[ -n "$cgroup_path" && -r "$cgroup_path/memory.current" ]]; then
            current="$(<"$cgroup_path/memory.current")"
            printf '%s,%s,%s\n' "$elapsed" "$unix_ms" "$current" >>"$samples"
            final_bytes="$current"
            current_peak="$(<"$cgroup_path/memory.peak")"
            if ((current_peak > peak)); then
                peak="$current_peak"
            fi
        fi

        restart_seen="$(grep -c '^control=restart-broker ' "$client_log" 2>/dev/null || true)"
        while ((restart_processed < restart_seen)); do
            restart_processed=$((restart_processed + 1))
            (
                restart_status=0
                restart_broker "$broker" || restart_status=$?
                echo "$restart_status" >"$run_dir/restarts/restart-$restart_processed.status"
            ) >"$run_dir/restarts/restart-$restart_processed.log" 2>&1 &
            restart_pids+=("$!")
        done

        boundary_seen="$(grep -c '^phase=round-idle ' "$client_log" 2>/dev/null || true)"
        while ((boundary_processed < boundary_seen)); do
            boundary_processed=$((boundary_processed + 1))
            line="$(grep '^phase=round-idle ' "$client_log" | sed -n "${boundary_processed}p")"
            kind="$(log_field "$line" kind)"
            round_number="$(log_field "$line" round)"
            client_elapsed="$(log_field "$line" elapsed_ms)"
            printf '%s,%s,%s,%s\n' \
                "$kind" "$round_number" "$elapsed" "$client_elapsed" >>"$boundaries"
            if [[ -n "$cgroup_path" ]]; then
                cp "$cgroup_path/memory.stat" \
                    "$run_dir/cgroup-rounds/$kind-$round_number-memory.stat" 2>/dev/null || true
                cp "$cgroup_path/memory.events" \
                    "$run_dir/cgroup-rounds/$kind-$round_number-memory.events" 2>/dev/null || true
                cp "$cgroup_path/memory.stat" "$run_dir/memory.stat" 2>/dev/null || true
                cp "$cgroup_path/memory.events" "$run_dir/memory.events" 2>/dev/null || true
                printf '%s\n' "$peak" >"$run_dir/memory.peak"
                if [[ -r "$cgroup_path/memory.events.local" ]]; then
                    cp "$cgroup_path/memory.events.local" \
                        "$run_dir/cgroup-rounds/$kind-$round_number-memory.events.local" 2>/dev/null || true
                    cp "$cgroup_path/memory.events.local" \
                        "$run_dir/memory.events.local" 2>/dev/null || true
                fi
            fi
        done
        sleep 0.1
    done

    wait "$wait_pid" || true
    wait "$log_pid" || true
    for restart_pid in "${restart_pids[@]}"; do
        wait "$restart_pid" || true
    done
    end_ms="$(date +%s%3N)"
    exit_code="$(<"$run_dir/exit-code.txt")"
    oom_killed="$(docker inspect --format '{{.State.OOMKilled}}' "$container")"
    docker inspect "$container" >"$run_dir/docker-inspect.json"
    docker logs "$broker" >"$broker_log" 2>&1 || true

    if [[ -n "$cgroup_path" && -r "$cgroup_path/memory.peak" ]]; then
        peak="$(<"$cgroup_path/memory.peak")"
        cp "$cgroup_path/memory.peak" "$run_dir/memory.peak"
        cp "$cgroup_path/memory.events" "$run_dir/memory.events"
        cp "$cgroup_path/memory.stat" "$run_dir/memory.stat"
        if [[ -r "$cgroup_path/memory.events.local" ]]; then
            cp "$cgroup_path/memory.events.local" "$run_dir/memory.events.local"
        else
            echo "unavailable" >"$run_dir/memory.events.local"
        fi
    elif [[ ! -r "$run_dir/memory.events.local" ]]; then
        echo "unavailable" >"$run_dir/memory.events.local"
    fi
    if [[ -r "$run_dir/memory.events" ]]; then
        max_events="$(event_value "$run_dir/memory.events" max)"
        oom_events="$(event_value "$run_dir/memory.events" oom)"
        oom_kill_events="$(event_value "$run_dir/memory.events" oom_kill)"
    fi

    if ! python3 "$REPORT_TOOL" extract-rounds \
        --samples "$samples" --boundaries "$boundaries" --output "$rounds"; then
        printf 'round,median_bytes,sample_count,boundary_elapsed_ms,round_duration_ms\n' >"$rounds"
    fi

    result_line="$(grep '^result=pass ' "$client_log" | tail -1 || true)"
    if [[ -n "$result_line" ]]; then
        scenario_success=true
        completed_messages="$(log_field "$result_line" echoes)"
        completed_reconnects="$(log_field "$result_line" reconnects)"
        completed_cycles="$(log_field "$result_line" unsubscriptions)"
    fi
    idle_boundaries="$(grep -c '^phase=round-idle .* idle=true ' "$client_log" || true)"
    if [[ "$restart_processed" -ne "$expected_reconnects" ]]; then
        restart_success=false
    else
        for status_file in "$run_dir"/restarts/*.status; do
            if [[ ! -r "$status_file" || "$(<"$status_file")" -ne 0 ]]; then
                restart_success=false
            fi
        done
    fi

    analyze_status=0
    python3 "$REPORT_TOOL" analyze-run \
        --rounds "$rounds" \
        --output-json "$run_dir/result.json" \
        --output-text "$run_dir/summary.txt" \
        --protocol "$protocol" \
        --run "$run" \
        --profile "$profile" \
        --peak-bytes "$peak" \
        --final-bytes "$final_bytes" \
        --memory-limit-bytes "$memory_bytes" \
        --exit-code "$exit_code" \
        --duration-ms "$((end_ms - start_ms))" \
        --completed-messages "$completed_messages" \
        --expected-messages "$expected_messages" \
        --completed-reconnects "$completed_reconnects" \
        --expected-reconnects "$expected_reconnects" \
        --completed-cycles "$completed_cycles" \
        --expected-cycles "$expected_cycles" \
        --idle-boundaries "$idle_boundaries" \
        --expected-idle-boundaries "$expected_idle_boundaries" \
        --max-events "$max_events" \
        --oom-events "$oom_events" \
        --oom-kill-events "$oom_kill_events" \
        --oom-killed "$oom_killed" \
        --scenario-success "$scenario_success" \
        --restart-success "$restart_success" || analyze_status=$?
    cat "$run_dir/summary.txt"

    docker rm -f "$container" "$broker" >/dev/null 2>&1 || true
    docker network rm "$network" >/dev/null 2>&1 || true
    return "$analyze_status"
}

protocols=()
if [[ "$client_selection" == both || "$client_selection" == v4 ]]; then
    protocols+=(v4)
fi
if [[ "$client_selection" == both || "$client_selection" == v5 ]]; then
    protocols+=(v5)
fi

overall_status=0
reports=()
echo "memory.max=$memory_bytes bytes memory.swap.max=0 warmup=$WARMUP_ROUNDS measured=$measured_rounds messages/round=$messages_per_round"
for protocol in "${protocols[@]}"; do
    image="$v4_image"
    [[ "$protocol" == v5 ]] && image="$v5_image"
    for run in $(seq 1 "$repeat"); do
        echo "Running $protocol repetition $run/$repeat..."
        run_client "$protocol" "$image" "$run" || overall_status=1
        reports+=("$results_dir/$protocol/run-$run/result.json")
    done
done

aggregate_status=0
python3 "$REPORT_TOOL" aggregate \
    --output-json "$results_dir/summary.json" \
    --output-csv "$results_dir/summary.csv" \
    --output-text "$results_dir/summary.txt" \
    "${reports[@]}" || aggregate_status=$?
cat "$results_dir/summary.txt"
echo "Detailed results: $results_dir"

if [[ "$overall_status" -ne 0 || "$aggregate_status" -ne 0 ]]; then
    exit 1
fi
