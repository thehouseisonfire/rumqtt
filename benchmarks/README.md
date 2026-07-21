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
- `options parse-url`
- `persistence envelope|codec|file-store|coordination|growth|mqtt`

Persistent-session methodology, fixture definitions, and baseline reporting are
documented in [`PERSISTENCE.md`](PERSISTENCE.md).

`options parse-url` requires the benchmark crate `url` feature:

```bash
cargo run -p benchmarks --features url --bin rumqtt-bench -- \
  options parse-url --protocol v5 --parses 100000
```

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
For websocket scenarios, use `ws://host:port/path`; the scenario automatically
enables the benchmark crate `websocket` feature.

## Broker Fixture

Use the broker fixture when you need a reproducible local Mosquitto for
broker-backed validation. The default backend is Docker with
`eclipse-mosquitto:2.0`, configured with TCP, TLS, and websocket listeners on
free localhost ports:

```bash
python3 benchmarks/broker_fixture.py validate \
  --transport all \
  --runs 1 \
  --warmup-runs 0 \
  --cargo-profile dev \
  --output-dir /tmp/rumqtt-bench-fixture
```

The fixture is intended for smoke validation, not statistical benchmarking. It
starts Mosquitto, runs selected scenarios through `benchmarks/runner.py`, writes
`broker-validation-summary.json`, and removes the broker container on success or
failure. The summary records the backend, Docker image, listener ports,
completed/failed/skipped scenarios, and each scenario's runner output
directory.

Run only the two websocket throughput scenarios:

```bash
python3 benchmarks/broker_fixture.py validate \
  --transport websocket \
  --scenario client-v4-throughput-websocket-qos1-1kib-1p1s \
  --scenario client-v5-throughput-websocket-qos1-1kib-1p1s \
  --runs 1 \
  --warmup-runs 0 \
  --cargo-profile dev \
  --output-dir /tmp/rumqtt-bench-websocket-fixture
```

Soak scenarios are skipped by default because they run for longer. Add
`--include-soak` when you explicitly want to validate them.

Override the Docker image with `RUMQTT_BENCH_MOSQUITTO_IMAGE`. A system
Mosquitto can be used for TCP/TLS fallback with `--backend system`, but local
Mosquitto packages are often built without websocket support. If websocket
scenarios are selected, the fixture probes the binary and fails with a clear
message when `protocol websockets` is not supported.

The maintained scenario set covers:

- codec encode, decode, and roundtrip for MQTT 3.1.1 and MQTT 5 across 0 B,
  64 B, 1 KiB, and 16 KiB payloads
- client throughput for MQTT 3.1.1 and MQTT 5 across QoS 0/1, representative
  payload sizes, and 1p1s, 4p1s, and 1p4s topologies
- bounded-rate latency tails with p50/p95/p99 metrics
- TCP and TLS connection churn
- sustained throughput soaks with collapse and RSS-growth diagnostics
- feature-sensitive TLS, websocket, and URL parsing scenarios

Each scenario declares:

- `description`
- `primary_metric`
- `higher_is_better`
- `requires_broker`
- optional `transport = "tcp" | "tls" | "websocket"` for client scenarios
- optional `cargo_features = [...]` for feature-gated scenarios
- `[quality]` gates for success rate, run count, primary-metric noise, and
  comparison CI width

The runner validates these fields and fails if a benchmark result does not
include the scenario's primary metric. When `transport` is set, the runner also
checks the broker URL scheme before starting the benchmark.

The runner writes generated reports under `benchmarks/results/`:

- `summary.json`
- `summary.csv`
- `report.html`
- `raw/current/*.json` for `run`
- `raw/baseline/*.json` and `raw/target/*.json` for `compare`

Reports classify comparisons with the scenario's metric direction:
`improvement`, `regression`, or `inconclusive`.

Latency metrics ending in `_us`, connection failures, connect latencies,
throughput-collapse percentage, and RSS growth are classified as
lower-is-better. Throughput, byte rate, parse rate, and connection rate are
classified as higher-is-better unless the scenario primary metric says
otherwise.

Branch comparisons are paired by measured run index after warmups. The runner
reports medians, MAD, CV, success rate, paired sample count, confidence interval
width, and scenario quality status. Quality failures are advisory in normal
runs: they are recorded as `pass`, `warn`, or `fail` but do not change the exit
code unless a benchmark command fails or emits invalid JSON.

Summaries include the command template, git refs, scenario file SHA-256, rustc,
OS, CPU count, and pointers to raw run records. Raw records keep the parsed
benchmark payloads so results can be audited without rerunning the benchmark.

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
