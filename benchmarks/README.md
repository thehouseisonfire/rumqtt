# Benchmarks

`benchmarks` is the maintained performance harness for this workspace. It is a
Cargo package named `benchmarks` with a Rust workload binary, a synthetic-router
binary, and Python orchestration tools.

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

- `codec encode|decode|roundtrip` for MQTT v4, MQTT v5, and the NATS PUB
  wire-format baseline
- `codec validation-cost` for a paired MQTT v4/v5 experiment that measures
  checked PUBLISH encoding against encoding of an already validated topic
- `client throughput|latency|connections`
- `client publish-path` for paired checked/`ValidatedTopic` measurement through
  the public API, event loop, state machine, encoder, and an in-process sink
- `options parse-url`

MQTT-specific file-backed persistence workloads live in the independent
[`session-store-file` benchmark package](../session-store-file/benchmarks/README.md).
Protocol-neutral storage benchmarks live in the standalone
[`atomic-blob-store`](https://github.com/thehouseisonfire/atomic-blob-store)
project.

## Matched Library Comparison

`rumqtt-library-bench` runs one client library per process. Shared code owns
payloads, topics, Correlation Data sequence/timestamps, pacing, timing,
delivery accounting, resource sampling, and drain behavior; the adapters only
perform MQTT operations. The comparison uses workspace `rumqttc-v5-next` and
the exact locked dependency `mqtt5 = 0.38.0`. Matched scenarios support TCP
(`mqtt://`) and TLS (`mqtts://`); WebSocket is not supported by both adapters.
For a TLS broker using a private CA, pass `--ca-cert /path/to/ca.pem`.

```bash
python3 benchmarks/runner.py compare-libraries \
  --scenario matched-v5-throughput-qos1-1kib-1p1s \
  --broker-url mqtt://127.0.0.1:1883 \
  --runs 12 \
  --warmup-runs 1
```

The runner alternates backend order and classifies the paired bootstrap
interval against a predeclared ±5% practical-equivalence band. A run with
loss, duplicates, malformed traffic, a rejected publish, or incomplete drain
is retained in raw output but excluded from valid pairs. For a smoke check,
use one development-profile run; shared CI runners are not performance
evidence.

The maintained matched catalog is representative, not exhaustive. It covers
QoS 0/1 throughput at 64 B, 1 KiB, and 16 KiB; QoS 1 fan-in and fan-out;
QoS 0/1 latency at 100 and 1000 messages/s; serial and 10-concurrent
connection churn; and one private-CA TLS smoke. Scenario files explicitly
declare timing, flow control, timeouts, transport, topics, and quality gates.

Matched message runs report harness-owned publish operations outstanding at
the deadline, their measurement peak, and those remaining after bounded drain.
These span shared-window acquisition through return from the adapter's public
publish operation. The deadline count is derived from timestamped publish
completion observations, so drain-period completions cannot reduce it. These
metrics are not internal queue or socket-write depths.
Connection runs report attempts, successful CONNACK-plus-disconnect cycles,
stable connect/disconnect failure classes, and cycles in flight at the
deadline. Failures, timeouts, zero successes, or incomplete drain invalidate a
run.

Add `cargo_features = ["alloc-metrics"]` to a matched scenario to enable
counting-system-allocator metrics. Allocation instrumentation is off by
default because it perturbs the workload.

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

NATS codec workloads use `--protocol nats --qos 0`. They measure the small
in-repo NATS `PUB` frame encoder/parser, not the full `async-nats` client and
not equivalent protocol features:

```bash
cargo run -p benchmarks --bin rumqtt-bench -- \
  codec roundtrip --protocol nats --qos 0 --messages 100000 --payload-size 64
```

### PUBLISH Topic Validation Cost

Use `validation-cost` to measure the upper-bound benefit of carrying a
prevalidated topic through the encoder. The benchmark-only path skips exactly
the codec's topic-name scan. Packet length, QoS/DUP and packet identifier
checks remain enabled; MQTT 5 property validation also remains enabled.

```bash
cargo run --release -p benchmarks --bin rumqtt-bench -- \
  codec validation-cost \
  --protocol v5 \
  --rounds 10 \
  --messages 1000000 \
  --topic bench/codec \
  --payload-size 64 \
  --qos 1
```

The two variants are run in alternating order and reported as paired elapsed
samples. `validation_share_percent` estimates the checked encoder time spent
on the skipped scan, while `prevalidated_speedup_percent` reports throughput
improvement. This is a codec microbenchmark and therefore an upper bound, not
evidence of the same improvement in a broker-backed client workload.

### ValidatedTopic Client Publish Path

`client publish-path` is the primary deterministic measurement for the
`ValidatedTopic` optimization. It alternates raw and prevalidated topic inputs,
constructs all inputs before timing, and drives every publish through the public
client API, event loop, state machine, encoder, and an in-process MQTT peer. It
reports raw samples, medians, and a paired bootstrap confidence interval.

```bash
cargo run --release -p benchmarks --bin rumqtt-bench -- \
  client publish-path \
  --protocol v5 \
  --warmup-rounds 1 \
  --rounds 12 \
  --messages 100000 \
  --topic bench/client \
  --payload-size 0 \
  --qos 0
```

For acceptance, run both v4 and v5, QoS 0 and 1, payload sizes 0, 64,
1024, and 4096 bytes, and both a roughly 12-byte and 160-byte topic. Use at
least one warmup round followed by 12 measured rounds. Retain the JSON
output and repeat the equivalent `codec validation-cost` matrix as the
theoretical upper bound. CI smoke runs confirm functionality only and must not
assert a performance ratio.

Broker-backed `client throughput` TCP and TLS scenarios remain realism checks.
Their results must be reported separately because network and broker work can
conceal the client-side delta.

#### Negative production experiment (2026-08-10)

An end-to-end provenance implementation was measured with Rust 1.96.1
(`31fca3adb 2026-06-26`) on `x86_64-unknown-linux-gnu`. The deliberately
favorable MQTT v4 case used a 160-byte topic, empty payload, QoS 0, 100,000
publishes, one warmup pair, and 12 measured pairs:

```bash
cargo run --release -q -p benchmarks --bin rumqtt-bench -- \
  client publish-path --protocol v4 --warmup-rounds 1 --rounds 12 \
  --messages 100000 --payload-size 0 --qos 0 \
  --topic aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
```

The checked-path median was 0.0838935455 seconds and the prevalidated-path
median was 0.0910975675 seconds: a 8.169% regression, with a paired 95%
bootstrap interval of 6.308% to 10.591% regression. This failed the first
realistic client-path acceptance attempt decisively, so the production
provenance plumbing was removed as required by the experiment plan. The wider
protocol/QoS/payload matrix, broker-backed realism checks, and artifact-size
comparison were not run because the candidate was already ineligible for
retention. The benchmark remains available for future implementations; the
direct `codec validation-cost` result should still be interpreted only as a
theoretical ceiling.

## Codec Profiling

On POSIX systems, build with the optional `profiling` feature and the
symbol-preserving Cargo profile to capture a pprof protobuf:

```bash
cargo run --profile profiling -p benchmarks --features profiling \
  --bin rumqtt-bench -- \
  codec decode --protocol v5 --messages 1000000 --payload-size 64 \
  --qos 1 --profile-output /tmp/rumqtt-codec.pb --profile-frequency 100

go tool pprof /tmp/rumqtt-codec.pb
```

Profiling changes timing and is intended for bottleneck diagnosis, not
regression reports.

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

Compare a compatible MQTT v5 client scenario against an installed
`mqttv5-cli` executable:

```bash
python3 benchmarks/runner.py compare-external \
  --scenario client-v5-throughput-qos1 \
  --external-bin mqttv5 \
  --broker-url mqtt://127.0.0.1:1883 \
  --runs 12 \
  --warmup-runs 1
```

The external adapter supports TCP/TLS throughput, fixed-rate 1000 msg/s
latency, and connection scenarios. It records the resolved executable and
`mqttv5 --version`, validates the tool's runtime JSON, normalizes comparable
metrics into schema version 1, and alternates execution order. mqttv5-cli is
not installed or updated by this repository.

`compare-external` remains an application-level CLI comparison. Do not combine
its results with the direct-library `compare-libraries` reports.

The runner uses `cargo run --release` by default. Use `--cargo-profile dev`
only when debugging the harness itself.

For client scenarios, pass `--broker-url mqtt://host:port`. For TLS, use
`mqtts://host:port` and pass `--ca-cert /path/to/ca.pem`.
For websocket scenarios, use `ws://host:port/path`; the scenario automatically
enables the benchmark crate `websocket` feature.

## Broker Fixture

Use the broker fixture when you need a reproducible local Mosquitto for
broker-backed validation. The default backend is Docker with
`eclipse-mosquitto:2.0.22`, configured with TCP, TLS, and websocket listeners on
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
directory. Mosquitto validation retains the exact effective configuration as
`broker-config/mosquitto.conf` with its SHA-256 digest. Common broker metadata
records normalized listeners, transports, persistence, anonymous access, TLS
certificate mode, image tag/digest, and sorted EMQX environment overrides
without retaining private keys or credentials.
`selected_transports` identifies the workloads run, while `active_transports`
and `listeners` are derived from the effective broker configuration and include
listeners that were enabled but not selected by those workloads.

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

For client-side isolation without Mosquitto, use the in-repo synthetic router:

```bash
python3 benchmarks/broker_fixture.py validate \
  --backend synthetic \
  --transport tcp \
  --scenario client-v4-throughput-qos1 \
  --scenario client-v5-throughput-qos1 \
  --runs 1 \
  --warmup-runs 0 \
  --cargo-profile dev \
  --output-dir /tmp/rumqtt-bench-synthetic
```

The synthetic backend supports MQTT 3.1.1/5 TCP CONNECT, subscriptions,
QoS 0/1 publish routing, acknowledgements, keepalive, and disconnect. It
intentionally does not implement authentication, persistence, QoS 2, TLS, or
WebSockets and is not a production broker.
Its deterministic negative-PUBACK rejection fault is MQTT-5-only because MQTT
3.1.1 has no negative PUBACK reason code; triggering it from a v3.1.1
connection closes that connection with an explicit router error and emits no
malformed acknowledgement.

Matched smoke tests can also use either pinned production broker:

```bash
python3 benchmarks/broker_fixture.py validate \
  --backend docker --broker mosquitto --transport tcp \
  --scenario matched-v5-throughput-qos1-1kib-1p1s

python3 benchmarks/broker_fixture.py validate \
  --backend docker --broker emqx --transport tcp \
  --scenario matched-v5-throughput-qos1-1kib-1p1s
```

The fixtures pin `eclipse-mosquitto:2.0.22` and `emqx/emqx:5.9.3`. EMQX
summaries also record the locally resolved image digest.

Override the Docker image with `RUMQTT_BENCH_MOSQUITTO_IMAGE`. A system
Mosquitto can be used for TCP/TLS fallback with `--backend system`, but local
Mosquitto packages are often built without websocket support. If websocket
scenarios are selected, the fixture probes the binary and fails with a clear
message when `protocol websockets` is not supported.

The maintained scenario set covers:

- codec encode, decode, and roundtrip for MQTT 3.1.1 and MQTT 5 across 0 B,
  64 B, 1 KiB, and 16 KiB payloads
- NATS PUB wire-frame encode, decode, and roundtrip across the same payload
  sizes as a protocol-format baseline
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

The runner writes generated reports under the owning package's `results/`
directory: `benchmarks/results/` for client scenarios and
`session-store-file/benchmarks/results/` for persistence scenarios.

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

`rumqtt-library-bench` emits schema version 2, documented by
`benchmarks/schema/rumqtt-library-bench-output-v2.schema.json`. It adds the
selected client, normalized and effective configurations, and explicit
per-run validity.

## Running Meaningful Benchmarks

Read `benchmarks/BENCHMARKING.md` before using results for performance claims.
In short: use release mode, an idle machine, stable CPU frequency, consistent
broker placement, warmup runs, and enough repeated runs to avoid drawing
conclusions from noise.

## CI Contract

CI should compile the benchmark package and run codec smoke tests. Broker-backed
performance measurements are intentionally manual or scheduled on controlled
hardware because they are too noisy for normal PR checks.
