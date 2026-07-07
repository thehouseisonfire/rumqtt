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

For client scenarios, pass `--broker-url mqtt://host:port`. For TLS, use
`mqtts://host:port` and pass `--ca-cert /path/to/ca.pem`.

The runner writes generated reports under `benchmarks/results/`:

- `summary.json`
- `summary.csv`
- `report.html`

## CI Contract

CI should compile the benchmark package and run codec smoke tests. Broker-backed
performance measurements are intentionally manual or scheduled on controlled
hardware because they are too noisy for normal PR checks.
