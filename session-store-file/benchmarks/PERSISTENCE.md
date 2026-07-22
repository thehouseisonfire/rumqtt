# Persistent-session performance benchmarks

These workloads characterize the complete MQTT 3.1.1 and MQTT 5 file-backed
session path. They do not relax checksum validation, bounded reads, FIFO
coordination, file synchronization, atomic replacement, or namespace
synchronization. Durable persistence is expected to cost more than memory-only
operation and is not an application outbox.

## Methodology

Run with the release profile on an otherwise idle machine. The JSON output
records the Rust toolchain, target, OS, architecture, CPU count, exact workload
configuration, raw latency samples, and summary percentiles. Record the
filesystem, mount options, storage device, CPU governor, broker version, and
benchmark command alongside retained results.

Envelope and codec workloads are CPU microbenchmarks. They use deterministic
bytes, warm up through repeated execution, call `black_box`, and complete every
operation before timing stops. File-store workloads invoke the production
coordinator and backend. Save, clear, and quarantine therefore include their
required synchronization. Repeated valid loads describe a warm page cache.
The harness does not drop the host page cache; cold-cache results must be
reported separately when obtained by a defensible platform-specific method.

The MQTT workload uses plain TCP and tracked publish notices. Startup, initial
session reconciliation, warmup, and teardown are outside the measured window.
Enabled and disabled runs keep protocol, broker, payload, inflight limit,
message count, runtime, build profile, and machine constant. MQTT byte
accounting performs one benchmark-only canonical encode before the adapter's
production encode; the JSON calls out this overhead. Barrier duration is the
observable `SessionStore::save` future duration and includes adapter encoding,
coordination, blocking-task scheduling, envelope work, and durable filesystem
commit. Dependency-private commit stages are not attributed individually.

## Fixtures

Canonical fixtures use client IDs `benchmark-client-v4` and
`benchmark-client-v5`, topic `bench/persistence`, sequential nonzero packet
identifiers, alternating DUP flags, no retain, and deterministic bytes cycling
through 0..250. The inflight matrix is 0, 1, 10, 100, and 1,000 entries. Both
QoS 1 and QoS 2 are accepted. The standard growth run uses 1 KiB per publish;
payload scaling uses 0 B, 1 KiB, 16 KiB, 256 KiB, 1 MiB, and 4 MiB without
padding outside the canonical session.

## Commands

```bash
store_bench=(cargo run --release --manifest-path session-store-file/Cargo.toml \
  -p session-store-file-benchmarks --bin rumqtt-session-store-file-bench --)
"${store_bench[@]}" persistence envelope --mode encode --payload-size 1048576
"${store_bench[@]}" persistence codec --protocol v4 --mode encode \
  --inflight 100 --payload-size 1024 --qos 1
"${store_bench[@]}" persistence codec --protocol v5 --mode decode \
  --inflight 100 --payload-size 1024 --qos 2
"${store_bench[@]}" persistence file-store --operation save-replace --payload-size 1048576
"${store_bench[@]}" persistence coordination --concurrency 8 --operations 100
"${store_bench[@]}" persistence growth --protocol v5 --payload-size 1024 --qos 2
python3 benchmarks/runner.py run \
  --scenario persistence-mqtt-v4-qos1-enabled \
  --broker-url mqtt://127.0.0.1:1883 --runs 5 --warmup-runs 1
```

Run every file-store operation by selecting `save-create`, `save-replace`,
`save-growing`, `save-shrinking`, `load-present`, `load-missing`,
`clear-present`, `clear-missing`, `inspect-present`, `inspect-missing`,
`quarantine-present`, or `quarantine-missing`. Run coordination twice, with and
without `--different-keys`.

## Baseline report template

Record envelope and canonical codec throughput separately from durable save and
load latency. For filesystem operations and MQTT barriers report sample count,
p50, p95, p99, and maximum. Growth tables must include protocol, inflight,
QoS, application payload bytes, canonical bytes, the 22-byte envelope overhead,
and total checkpoint bytes. MQTT reports should include save count, submitted
checkpoint bytes, logical state changes, throughput, publish latency, and
barrier latency.

For an isolated successful outgoing exchange, the expected production save
sequence is admission plus terminal completion for QoS 1 (two saves), and
admission, durable PUBREL transition, plus terminal completion for QoS 2
(three saves). The admission checkpoint stores the recovery PUBLISH with
`DUP = 1`; the uninterrupted first wire transmission remains `DUP = 0`.
Batching can share independently required checkpoints and produce lower counts.

Results are specific to the measured hardware and filesystem. File and
directory synchronization dominate on many systems. Do not infer physical
flash write amplification from submitted checkpoint bytes. Windows, macOS,
edge-device, and cold-cache numbers remain unknown until run on those actual
environments.
