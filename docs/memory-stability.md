# Memory-stability client acceptance profile

This profile tests whether the MQTT 3.1.1 (`rumqttc-v4-next`) and MQTT 5
(`rumqttc-v5-next`) clients maintain bounded steady-state memory while one
process performs repeated equivalent work. It is a memory-correctness gate, not
an absolute throughput benchmark.

The existing [10 MiB low-memory profile](low-memory.md) remains a separate
acceptance test. The 16 MiB limit here is a safety boundary that leaves room to
observe gradual growth; stability is decided from equivalent post-warm-up round
measurements, not proximity to that ceiling.

## Reproduce the result

Run the official profile from the repository root on an x86_64 Linux host with
Docker and cgroup v2:

```bash
scripts/test-memory-stability.sh
```

The command builds both static client images, executes three independent runs
per client, calculates the acceptance metrics, and exits nonzero unless every
selected run passes. Each run has a fresh scratch client container, Mosquitto
container, network, topic namespace, and cgroup. Raw and summarized results are
preserved below a timestamped directory in
`target/memory-stability/results/`.

Diagnostic subsets and workloads are supported:

```bash
scripts/test-memory-stability.sh --client v4
scripts/test-memory-stability.sh --repeat 1
scripts/test-memory-stability.sh --rounds 30
scripts/test-memory-stability.sh --messages-per-round 10000
scripts/test-memory-stability.sh --memory-mib 20
```

Any scope or workload override is labeled `diagnostic` in console and
machine-readable output and does not replace the documented official profile.

## Exact profile

| Setting | Value |
| --- | --- |
| Host requirement | Linux, x86_64, Docker, cgroup v2 |
| Rust toolchain and target | Rust 1.85.0, `x86_64-unknown-linux-musl` |
| Build | Workspace release profile, locked dependencies, LTO, stripped, static musl 1.2.5 |
| Client features | `default-features = false`; no optional features |
| Runtime and transport | Tokio current-thread; plain TCP with TCP_NODELAY |
| Broker | Mosquitto 2.0.22 in a fresh, separate, unconstrained container |
| Measured process | One client binary as PID 1 in a `scratch` container |
| Cgroup limit | `memory.max=16777216`; `memory.swap.max=0` |
| Request and inflight limits | Four request-channel entries; four outgoing QoS 1 publishes |
| Batching | At most four client requests and four incoming packets per batch |
| Packet and socket limits | 1,024-byte packets; requested 4,096-byte send/receive socket buffers |
| Payload | 128 deterministic bytes containing phase, round, message index, and validated filler |
| Keep-alive and connection timeout | Three seconds and five seconds |
| Warm-up | Five complete rounds |
| Measurement | Twenty complete rounds |
| Work per round | 5,000 QoS 1 self-publishes in four-message waves |
| Reconnects | After warm-up round 5 and measured rounds 5, 10, 15, and 20 |
| Subscription churn | One temporary-topic subscribe/SUBACK and unsubscribe/UNSUBACK per round |
| Continuous sampling | `memory.current` nominally every 100 ms |
| Boundary measurement | Verify logical idle; settle 250 ms; median of five subsequent samples |
| Repetitions | Three fresh runs per client |

Mosquitto uses the same configuration and pinned image as the low-memory
profile. Persistence is disabled, broker inflight and queued-message limits are
four, and its packet limit is 1,024 bytes.

The stability clients enable TCP_NODELAY to keep the deliberately small
four-message waves from being dominated by delayed-ACK/Nagle pauses. An
incomplete trial with the default setting took about 55 seconds per 5,000
messages; enabling TCP_NODELAY makes the sustained official workload practical
without changing its message count, inflight bound, acknowledgement checks, or
the existing low-memory binaries.

## Workload phases

The client first connects and completes its primary QoS 1 subscription.
Warm-up rounds exercise the same publishing, echo reception, acknowledgements,
and temporary subscription churn as measured rounds. Warm-up also observes both
`PINGREQ` and `PINGRESP`, then forces a broker restart, detects connection loss,
reconnects, and resubscribes. Startup memory is never used as the baseline.

Every measured round sends 5,000 messages in four-message waves. A wave is
complete only after all four outgoing publishes were emitted, all expected
`PUBACK` packets arrived, all echoed publishes matched the exact topic and
payload, and the client emitted the corresponding incoming-publish
acknowledgements. The temporary subscription must then receive a successful
`UNSUBACK`.

After every fifth measured round, the client emits a deterministic broker
restart marker. The host restarts Mosquitto while continuing cgroup sampling.
The round does not finish until the client observes connection loss, reconnects,
receives a new `CONNACK`, and completes primary-topic resubscription.

At every round boundary the client requires a connected transport; empty
request, replay, and control queues; zero inflight publishes and reserved packet
identifiers; no pending subscribe or unsubscribe; no pending incoming QoS 1
acknowledgement; and `outbound_drained=true`. Only then does it emit the idle
marker and hold the equivalent state for host sampling. The hold does not
determine completion.

## Meaning of the measurements

The authoritative measurements are Linux cgroup v2 counters for the scratch
client container, not process RSS. `memory.current` includes userspace memory
and kernel memory charged to that cgroup, including allocator state, executable
pages, Tokio and MQTT state, stack, socket memory, and slab. The broker, Docker
daemon, and host runner are outside the measured cgroup.

Each run preserves:

- The complete timestamped `memory.current` CSV and per-round median CSV.
- `memory.peak`, `memory.events`, `memory.events.local` when available, and
  `memory.stat`, including snapshots at every logical round boundary.
- Client, broker, and broker-restart logs; Docker inspection and exit state.
- Per-run JSON/text reports and aggregate CSV/JSON/text summaries.
- Duration, validated echoes, acknowledgements, reconnects, subscription churn,
  round durations, and approximate validated echoes per second.

Allocator retention after temporary work is allowed. The comparison asks
whether equivalent rounds continue to accumulate charged memory.

## Stability criteria

For measured round steady-state values \(y_1 \ldots y_n\):

- `baseline` is the median of rounds 1 through 5.
- `ending` is the median of the final five rounds.
- `growth = ending - baseline`.
- `growth_percent = 100 * growth / baseline`.
- The least-squares slope fits round numbers `1..n` against the round medians.
- `projected_trend = max(0, slope) * (n - 1)`.
- `allowed_growth = max(1 MiB, 10% of baseline)`.

A run passes only when the full MQTT scenario and every expected counter and
idle boundary succeed; Docker did not report OOM kill; cgroup `max`, `oom`, and
`oom_kill` events remain zero; the process exits zero; the peak stays within
16 MiB; growth is no greater than `allowed_growth`; and the projected positive
trend is no greater than `allowed_growth`. Negative slope is accepted.

Results are classified individually as `Pass`, `Fail — growth`,
`Fail — memory`, `Fail — functional`, or `Inconclusive`. The automated
aggregate passes only if all three official runs pass. A maintainer may
separately record an acceptance conclusion when preserved diagnostic evidence
demonstrates that an isolated failure came from the measurement environment
rather than continuing client growth.

## Measured result

The official six-run profile was recorded on 2026-07-29. MQTT 5 passed all
three runs. MQTT 3.1.1 passed two runs and the unchanged calculator classified
one run as `Fail — growth`; that run remains visible below rather than being
hidden by an average.

The v4 failure did not reproduce in either adjacent official run or a fresh
full-size diagnostic run. Its detailed client state and `memory.stat`
categories remained flat while only `memory.current` moved. The maintainer
therefore accepts both clients as satisfying the practical bounded-memory
requirement, with v4 carrying a documented cgroup-accounting outlier. This
acceptance conclusion does not rewrite the raw per-run classification or
loosen the formula.

| Client | Run | Result | Baseline | Ending level | Growth | Projected trend | Peak | OOM-killed | Messages/s |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | --- | ---: |
| MQTT 3.1.1 / v4 | 1 | Pass | 3,198,976 | 2,916,352 | −282,624 (−8.83%) | 0 | 8,040,448 | No | 341.3 |
| MQTT 3.1.1 / v4 | 2 | Fail — growth | 3,231,744 | 5,029,888 | +1,798,144 (+55.64%) | 780,288 | 8,101,888 | No | 342.8 |
| MQTT 3.1.1 / v4 | 3 | Pass | 3,076,096 | 3,063,808 | −12,288 (−0.40%) | 0 | 10,440,704 | No | 372.3 |
| MQTT 5 / v5 | 1 | Pass | 4,251,648 | 3,743,744 | −507,904 (−11.95%) | 0 | 9,031,680 | No | 411.8 |
| MQTT 5 / v5 | 2 | Pass | 3,272,704 | 2,990,080 | −282,624 (−8.64%) | 198,890 | 8,253,440 | No | 409.8 |
| MQTT 5 / v5 | 3 | Pass | 2,592,768 | 3,207,168 | +614,400 (+23.70%) | 0 | 8,413,184 | No | 361.6 |

| Client | Overall result | Passing runs | Peak range | Growth range | Throughput range |
| --- | --- | ---: | ---: | ---: | ---: |
| MQTT 3.1.1 / v4 | **Pass with documented accounting outlier** | 2/3 official; full reproduction passed | 8,040,448–10,440,704 | −282,624–+1,798,144 | 341.3–372.3/s |
| MQTT 5 / v5 | **Pass** | 3/3 | 8,253,440–9,031,680 | −507,904–+614,400 | 361.6–411.8/s |

Every official run completed 125,000 validated echoes, 125,000 outgoing
`PUBACK` completions, 125,000 echoed-message acknowledgement emissions, five
reconnects, 25 temporary subscribe/unsubscribe cycles, and 25 idle boundaries.
All processes exited zero. Docker reported no OOM kill, and every cgroup
reported zero `max`, `oom`, and `oom_kill` events. Final live sampled memory
for v4 runs 1–3 was 1,597,440, 3,686,400, and 2,027,520 bytes; for v5 it was
3,264,512, 1,802,240, and 1,232,896 bytes.

The failed v4 run had a positive slope of 41,068 bytes/round, projected to
780,288 bytes—below the 1,048,576-byte allowance. It failed only because its
ending-window growth was 1,798,144 bytes. Boundary `memory.stat` snapshots did
not show an accumulating resource: `anon` stayed between 2,297,856 and
2,322,432 bytes, kernel memory between 364,544 and 397,312 bytes, slab between
263,928 and 273,032 bytes, and socket memory between 0 and 4,096 bytes.
All MQTT pending-state counters stayed zero.

A fresh full-size v4 diagnostic reproduction completed immediately afterward
and passed: baseline 3,194,880 bytes, ending 3,444,736, growth +249,856
(+7.82%), projected trend 0, peak 9,060,352, final memory 1,863,680, and
426.4 messages/s. The official failure therefore did not reproduce in the
other two official v4 runs or this additional run. The evidence points to
`memory.current` accounting/page-stock variability rather than MQTT state,
socket, logging, or broker-driven accumulation. The initial tolerance was not
changed. The raw automated aggregate remains Fail because it intentionally
requires 3/3 per-run passes; the maintainer acceptance conclusion treats this
non-reproduced measurement outlier as external to the client and accepts v4.

The measurement host used Linux 7.1.5 on x86_64, Docker 29.6.2 with the systemd
cgroup driver and cgroup v2, the pinned Rust 1.85.0 Alpine builder, static musl
1.2.5 linking, and pinned Mosquitto 2.0.22. The host toolchain was Rust 1.96.1;
it did not build the measured binaries. The concise official output was:

```text
v4 run=1 Pass growth=-282624 trend=0 peak=8040448 messages/s=341.275
v4 run=2 Fail — growth growth=1798144 trend=780288 peak=8101888 messages/s=342.766
v4 run=3 Pass growth=-12288 trend=0 peak=10440704 messages/s=372.265
v5 run=1 Pass growth=-507904 trend=0 peak=9031680 messages/s=411.810
v5 run=2 Pass growth=-282624 trend=198890 peak=8253440 messages/s=409.837
v5 run=3 Pass growth=614400 trend=0 peak=8413184 messages/s=361.559
v4 overall=Fail passing_runs=2/3
v5 overall=Pass passing_runs=3/3
```

## Throughput observations

Validated messages per second and round durations are diagnostic. This workload
includes fixed idle sampling windows and broker restarts, and was not designed
to establish a portable speed threshold.

The maintained `benchmarks` package already contains separate v4/v5 QoS 1 soak,
throughput, and latency scenarios with warmups, repeated comparisons, quality
gates, and controlled-baseline guidance. Those scenarios—not this acceptance
test—are the appropriate mechanism for performance regression claims.

## Limits of the claim

The result covers the documented Linux/x86_64 static-musl, plain-TCP,
no-optional-feature configuration. It does not establish memory behavior for
TLS, WebSockets, proxies, tracing, alternate allocators, dynamically linked GNU
builds, other operating systems or architectures, arbitrary application queues,
larger packets, or components outside the client cgroup.

Kernel cgroup accounting can vary by pages between samples, which is why each
round uses a settled median and the tolerance has an absolute 1 MiB floor.
On this host, `memory.current` varied by several MiB between boundaries even
when the detailed `memory.stat` resource categories and client state were
flat. The tolerance is fixed in advance and was not adjusted based on the
result; consequently, the non-reproduced v4 outlier remains a per-run failure
in the raw evidence even though the maintainer acceptance conclusion is Pass.

This profile validates bounded memory under one sustained workload. It does not
prove that the clients are universally “fast” or replace controlled,
baseline-relative performance testing.
