#!/usr/bin/env bash
set -euo pipefail

# Interleaved strict A/B matrix focused on batching downside metrics (latency tails).
# Defaults compare 8420fdf5 -> 63ee4d98 and run latency scenarios at batch sizes 8,16,32,64.
#
# Required tools: git, python3, rg, ss, and local mosquitto binary at /home/eagle/MQTT/mosquitto/build/src/mosquitto.

BASE_REF="${BASE_REF:-8420fdf5}"
TARGET_REF="${TARGET_REF:-63ee4d98}"
RUNS="${RUNS:-20}"
WARMUP="${WARMUP:-2}"
TIMEOUT_SEC="${TIMEOUT_SEC:-300}"
QUALITY_MODE="${QUALITY_MODE:-strict}"
BATCHES="${BATCHES:-8 16 32 64}"
BROKER_PORT="${BROKER_PORT:-1884}"
BENCH_DURATION="${BENCH_DURATION:-10}"
BENCH_WARMUP="${BENCH_WARMUP:-2}"
PAYLOAD_SIZE="${PAYLOAD_SIZE:-64}"
QOS="${QOS:-1}"
PUBLISHERS="${PUBLISHERS:-1}"
SUBSCRIBERS="${SUBSCRIBERS:-1}"
TOPIC_BASE="${TOPIC_BASE:-bench/batch}"
INCLUDE_THROUGHPUT_SANITY="${INCLUDE_THROUGHPUT_SANITY:-1}"
OUT_ROOT="${OUT_ROOT:-benchmarks/results/regressions/batch-latency-matrix-8420fdf5-vs-63ee4d98}"
SEED_PREFIX="${SEED_PREFIX:-batch-latency-matrix-8420fdf5-vs-63ee4d98}"

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"
mkdir -p "$OUT_ROOT"

TMPROOT=$(mktemp -d /tmp/rumqtt-batch-latency-matrix-XXXXXX)
BASE_WT="$TMPROOT/base"
TARGET_WT="$TMPROOT/target"

cleanup() {
  set +e
  git worktree remove --force "$BASE_WT" >/dev/null 2>&1
  git worktree remove --force "$TARGET_WT" >/dev/null 2>&1
  rm -rf "$TMPROOT"
  pkill -f "mosquitto.*-p ${BROKER_PORT}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# 1) Create detached worktrees at requested refs.
git worktree add --detach "$BASE_WT" "$BASE_REF" >/dev/null
git worktree add --detach "$TARGET_WT" "$TARGET_REF" >/dev/null

apply_patch_to_wt() {
  local wt="$1"

  # Ensure benchmark client exists in historical refs.
  cp "$ROOT/benchmarks/clients/rumqttv5bench.rs" "$wt/benchmarks/clients/rumqttv5bench.rs"

  # Enable URL parsing for query-based max_request_batch override.
  perl -0pi -e 's@rumqttc = \{ package = "rumqttc-next", path = "\.\./rumqttc" \}@rumqttc = { package = "rumqttc-next", path = "../rumqttc", features = ["url"] }@' "$wt/benchmarks/Cargo.toml"

  # Add env-driven max_request_batch override in build_options, compatible with old refs.
  perl -0pi -e 's@let mut options = MqttOptions::new\(client_id, endpoint\.host\.clone\(\), endpoint\.port\);\n    options\.set_keep_alive\(30\);@let mut options = MqttOptions::new(client_id.clone(), endpoint.host.clone(), endpoint.port);\n\n    if let Ok(raw_batch) = std::env::var("RUMQTT_BENCH_MAX_REQUEST_BATCH") {\n        if let Ok(batch) = raw_batch.parse::<usize>() {\n            if batch > 0 {\n                let scheme = if endpoint.transport == "tls" { "mqtts" } else { "mqtt" };\n                if let Ok(parsed) = MqttOptions::parse_url(format!(\n                    "{scheme}://{}:{}?client_id={client_id}&max_request_batch_num={batch}",\n                    endpoint.host, endpoint.port\n                )) {\n                    options = parsed;\n                }\n            }\n        }\n    }\n\n    options.set_keep_alive(30);@s' "$wt/benchmarks/clients/rumqttv5bench.rs"

  git -C "$wt" add -f benchmarks/clients/rumqttv5bench.rs benchmarks/Cargo.toml
  git -C "$wt" commit --no-gpg-sign -m "bench: latency matrix temp patch" >/dev/null
}

# 2) Apply identical benchmark patch in both sides.
apply_patch_to_wt "$BASE_WT"
apply_patch_to_wt "$TARGET_WT"

PATCH_BASE_REF=$(git -C "$BASE_WT" rev-parse HEAD)
PATCH_TARGET_REF=$(git -C "$TARGET_WT" rev-parse HEAD)

echo "patched baseline ref: $PATCH_BASE_REF"
echo "patched target ref:   $PATCH_TARGET_REF"

# 3) Start local broker.
/home/eagle/MQTT/mosquitto/build/src/mosquitto -p "$BROKER_PORT" -d >/dev/null 2>&1 || true
sleep 1
ss -ltnp | rg ":${BROKER_PORT}" -n -S >/dev/null || {
  echo "broker failed to start on port ${BROKER_PORT}" >&2
  exit 1
}

run_one() {
  local mode="$1"
  local batch="$2"
  local out="$3"

  local topic="${TOPIC_BASE}/${mode}"
  local cmd="RUMQTT_BENCH_MAX_REQUEST_BATCH=${batch} cargo run -p benchmarks --bin rumqttv5bench --release -- \
    --mode ${mode} \
    --duration ${BENCH_DURATION} \
    --warmup ${BENCH_WARMUP} \
    --payload-size ${PAYLOAD_SIZE} \
    --qos ${QOS} \
    --url mqtt://127.0.0.1:${BROKER_PORT} \
    --publishers ${PUBLISHERS} \
    --subscribers ${SUBSCRIBERS} \
    --topic ${topic}"

  python3 benchmarks/regression.py \
    --baseline-branch "$PATCH_BASE_REF" \
    --target-branch "$PATCH_TARGET_REF" \
    --cmd "$cmd" \
    --runs "$RUNS" --warmup "$WARMUP" --timeout-sec "$TIMEOUT_SEC" --quality-mode "$QUALITY_MODE" \
    --interleaved --alternate-order --paired-analysis \
    --seed-prefix "${SEED_PREFIX}-${mode}-batch${batch}" \
    --output-dir "$out"
}

print_latency_verdicts() {
  local out="$1"
  local batch="$2"

  python3 - <<PY
import json
from pathlib import Path

p = Path("$out/summary.json")
s = json.loads(p.read_text())
pc = s.get("paired_comparison", {})
metrics = ["p50_us", "p95_us", "p99_us", "avg_us", "max_us"]
batch = "$batch"
print(f"latency batch={batch} paired metrics:")
for m in metrics:
    row = pc.get(m)
    if not row:
        print(f"  {m}: missing")
        continue
    low = row.get("relative_delta_ci_low_pct")
    high = row.get("relative_delta_ci_high_pct")
    med = row.get("relative_delta_median_pct")
    if isinstance(low, (int, float)) and isinstance(high, (int, float)):
        # For latency, positive delta means worse.
        if low > 0:
            verdict = "regression"
        elif high < 0:
            verdict = "uplift"
        else:
            verdict = "inconclusive"
    else:
        verdict = row.get("classification") or row.get("error") or "inconclusive"
    print(f"  {m}: median={med} ci=[{low}, {high}] verdict={verdict}")
PY
}

print_throughput_sanity() {
  local out="$1"
  local batch="$2"

  python3 - <<PY
import json
from pathlib import Path

p = Path("$out/summary.json")
s = json.loads(p.read_text())
pc = s.get("paired_comparison", {})
m = "throughput_avg" if "throughput_avg" in pc else "throughput"
row = pc.get(m, {})
batch = "$batch"
print(
    f"throughput batch={batch} metric={m} paired_runs={row.get('paired_runs')} "
    f"median={row.get('relative_delta_median_pct')} "
    f"ci=[{row.get('relative_delta_ci_low_pct')}, {row.get('relative_delta_ci_high_pct')}] "
    f"class={row.get('classification')}"
)
PY
}

# 4) Run matrix.
for b in $BATCHES; do
  lat_out="$OUT_ROOT/latency/interleaved-${QUALITY_MODE}-runs${RUNS}-batch${b}"
  echo "running latency batch=$b -> $lat_out"
  run_one latency "$b" "$lat_out"
  print_latency_verdicts "$lat_out" "$b"

  if [[ "$INCLUDE_THROUGHPUT_SANITY" == "1" ]]; then
    thr_out="$OUT_ROOT/throughput/interleaved-${QUALITY_MODE}-runs${RUNS}-batch${b}"
    echo "running throughput sanity batch=$b -> $thr_out"
    run_one throughput "$b" "$thr_out"
    print_throughput_sanity "$thr_out" "$b"
  fi
done

echo "done. outputs in: $OUT_ROOT"
