#!/usr/bin/env bash
set -euo pipefail

# Reproduce interleaved strict A/B for 8420fdf5 -> 63ee4d98 with batch sizes 8, 32, 64.
# This script creates temporary patch refs for BOTH sides so comparison stays fair.

BASE_REF="${BASE_REF:-8420fdf5}"
TARGET_REF="${TARGET_REF:-63ee4d98}"
RUNS="${RUNS:-40}"
WARMUP="${WARMUP:-2}"
TIMEOUT_SEC="${TIMEOUT_SEC:-300}"
BATCHES="${BATCHES:-8 32 64}"
BROKER_PORT="${BROKER_PORT:-1884}"
OUT_ROOT="${OUT_ROOT:-benchmarks/results/regressions/batch-matrix-8420fdf5-vs-63ee4d98}"
SEED_PREFIX="${SEED_PREFIX:-batch-matrix-8420fdf5-vs-63ee4d98}"

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"
mkdir -p "$OUT_ROOT"

TMPROOT=$(mktemp -d /tmp/rumqtt-batch-matrix-XXXXXX)
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

# 1) Create detached worktrees at requested refs

git worktree add --detach "$BASE_WT" "$BASE_REF" >/dev/null
git worktree add --detach "$TARGET_WT" "$TARGET_REF" >/dev/null

# 2) Apply identical benchmark patches in both worktrees
# - env override for max_request_batch in rumqttasync
# - force local broker port for that override path
# - enable rumqttc url feature in benchmarks/Cargo.toml

apply_patch_to_wt() {
  local wt="$1"

  perl -0pi -e 's@let mut mqttoptions = MqttOptions::new\(id, "localhost", 1883\);\n    mqttoptions.set_keep_alive\(20\);\n    mqttoptions.set_inflight\(100\);@let mut mqttoptions = if let Ok(raw_batch) = std::env::var("RUMQTT_BENCH_MAX_REQUEST_BATCH") {\n        let batch = raw_batch.parse::<usize>()?;\n        MqttOptions::parse_url(format!("mqtt://localhost:1883?client_id={id}&max_request_batch_num={batch}"))?\n    } else {\n        MqttOptions::new(id, "localhost", 1883)\n    };\n    mqttoptions.set_keep_alive(20);\n    mqttoptions.set_inflight(100);@s' "$wt/benchmarks/clients/rumqttasync.rs"
  perl -0pi -e "s@mqtt://localhost:1883\\?client_id=@mqtt://localhost:${BROKER_PORT}?client_id=@g" "$wt/benchmarks/clients/rumqttasync.rs"

  perl -0pi -e 's@rumqttc = \{ package = "rumqttc-next", path = "\.\./rumqttc" \}@rumqttc = { package = "rumqttc-next", path = "../rumqttc", features = ["url"] }@' "$wt/benchmarks/Cargo.toml"

  git -C "$wt" add -f benchmarks/clients/rumqttasync.rs benchmarks/Cargo.toml
  git -C "$wt" commit --no-gpg-sign -m "bench: batch matrix env override patch" >/dev/null
}

apply_patch_to_wt "$BASE_WT"
apply_patch_to_wt "$TARGET_WT"

PATCH_BASE_REF=$(git -C "$BASE_WT" rev-parse HEAD)
PATCH_TARGET_REF=$(git -C "$TARGET_WT" rev-parse HEAD)

echo "patched baseline ref: $PATCH_BASE_REF"
echo "patched target ref:   $PATCH_TARGET_REF"

# 3) Start a local broker on BROKER_PORT
/home/eagle/MQTT/mosquitto/build/src/mosquitto -p "$BROKER_PORT" -d >/dev/null 2>&1 || true
sleep 1
ss -ltnp | rg ":${BROKER_PORT}" -n -S >/dev/null || {
  echo "broker failed to start on port ${BROKER_PORT}" >&2
  exit 1
}

# 4) Run matrix
for b in $BATCHES; do
  out="$OUT_ROOT/interleaved-strict40-batch${b}"
  echo "running batch=$b -> $out"

  python3 benchmarks/regression.py \
    --baseline-branch "$PATCH_BASE_REF" \
    --target-branch "$PATCH_TARGET_REF" \
    --cmd "RUMQTT_BENCH_MAX_REQUEST_BATCH=${b} cargo run -p benchmarks --bin rumqttasync --release" \
    --runs "$RUNS" --warmup "$WARMUP" --timeout-sec "$TIMEOUT_SEC" --quality-mode strict \
    --interleaved --alternate-order --paired-analysis \
    --seed-prefix "${SEED_PREFIX}-batch${b}" \
    --output-dir "$out"

  python3 - <<PY
import json
from pathlib import Path
p = Path("$out/summary.json")
s = json.loads(p.read_text())
m = "throughput_avg" if "throughput_avg" in s["results"]["baseline"]["metrics"] else "throughput"
pc = s.get("paired_comparison", {}).get(m, {})
batch = "$b"
print(
    f"batch={batch} metric={m} paired_runs={pc.get('paired_runs')} "
    f"median={pc.get('relative_delta_median_pct')} "
    f"ci=[{pc.get('relative_delta_ci_low_pct')}, {pc.get('relative_delta_ci_high_pct')}] "
    f"class={pc.get('classification')}"
)
PY

done

echo "done. outputs in: $OUT_ROOT"
