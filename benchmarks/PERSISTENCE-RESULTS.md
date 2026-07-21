# Persistent-session baseline: 2026-07-21

This is one local characterization, not a performance guarantee. The machine
was an x86_64 Linux 7.1.3 system with an Intel Core i5-13500H, 16 logical CPUs,
and a Solidigm NVMe SSD. The checkout was on Btrfs with `noatime`, zstd level 3
compression, and SSD/discard options. Rust was 1.96.1; the workspace MSRV is
1.89. Release builds used workspace LTO and one codegen unit.

The resolved persistence dependencies included `atomic-write-file` 0.3.0,
CRC32C 0.6.8, Tokio 1.53.0, and BLAKE3 1.8.5. Broker-backed runs used local
Mosquitto 2.1.2 over plain loopback TCP. Loads were warm-cache. CPU governor and
physical flash amplification were not measured.

## Microbenchmarks

| Operation | Shape | p50 | p95 | p99 |
| --- | ---: | ---: | ---: | ---: |
| Envelope encode | 1 MiB | 85.2 µs | 197.8 µs | 204.0 µs |
| Envelope decode | 1 MiB | 264.7 µs | 344.3 µs | 364.9 µs |
| CRC32C | 1 MiB | 172.4 µs | 210.6 µs | 223.9 µs |
| v4 codec encode | 100 QoS 1 × 1 KiB | 13.9 µs | 18.8 µs | 48.4 µs |
| v4 codec decode | 100 QoS 1 × 1 KiB | 13.7 µs | 15.0 µs | 18.9 µs |
| v5 codec encode | 100 QoS 2 × 1 KiB | 6.6 µs | 10.4 µs | 31.6 µs |
| v5 codec decode | 100 QoS 2 × 1 KiB | 15.3 µs | 17.7 µs | 19.7 µs |

The codec results are single runs and differences between protocol encoders
must not be overinterpreted. Envelope decode includes bounded reads, trailing
data probing, and checksum validation.

A later 20-sample payload sweep (three operations per sample) produced these
medians. It was run after the store optimization and is a separate run from the
1 MiB baseline above; the differing 1 MiB values illustrate cache and frequency
noise rather than a before/after comparison.

| Payload | Encode p50 | Decode p50 | CRC32C p50 |
| ---: | ---: | ---: | ---: |
| 0 B | 51 ns | 33 ns | 9 ns |
| 1 KiB | 247 ns | 237 ns | 90 ns |
| 16 KiB | 2.76 µs | 3.20 µs | 2.35 µs |
| 256 KiB | 19.5 µs | 52.0 µs | 34.8 µs |
| 1 MiB | 180.9 µs | 106.2 µs | 59.2 µs |
| 4 MiB | 368.3 µs | 464.1 µs | 234.9 µs |

The checksum-mismatch path over a 1 MiB envelope measured 498.2/512.7/657.2
µs p50/p95/p99 in a separate 30-sample run. The benchmark command also
exercises production bounded parsing; exhaustive trailing-byte and size-limit
behavior remains covered by core correctness tests rather than timed here.

## Durable store operations

| Operation | Shape | p50 | p95 | p99 |
| --- | ---: | ---: | ---: | ---: |
| Create | 1 KiB | 19.9 µs | 24.3 µs | 26.7 µs |
| Replace, before optimization | 1 MiB | 324.2 µs | 587.6 µs | 623.5 µs |
| Warm load | 1 MiB | 155.3 µs | 279.7 µs | 494.8 µs |
| Clear present | 1 KiB | 14.3 µs | 15.8 µs | 16.8 µs |
| Inspect present | 1 KiB | 10.1 µs | 10.9 µs | 14.7 µs |
| Quarantine present | 1 KiB | 15.9 µs | 18.6 µs | 23.6 µs |
| Grow replacement | 1 MiB | 481.9 µs | 667.7 µs | 694.4 µs |
| Shrink replacement | 1 MiB | 418.0 µs | 603.7 µs | 653.0 µs |
| Load absent | missing | 10.9 µs | 14.5 µs | 19.6 µs |
| Clear absent | missing | 10.1 µs | 13.4 µs | 13.6 µs |
| Inspect absent | missing | 10.4 µs | 12.6 µs | 13.9 µs |
| Quarantine absent | missing | 13.1 µs | 18.8 µs | 59.8 µs |

These unusually low synchronization latencies are specific to this Btrfs/NVMe
host and its cache/controller state. They are not evidence that synchronization
is generally cheap. The growing/shrinking and missing-path rows are later,
30-sample runs and should not be treated as paired with the original rows.

Eight workers performing missing-checkpoint inspections reached 181,210 ops/s
on one key (41.8 µs p50 submission-to-completion) and 385,986 ops/s across
different keys (15.3 µs p50). This confirms useful different-key concurrency;
the aggregate timing cannot separate scheduler wait from metadata service time.

## Checkpoint growth

With 1 KiB application payloads, v4 QoS 1 grew from 73 bytes for an empty
checkpoint to 1,055,073 bytes at 1,000 inflight publishes. V5 QoS 2 grew from
79 to 1,056,079 bytes. Approximate per-entry growth in these fixtures was 1,055
bytes for v4 and 1,056 bytes for v5. This is fixture-specific, not a universal
linear model; properties, topics, control packets, and acknowledgement state
change the result.

## MQTT persistence behavior

### Conservative replay-PUBLISH checkpoint optimization

On 2026-07-21, the isolated inflight-1 MQTT fixtures were run immediately
before and after changing admission checkpoints to store outgoing QoS 1/2
PUBLISH recovery packets with `DUP = 1`. Each run used 100 measured messages,
10 warmup messages, a 1 KiB payload, the production file-backed store, and
local Mosquitto 2.1.2 on loopback. File and directory synchronization remained
enabled.

| Workload | Saves/message before → after | Throughput msg/s before → after | Submitted bytes before → after | Barrier p50/p95/p99 before → after |
| --- | ---: | ---: | ---: | ---: |
| v4 QoS 1 | 3 → 2 | 9,937 → 16,715 | 239,500 → 124,300 | 22.1/26.1/39.4 µs → 15.4/17.8/20.5 µs |
| v4 QoS 2 | 4 → 3 | 6,287 → 9,544 | 249,300 → 134,300 | 23.8/28.5/54.4 µs → 14.4/27.9/63.5 µs |
| v5 QoS 1 | 3 → 2 | 9,939 → 14,860 | 240,900 → 125,800 | 21.5/26.0/31.6 µs → 15.2/22.8/37.2 µs |
| v5 QoS 2 | 4 → 3 | 8,122 → 7,489 | 251,900 → 136,200 | 14.3/28.5/45.9 µs → 23.4/27.9/34.0 µs |

Final checkpoint sizes before/after were 95/94 bytes (v4 QoS 1), 95/95 bytes
(v4 QoS 2), 99/101 bytes (v5 QoS 1), and 101/101 bytes (v5 QoS 2). The
structural codec and file envelope did not change; small final-size differences
reflect the benchmark's terminal session metadata, not a new format.

The matching persistence-disabled context runs measured 33,227 → 32,863 msg/s
for v4 QoS 1 and 23,895 → 18,039 msg/s for v5 QoS 2. These are single, short
loopback samples. The v5 QoS 2 enabled result did not improve despite one fewer
durable save, and the disabled result also moved substantially, so no
proportional throughput claim is warranted.

Additional enabled post-change runs demonstrate checkpoint sharing at higher
inflight limits. V4 QoS 1 made 200 saves at both inflight 10 and 100 (15,346
and 13,667 msg/s; 1,073,800 and 10,568,800 submitted bytes). V5 QoS 2 made 220
and 210 saves (239 and 2,022 msg/s; 626,030 and 1,990,950 submitted bytes). The
approximately 40 ms broker/TCP acknowledgement behavior noted below strongly
affects these higher-inflight figures.

The paired command shape was:

```bash
cargo run --quiet --release -p benchmarks --bin rumqtt-bench -- persistence mqtt \
  --protocol v4 --qos 1 --persistence enabled \
  --broker-url mqtt://127.0.0.1:18883 --messages 100 --warmup-messages 10 --inflight 1
```

Protocol, QoS, persistence mode, and inflight were varied for the other rows.
Removing the DUP-promotion save reduced submitted full-checkpoint bytes, but it
did not alter synchronization semantics, the PUBREL barrier, terminal barriers,
or persistence-disabled execution.

At inflight 1, v4 QoS 1 completed 32,496 messages/s without persistence and
11,930 messages/s with persistence. The enabled run made exactly three saves
per publish; barrier p50/p95/p99 were 18.5/21.9/38.8 µs.

V5 QoS 2 at inflight 1 completed 19,397 messages/s without persistence and
7,277 messages/s with persistence. It made four saves per publish;
barrier p50/p95/p99 were 19.9/25.2/34.9 µs.

A later enabled-only run, after adding final-size reporting, ended with a
94-byte v4 checkpoint and a 99-byte v5 checkpoint. Its v4 barrier distribution
was 16.1/26.9/36.3 µs and its v5 distribution was 18.5/24.2/42.2 µs. These
are separate runs and are not substituted into the paired baseline above.

At inflight 10, persistence-disabled loopback runs exhibited approximately
40 ms acknowledgement batching while enabled barriers changed packet timing.
Those results are retained in transient output but are not used as the primary
enabled/disabled comparison. This is an unresolved broker/TCP interaction, not
evidence that persistence improves throughput.

For v4 QoS 1, 100 messages submitted about 0.24 MiB of checkpoints at inflight
1, 1.66 MiB at inflight 10, and 15.9 MiB at inflight 100. Full checkpoints are
written for each save; there is no delta encoding or coalescing. Save frequency
and checkpoint growth therefore dominate logical write amplification under
protocol traffic.

## Optimization

The baseline showed that 1 MiB envelope construction (85.2 µs p50) was a
material part of a 324.2 µs durable replacement. Production saves now compute
the same CRC incrementally and write the header, payload, and checksum directly
to the same atomic writer/Windows staging handle, avoiding the redundant full
envelope allocation. File and directory synchronization and atomic replacement
are unchanged.

After the change, the 1 MiB replacement measured 311.3 µs p50, 481.6 µs p95,
501.5 µs p99, and 334.5 µs mean, versus 324.2/587.6/623.5/382.8 µs before.
The p50 change is small enough to be noise-sensitive, but the allocation is
provably removed and the observed mean/tail moved in the expected direction.
The 1 KiB path remained in the same tens-of-microseconds range.

## Unmeasured areas

No cold-cache, macOS, native Windows, edge-device, CPU-utilization, allocation
count, or physical-device write-amplification measurements were obtained.
Dependency-private commit stages, blocking-pool scheduling delay, and
coordination wait could not be timed independently without invasive hooks.
Run the documented suite on those actual platforms rather than simulating them
with sleeps.
