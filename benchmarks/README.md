# Benchmarks

`benchmarks` is the maintained performance harness for this workspace. It is a
Cargo package named `benchmarks` with one Rust workload binary and one Python
orchestrator.

## Workload CLI

Run deterministic codec benchmarks without a broker:

```bash
cargo run -p benchmarks --bin rumqtt-bench -- \
  codec roundtrip --protocol v5 --messages 100000 --payload-size 64 --qos 1
```

Run broker-backed client benchmarks against an external broker:

```bash
cargo run -p benchmarks --bin rumqtt-bench -- \
  client throughput \
  --protocol v5 \
  --broker-url mqtt://127.0.0.1:1883 \
  --duration-sec 10 \
  --warmup-sec 2 \
  --payload-size 64 \
  --qos 1 \
  --publishers 1 \
  --subscribers 1
```

Supported workload groups:

- `codec encode|decode|roundtrip`
- `client throughput|latency|connections`

Every workload prints a single JSON object with:

- `schema_version`, `run_id`, `scenario`
- `config`
- `metrics`
- `samples`
- `environment`

## Scenario Runner

Named scenarios live in `benchmarks/scenarios/*.toml`.

Run a scenario repeatedly in the current worktree:

```bash
python3 benchmarks/runner.py run \
  --scenario codec-v5-publish-roundtrip \
  --runs 5 \
  --warmup-runs 1
```

Compare a scenario across two git refs:

```bash
python3 benchmarks/runner.py compare \
  --scenario codec-v5-publish-roundtrip \
  --baseline-ref main \
  --target-ref HEAD \
  --runs 12 \
  --warmup-runs 1
```

The runner uses `cargo run --release` by default. Use `--cargo-profile dev`
only when debugging the harness itself.

For client scenarios, pass `--broker-url mqtt://host:port`. For TLS, use
`mqtts://host:port` and pass `--ca-cert /path/to/ca.pem`.

Each scenario declares:

- `description`
- `primary_metric`
- `higher_is_better`
- `requires_broker`

The runner validates these fields and fails if a benchmark result does not
include the scenario's primary metric.

The runner writes generated reports under `benchmarks/results/`:

- `summary.json`
- `summary.csv`
- `report.html`

Reports classify comparisons with the scenario's metric direction:
`improvement`, `regression`, or `inconclusive`.

## Output Contract

`rumqtt-bench` emits JSON schema version 1. The machine-readable contract is
`benchmarks/schema/rumqtt-bench-output-v1.schema.json`. The runner validates the
same stable top-level shape:

- `schema_version` must be `1`
- `run_id` and `scenario` are non-empty strings
- `started_at_unix` and `finished_at_unix` are Unix timestamp integers
- `config`, `metrics`, `samples`, and `environment` are objects
- every metric value is numeric
- every sample series is an array of numbers

## Running Meaningful Benchmarks

Read `benchmarks/BENCHMARKING.md` before using results for performance claims.
In short: use release mode, an idle machine, stable CPU frequency, consistent
broker placement, warmup runs, and enough repeated runs to avoid drawing
conclusions from noise.

## CI Contract

CI should compile the benchmark package and run codec smoke tests. Broker-backed
performance measurements are intentionally manual or scheduled on controlled
hardware because they are too noisy for normal PR checks.
