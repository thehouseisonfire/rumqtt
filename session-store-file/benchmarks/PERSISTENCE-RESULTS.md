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

### Executor-neutral engine check (2026-07-24)

After replacing the private Tokio runtime with the store-owned executor-neutral
engine, a 30-sample release run of 1 MiB streaming replacement measured
904.7 µs p50, 2.34 ms p95, and 2.49 ms p99. Eight tasks performing 100
different-key missing inspections each reached 273,234 operations/s with
20.4 µs p50, 73.3 µs p95, and 138.5 µs p99 submission-to-completion latency.

These are validation runs on the same general host/filesystem class described
above, but not tightly paired measurements. The durable replacement result is
therefore recorded as a regression sentinel rather than attributed solely to
the engine change. Correctness gates additionally verify lazy worker startup,
bounded active operations, explicit worker joining, and Tokio-free blocking
builds.

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
cargo run --quiet --release --manifest-path session-store-file/Cargo.toml \
  -p session-store-file-benchmarks --bin rumqtt-session-store-file-bench -- \
  persistence mqtt \
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

No cold-cache, macOS, native Windows, edge-device, CPU-utilization, or
physical-device write-amplification measurements were obtained.
Dependency-private commit stages and physical synchronization costs could not
be attributed independently. Run the documented suite on actual target
platforms rather than simulating them with sleeps.

## TODO2 executor-neutral store validation (2026-07-25)

The maintained harness was fixed at commit
`607aac41fe7161cc37c2c0c8ffcc593b3ff33d1f`. The final engine source was
`cae75823b18eb8cf118ab5dd55563693e29b448a`. Raw schema-version-1 JSON is in
`baselines/todo2-linux-2026-07-25/` and
`results/todo2-linux-2026-07-25/`.

Both runs used Rust 1.96.1 release builds with workspace LTO on Linux 7.1.4,
x86-64, and Btrfs with `noatime`, zstd level 3, SSD, and asynchronous discard.
File-store samples used 20 observations per payload/I/O combination over 0 B,
1 KiB, 16 KiB, 256 KiB, 1 MiB, and 4 MiB. Lifecycle/resource runs used 10
observations for 1, 2, and 8 stores at concurrency bounds 1, 4, and 8.
Coordination used 100 operations per submitting thread. Broker runs used local
Mosquitto, 100 measured messages, 10 warmups, and inflight 1.

The non-interleaved full sweep experienced substantial host-frequency and
scheduling drift between the early baseline and later final run. Representative
cases were therefore repeated with baseline and final binaries alternated on
the same loaded host. The median of three paired 256 KiB streaming-save
medians was 150.6 µs baseline and 151.3 µs final; paired p95 medians were
182.8 µs and 181.5 µs. Single-submitter different-key inspection medians were
13.1 µs and 13.5 µs, with p95 medians of 18.6 µs and 16.8 µs. These paired
repeats do not reproduce a material raw-I/O or scheduler regression.

Fresh-broker paired v4 barrier p50 ranges were 21.9–23.0 µs baseline and
20.2–21.0 µs final; p95 ranges were 32.3–38.1 µs and 26.5–31.0 µs. V5 p50
ranges overlapped at 19.1–25.4 µs and 21.5–22.7 µs. V5 p95 was noisier:
25.9–36.5 µs baseline and 36.0–38.6 µs final. Save counts remained exactly two
per v4 QoS 1 message and three per v5 QoS 2 message. The raw files retain all
throughput and checkpoint-byte values; the short loopback runs are not used to
claim a throughput improvement.

Thread ownership matched the configured bound in both versions. One store used
one idle coordinator and reached coordinator-plus-worker deltas of 2, 5, and 9
threads for concurrency 1, 4, and 8. Eight stores remained at eight idle
coordinators and no run exceeded eight coordinators plus eight workers per
store. For the one-store/concurrency-four case, final versus baseline medians
were 27.8/39.1 µs open, 3.49/3.30 ms cold four-key save, 5.6/8.8 µs flush, and
94.5/125.8 µs deterministic close. Close includes joining workers and the
coordinator.

The same resource case recorded 123 allocation calls and 16,789,469 allocated
bytes final versus 124 and 16,789,637 baseline during four simultaneous 4 MiB
saves. Peak process RSS was 55.9 MiB final and 51.8 MiB baseline. `VmHWM` is a
process-lifetime high-water mark and the allocation counter is process-global,
so the RSS difference is treated as allocator/runtime noise rather than a
store-retained-memory regression. Bounded-channel correctness for payloads
larger than a chunk is enforced separately by deterministic facade tests.

These measurements cover the local Unix implementation only. They do not
constitute native evidence for any other platform.

### Remaining TODO2 scenarios

The maintained harness now includes deterministic ordered-barrier,
slow-source/slow-destination backpressure, and MQTT v4/v5 recovery commands.
Their schema-version-1 output separates idle from queued barrier latency,
backpressure establishment from post-release completion, and adapter loading
from client-state application. A queued barrier is released only after the
coordinator reports accepting it. Slow endpoints remain gated until the worker
reports the stream's first empty input channel or full output channel
immediately before the blocking transport operation. Each completed stream is
a synchronization point, and the observation queue is drained before the next
timed operation. Destination fixture creation is outside the establishment
timer. No sleep is part of either synchronization protocol.

Paired release measurements for these commands are retained under
`baselines/todo2-completion-linux-2026-07-26/` and
`results/todo2-completion-linux-2026-07-26/`. They use the same completed
harness overlay for baseline `607aac41fe7161cc37c2c0c8ffcc593b3ff33d1f`
and final `cae75823b18eb8cf118ab5dd55563693e29b448a`, with 50 samples per
scenario, a 1 MiB streaming payload, four workers, and 100 inflight 1 KiB
publishes for recovery. Raw samples and exact environment metadata remain
authoritative; short host-local differences are treated as noise unless the
paired distributions show a consistent material shift.

Earlier first-poll, thread-completion, cross-sample-event, and
fixture-inclusive samples were invalidated and replaced.
Two alternating fixed-harness sweeps measured idle-barrier medians of
8.9–12.2 µs baseline and 10.7–16.8 µs final; queued-barrier medians were
516–695 µs and 661–964 µs. The barrier distributions do not show a consistent
regression.

With first-pressure events scoped to one per completed stream, slow-source
establishment medians were 60.9–75.8 µs baseline and 23.9–70.0 µs final;
post-release medians were 393–479 µs and 366–408 µs. Slow-destination
establishment medians, now excluding fixture creation, were 220–416 µs
baseline and 341–468 µs final; post-release medians were 81.4–161 µs and
132–192 µs. The destination p95 ranges were 272–648 µs versus 484–1,016 µs
for establishment and 101–250 µs versus 180–593 µs after release. The paired
median and tail ranges overlap and vary between repeats, so these short
loaded-host measurements do not support a material source- or
destination-side regression. Raw samples preserve the complete distributions
for a quieter-host follow-up.

V4/v5 adapter-load medians were 92.7/87.2 µs baseline and 71.7/85.1 µs final.
State application was 26.9/26.3 µs baseline and 38.0/46.1 µs final. All runs
restored exactly 100 replay entries. Client-state source also changed between
the historical commits, so application timing is reported as an end-to-end
recovery characteristic rather than attributed to the blob-store engine.
