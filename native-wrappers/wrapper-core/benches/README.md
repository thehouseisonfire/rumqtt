# Wrapper ordered shutdown measurements

Keep `ordered-shutdown` disabled in standard wrapper packages. Enabling it changes
native admission and publication tracking even when no fence is requested. The
optional API provides a useful publish drain guarantee, with a measurable runtime
cost. Cargo feature unification can activate the native cost independently of
callable C support; check the C capability bit for API availability.

## Method

The release harness measures wrapper-core against an immediately acknowledging
loopback TCP peer. It checks that every publication arrives before DISCONNECT and
implements PUBACK and PUBREC/PUBREL/PUBCOMP. This is synthetic broker-backed
traffic, with no TLS, persistence, broker processing delay or network loss. It
does not measure C argument conversion or foreign-language overhead.

The matrix covers both protocols, QoS 0/1/2, one/eight producers, request channel
capacity one/64, and inflight limit one/64: 48 configurations. Each timing run
publishes 10,000 messages with a 64-byte payload and topic `a`. Both TCP endpoints
use `TCP_NODELAY`. Four profiles rotate their execution order across seven
repetitions per configuration:

- `baseline-disabled`: feature-disabled code at
  `29e29c5b63a0002acdc954c04d2b9afcbba7d95b`, with the same harness copied in.
- `disabled`: this implementation with wrapper and native ordered support off.
- `enabled-unused`: ordered support enabled, without requesting a fence.
- `enabled-ordered`: the same enabled executable, requesting an ordered fence.

Producer threads exist before a start barrier. Admission latency includes queue
capacity waits. Admission throughput includes collecting and joining producer
results; completion throughput includes final drain and execution-owner join.
Disabled and enabled-unused runs await all tracked publication results before
ordinary close. Ordered runs await capacity for native fence admission, then use
the matching ordered closer. Thus all four workloads deliver the same number of
publications, without changing ordinary close's production semantics. Shutdown
latency starts after producer joins and includes the respective remaining drain.
The fence has a 60-second deadline; this benchmark does not exercise expiry.

A separate allocation-instrumented run accompanies each profile/configuration.
Its timing is excluded from throughput and latency summaries. Allocation counts
and cumulative bytes requested cover admission and concurrent native progress,
ending after producer joins. They exclude startup and shutdown, including fence
allocation; they are not retained heap size, peak memory or RSS. Native progress
and capacity retries can vary the counts. Uninstrumented runs retain an allocator
flag check but do not increment counters.

Measured on Linux `7.2.9-1-cachyos-x86_64`, glibc 2.44, an Intel Core i5-13500H
with 16 online logical CPUs, using Rust
`1.96.1 (31fca3adb 2026-06-26)`. Runs were interleaved on the same host without
concurrent builds. CPU affinity and frequency were not fixed. The reported
ranges show substantial scheduler/frequency variation; these are workload
observations, not production sizing estimates or cross-platform results.

## Results

The complete [192-row CSV](ordered-shutdown-results.csv) includes all configurations
and observed minimum/median/maximum timing values. Across the 48 configurations,
enabled-unused versus disabled has a median change of **−9.9% completion
throughput** and **−9.7% admission throughput**. Enabled-ordered has median changes
of −9.3% and −10.4%, respectively. These are equally weighted medians of per-
configuration ratios, not pooled workload rates. Individual completion-rate
changes span −32.8% to +23.5% for enabled-unused and −42.5% to +9.8% for ordered.

With one producer and capacity/inflight 64, enabled-unused QoS 1/2 completion
rates are approximately 16–27% below disabled. The following medians are
messages/second; the baseline is the feature-disabled Git revision above.

| Protocol | QoS | Baseline | Disabled | Enabled-unused | Enabled-ordered |
| --- | --- | ---: | ---: | ---: | ---: |
| v4 | 0 | 163,179 | 166,631 | 152,610 | 154,742 |
| v4 | 1 | 58,139 | 53,223 | 43,975 | 44,596 |
| v4 | 2 | 35,018 | 32,428 | 27,215 | 26,677 |
| v5 | 0 | 125,934 | 137,327 | 123,940 | 127,098 |
| v5 | 1 | 43,135 | 42,926 | 32,152 | 32,451 |
| v5 | 2 | 27,324 | 28,052 | 20,577 | 20,831 |

For QoS 1 with capacity/inflight 64, admission rates and median per-run latency
percentiles are below. `Off` means disabled and `On` means enabled-unused.
Latency units are microseconds; concurrent producers expose long capacity waits.

| Protocol | Producers | Profile | Admissions/s | p50 µs | p95 µs | p99 µs |
| --- | ---: | --- | ---: | ---: | ---: | ---: |
| v4 | 1 | Off | 54,075 | 14.82 | 28.26 | 31.98 |
| v4 | 1 | On | 44,753 | 19.20 | 40.17 | 62.42 |
| v4 | 8 | Off | 37,617 | 49.73 | 817.21 | 2587.60 |
| v4 | 8 | On | 34,646 | 41.18 | 926.07 | 3245.46 |
| v5 | 1 | Off | 43,805 | 18.24 | 45.17 | 52.25 |
| v5 | 1 | On | 32,850 | 23.90 | 62.96 | 69.84 |
| v5 | 8 | Off | 34,727 | 49.28 | 547.95 | 2535.46 |
| v5 | 8 | On | 27,114 | 59.10 | 790.46 | 3956.01 |

Shutdown medians in milliseconds for one producer and capacity/inflight 64:

| Protocol | QoS | Disabled | Enabled-unused | Enabled-ordered |
| --- | --- | ---: | ---: | ---: |
| v4 | 0 | 1.309 | 1.295 | 0.235 |
| v4 | 1 | 2.959 | 4.271 | 3.959 |
| v4 | 2 | 4.274 | 6.643 | 6.061 |
| v5 | 0 | 1.391 | 1.450 | 0.308 |
| v5 | 1 | 4.749 | 6.586 | 5.938 |
| v5 | 2 | 7.618 | 10.013 | 9.042 |

The separate QoS 1 allocation samples for that configuration show fewer
cumulative allocation requests with enabled native queues, despite lower
publication throughput. This does not establish lower retained memory usage.

| Protocol | Off calls/msg | On calls/msg | Off bytes/msg | On bytes/msg |
| --- | ---: | ---: | ---: | ---: |
| v4 | 31.94 | 28.04 | 2772.82 | 2312.37 |
| v5 | 34.27 | 30.22 | 4851.67 | 3811.99 |

Disabled versus baseline has a median completion-rate change of +1.1% and
admission-rate change of +1.3%. Individual completion-rate changes range from
−27.4% to +17.0%; the aggregate median does not prove every configuration is
unchanged. For example, v4 QoS 1 baseline samples span 36,089–61,417 messages/s
and disabled samples span 28,911–61,376 at capacity/inflight 64 with one producer.
Investigations and follow-up measurements are recorded below.

## Disabled-build investigation

An earlier implementation stored ordered diagnostics inline in the shared
completion enum. Allocation samples showed approximately 24 additional requested
bytes per tracked publication even with the feature disabled. The final optional
diagnostics record is boxed and absent until fence admission. This avoids that
per-publication growth and avoids allocating diagnostics on enabled clients
before they request a fence.

The original admission mutex is retained in disabled builds. Only enabled builds
use a timed mutex for bounded ordered-close gate acquisition. A separate
interleaved QoS 0 comparison used 15 runs of 30,000 messages per profile with one
producer and capacity/inflight 64. Median completion rates for baseline,
unconditionally timed mutex, and final standard mutex were respectively
161,875 / 157,612 / 157,942 messages/s for v4, and
143,229 / 142,218 / 147,409 for v5. These results do not attribute all variation
to the mutex. The default path keeps its original primitive, and the final matrix
compares disabled code directly with the baseline rather than using native
feature measurements as an explanation for wrapper regressions.

The two largest disabled completion slowdowns in the matrix were v5 QoS 0 with
eight producers, capacity 64 and inflight one (−27.4%), and v4 QoS 2 with one
producer and the same limits (−17.4%). Follow-up interleaved baseline/disabled
measurements used 15 repetitions of 30,000 messages per profile, without builds
running concurrently. [The follow-up CSV](disabled-followup.csv) retains the
admission and completion ranges. Completion rates were:

| Workload | Baseline median [min–max] | Disabled median [min–max] |
| --- | ---: | ---: |
| v5 QoS 0, eight producers | 94,789 [86,316–99,809] | 96,289 [89,897–101,310] |
| v4 QoS 2, one producer | 49,565 [44,480–54,101] | 50,483 [44,593–54,916] |

The original slowdowns did not reproduce in these longer runs: disabled medians
were +1.6% and +1.9%. Host scheduling/frequency variation is a plausible cause,
but these observations do not establish causality or prove universal zero cost.

## Reproduction

Run from the repository root. The matrix requires Python 3.12 or newer, Cargo and
a Git baseline containing the existing wrapper-core API. It builds all profiles
before measuring and rejects failed peer assertions rather than reporting them
as successful throughput samples.

```sh
python3 native-wrappers/wrapper-core/benches/ordered_shutdown_matrix.py \
  --baseline 29e29c5b63a0002acdc954c04d2b9afcbba7d95b \
  --messages 10000 --repetitions 7 \
  --output native-wrappers/target/ordered-shutdown-results.jsonl
python3 native-wrappers/wrapper-core/benches/summarize_ordered_shutdown.py \
  native-wrappers/target/ordered-shutdown-results.jsonl \
  native-wrappers/target/ordered-shutdown-summary.csv
```

The summarizer checks that every configuration has all timing and allocation
samples. JSONL contains individual runs and host metadata; the CSV retains
minimum/median/maximum admission and completion rates and shutdown latency,
median per-run p50/p95/p99 admission latency, and separate allocation samples.
Per-run percentile medians are not pooled latency percentiles. Rerun on the
target platform with representative payloads, transport and persistence before
choosing an optional ordered build for a throughput-sensitive deployment.
