# C readiness measurements

The `readiness` feature is disabled by default. Publication hooks and readiness
fields are compiled out without it. The C symbols remain exported as unavailable
stubs. Enabling the core feature elsewhere through Cargo feature unification can
activate core costs without enabling the C API; query C capabilities separately.

## Acceptance criteria (set before measurements)

- No notification threads, polling supervisor, or runtime; two POSIX descriptors
  or one Windows event per context, independent of registration count.
- All observations delivered, bounded registrations/subscribers, and healthy MQTT
  peers progressing even when a different client's event consumer stalls.
- No host timer polling in active mode and no repeated wakeups for acknowledged
  immutable results. Stalled hosts retain at most the configured registrations.
- Compare release baseline, changed feature-disabled, enabled-unused, and active
  notification builds with at least five alternating fresh-process runs. Flag
  sustained throughput loss above 5% or peer p99 observation-latency increase
  above 10%; investigate noisy results before accepting or rejecting them.
- Record active integration costs and idle CPU/wake improvements explicitly;
  platform execution and cross-compilation are separate evidence.

## Reproduction

`readiness.c` is a real C consumer. Build it with `c/tests/native/native_common.c`
and `event_contract.c`, the checked C header, and each release library. Define
`WITH_READINESS=1` only when linking an enabled library; disabled/baseline binaries
contain no references to the new API. Pass mode (`poll` or `active`), client count,
round count, and protocol (`1` or `2`). `RUMQTTC_TEST_PORT` selects the broker.

The original `active` loop is retained for comparisons. `reactor` bounds each
event-source drain to 64 events, services up to four hint batches before rearming,
and skips rearming a finished round. `blocking` waits on each round's completion
handles in client order, draining client events between waits. It measures a
single host's blocking completion baseline, not a general event reactor or one
observer thread per client. All modes verify the same QoS 1 results.

Optional `--capacity N` selects registration capacity (at least twice the client
count); use 1,024 to exercise sparse default-sized tables. `--host-work-us N`
adds unrelated host computation after each round's submissions, equally in every
profile. This is a synthetic host workload, not measured production application
work. Neither option changes the original workload when omitted. Active CPU and
idle CPU use process accounting (`getrusage` or `GetProcessTimes`), include MQTT
runtime threads, and exclude the broker. Reported p99 is end-to-end MQTT
publish-to-observation latency; it does not isolate publication-to-host wake latency.

The runner `readiness_matrix.py` accepts explicit binaries and never edits sources
or builds. It starts a fresh loopback Mosquitto broker per run, rotates profile
order, records hashes, platform, raw operation latencies, CPU, RSS, thread counts,
and completion/wait counts. All clients share one managed execution context.
Startup is outside publication timing. TLS, persistence, reconnection, and
broker processing delays are outside this microbenchmark.

The polling baseline busy-polls during publication to expose unused-path overhead
without adding an artificial timer delay. Its idle interval uses 1 ms polling.
Active mode waits on OS readiness. Both modes use identical tracked QoS 1 work,
consume events independently of completion observation, and verify every result.

For example, after building the four artifacts described above:

```sh
python3 native-wrappers/wrapper-core/benches/readiness_matrix.py \
  --profile baseline-disabled=poll:/artifacts/baseline/readiness-bench \
  --library baseline-disabled=/artifacts/baseline/librumqttc.so \
  --profile disabled=poll:/artifacts/disabled/readiness-bench \
  --library disabled=/artifacts/disabled/librumqttc.so \
  --profile enabled-unused=poll:/artifacts/enabled/readiness-bench \
  --library enabled-unused=/artifacts/enabled/librumqttc.so \
  --profile active=active:/artifacts/enabled/readiness-bench \
  --library active=/artifacts/enabled/librumqttc.so \
  --cpus 12,13,14,15 --rounds 512 --repetitions 5 --output /tmp/readiness.json
```

Choose available CPUs locally or omit `--cpus`. Binary and library pairs must
actually link together; on POSIX, embed the corresponding absolute artifact
directory in each binary's runpath. Merely naming a library in the runner does
not override a binary's loader configuration.

## Linux results, 2026-10-16

**The original performance acceptance gate is not met.** Active readiness removes
idle timer polling, but the active registration and OS-wait path costs throughput
relative to the busy-poll publication baseline. The feature remains disabled by default;
these measurements do not establish that it meets TODO42's release criteria.

The host ran Linux 7.2.9-2-cachyos x86_64, glibc 2.44, rustc 1.96.1, and Mosquitto
2.1.2. Consumers and brokers inherited affinity to CPUs 12–15, four distinct
physical cores. Other host workloads were not isolated. Each dataset has 80 fresh
process runs: four profiles, two client counts, two MQTT versions, five rotated
repetitions, and 512 publication rounds per client.

The baseline is commit `9e33415d3865030bddef6ce025f08460dba8abc0`, before this
implementation. The [initial raw dataset](readiness-results/linux.json.gz) used
different checkout paths for baseline and candidate builds. The
[follow-up raw dataset](readiness-results/linux-matched-paths.json.gz) rebuilt
candidates at the same source path and with the same release target directory
used for the preserved baseline artifact, reducing path-dependent build/layout
differences. Both gzip files retain per-operation latencies, resource samples,
binary/library/source hashes, and summary statistics.

These historical datasets precede the invalidation-snapshot and collection-visitor
locking fixes. Their hashes identify that earlier implementation; the bounded
follow-up below records newer measurements separately. The acceptance gate remains open.

The table shows the follow-up medians across five runs. Each cell is
**operations/s; p99 observation latency in microseconds**. Protocol names use the
MQTT versions rather than the C API's numeric protocol identifiers.

| MQTT | Clients | Baseline | Changed, disabled | Enabled, unused | Active |
| --- | ---: | ---: | ---: | ---: | ---: |
| 3.1.1 | 1 | 21,956; 159.2 | 21,626; 214.1 | 21,863; 173.5 | 11,422; 182.1 |
| 5 | 1 | 21,068; 252.2 | 21,822; 184.0 | 21,709; 191.5 | 12,601; 240.5 |
| 3.1.1 | 32 | 70,851; 581.9 | 70,685; 559.0 | 70,197; 554.5 | 66,713; 627.8 |
| 5 | 32 | 59,628; 531.7 | 58,665; 658.7 | 58,517; 543.4 | 54,764; 575.7 |

Active throughput fell 48.0%/40.2% with one client and 5.8%/8.2% with 32 clients
for MQTT 3.1.1/5 respectively. The initial dataset also exceeded the throughput
gate in all four active cases (47.7%/42.5% and 10.0%/9.3% losses). One-client
median observation latency rose from about 36–38 to 69–75 microseconds in the
follow-up. This workload repeatedly registers, waits, and deregisters between
round trips; it measures their combined cost without isolating each contribution
or predicting application throughput.

Disabled and enabled-unused throughput stayed within 3.7% of baseline in the
follow-up. Initial results included 6.7% disabled and 6.8% enabled-unused losses
in different cases, so the combined evidence does not justify a universal
negligible-overhead claim. Tail latency is noisy even with readiness disabled:
its follow-up p99 rose 34.5% in the one-client MQTT 3.1.1 case and 23.9% in the
32-client MQTT 5 case. The active one-client MQTT 3.1.1 p99 increase was 14.4%;
other active follow-up increases stayed below 10%, while the initial 32-client
MQTT 5 case rose 45.3%. The p99 gate therefore also remains unresolved.

During each 500 ms idle interval, active mode made one OS wait versus median
482–493 polling waits in the follow-up. With 32 clients the sampled CPU delta
was 0.00 s active versus 0.01 s polling; the 10 ms accounting resolution cannot
establish zero CPU use. All profiles had four threads. Open descriptors were
7 versus 9 with one client and 38 versus 40 with 32 clients: one active context
adds two descriptors, with no growth per registration. Median RSS was about
5.5–6.0 MiB with one client and 8.0–9.3 MiB with 32 clients across profiles;
process noise prevents attributing these small differences to retained results.
Every run verified all expected completions.

## Bounded optimization pass, 2026-10-18

Readiness remains experimental and disabled by default. The original acceptance
gates are unchanged; a different application-specific contract requires a separate
decision before qualification. If the original gates remain mandatory and unmet,
defer acceptance rather than add spinning policies or scheduler machinery.

Sampling profiles were unavailable under this host's performance-monitoring
restrictions. Direct C API instrumentation of a release build with 32 MQTT 3.1.1
clients recorded four allocations and 280 requested bytes per completion
registration, about 2.0 microseconds per collection, and 2.9 microseconds per
preparation call. These wall-clock API timings include instrumentation and may
include descheduling; they are diagnostic attribution, not acceptance scores.

The retained core change sizes subscriber vectors for observed fan-out rather
than always reserving for 16 subscribers. Instrumentation of this sizing policy requested 160 rather than 280
bytes per fresh completion registration, still with four allocations. Free-slot and guarded-batch prototypes
gave mixed throughput results across MQTT versions, including a repeated MQTT 5
slowdown. Neither a constant-time LIFO list nor a minimum-slot heap with guarded
enumeration established a consistent benefit that justified extra context storage
and a new public iterator API. Those prototypes were removed. Registration,
enumeration, producer work, sequentially consistent revision ordering, OS adapters,
cancellation ownership, and close arbitration remain unchanged. No new API, ready
queue, object recycling, notification thread, or additional context storage was added.

For a supplementary sparse-table host workload, use the same binary/library
profiles as above, add `--capacity 1024 --host-work-us 50`, and include
`--profile reactor=reactor:/artifacts/enabled/readiness-bench` and
`--profile blocking=blocking:/artifacts/disabled/readiness-bench` with their matching
`--library` arguments. Keep original-workload results separate from these
supplementary measurements and preserve the busy-poll comparison.

### Recorded comparisons

The [original-workload dataset](readiness-results/linux-bounded-original.json.gz) contains 140 fresh-process
runs and 1,182,720 verified completions. The [supplementary
dataset](readiness-results/linux-bounded-reactor-work.json.gz) contains 80 runs and 675,840 verified
completions with 1,024 context slots and 50 microseconds of simulated host computation per round. Both use
release builds without default features, both MQTT versions, 1/32 clients, 512 rounds, and five rotated
repetitions. Affinity was CPUs 12–15; the rest of the host was not isolated. All profiles had four threads.

The historical baseline is `9e33415d3865030bddef6ce025f08460dba8abc0`; the pre-pass library is
`580d3f265193cb772d543b171f4515112beee852`. Frozen copies of the original C consumer compare the original
`active` algorithm before and after the allocation change. Enhanced consumers measure `reactor`, polling, and
blocking completion waits. The datasets retain library/binary hashes, commands, build settings, frozen
consumer source, source hashes, per-operation latencies, resource samples, and per-run distributions. Absolute
rates from different datasets should not be compared as if conditions were identical.

Original workload medians are **operations/s; p99 microseconds**. Percentiles are medians of per-process p99
values, not pooled percentiles.

| MQTT | Clients | Historical polling | Current polling | Before, active | Allocation change, active | Bounded reactor |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 3.1.1 | 1 | 27,088; 109.0 | 24,715; 101.0 | 11,654; 183.9 | 12,513; 192.6 | 12,282; 266.7 |
| 5 | 1 | 21,261; 237.6 | 21,222; 226.9 | 12,424; 193.2 | 12,373; 232.0 | 13,455; 204.9 |
| 3.1.1 | 32 | 70,439; 490.7 | 72,921; 450.2 | 66,704; 518.3 | 69,618; 467.4 | 70,518; 462.7 |
| 5 | 32 | 57,736; 546.4 | 58,109; 542.0 | 56,571; 553.5 | 54,433; 571.1 | 54,994; 567.3 |

The bounded reactor loses 50.3%/36.6% throughput against current polling with one client for MQTT 3.1.1/5, and
3.3%/5.4% with 32 clients. Its p99 changes are +164.1%/−9.7% and +2.8%/+4.7%, respectively. The
allocation-only active comparison is mixed: relative to the pre-pass active library, 32-client throughput
rises about 4.4% for MQTT 3.1.1 and falls about 3.8% for MQTT 5. These results do not establish a universal
throughput improvement.

One original-workload enabled-but-unused result is also flagged: one-client MQTT 3.1.1 throughput is 14.4%
below current disabled polling and p99 is 52.1% higher. This does not establish causality from a
subscriber-allocation branch that is not exercised without registrations. It prevents claiming negligible
unused-path overhead from this matrix. The other unused throughput
medians are within 2.1% of their current disabled comparisons.

A [focused follow-up](readiness-results/linux-bounded-unused-followup.json.gz) on 2026-10-19 rebuilt the
before-enabled, allocation-only enabled, and disabled libraries at the same source and target paths. It used
15 rotated fresh-process runs per profile, 2,048 rounds, one MQTT 3.1.1 client, and 92,160 verified
completions. Median throughput was 30,305/29,774/30,587 operations/s, respectively; p99 was
41.8/42.4/41.3 microseconds. The allocation-only candidate was 1.8% slower than before-enabled and 2.7%
slower than disabled; its p99 was 1.4%/2.7% higher. The earlier large unused-path loss was not reproduced.
The host was still not isolated, including an unrelated Rust build; this focused check supports retaining
the small allocation change, not a causal attribution or universal negligible-overhead claim. The raw
archive preserves all runs, latencies, artifact hashes, and matched-build provenance.

With 32 clients, the bounded reactor uses about 31.7/43.5 microseconds of process CPU per operation for MQTT
3.1.1/5, versus polling at 38.8/49.2. Sequential blocking completions use about 26.9/34.1 microseconds but
have higher p99 observation latency than the reactor in these cases. The single-client reactor does not
establish a CPU-per-operation benefit. Active idle phases make one OS wait rather than roughly 480–495 timed
polling waits; this is not a claim of zero idle CPU or measured energy savings.

The supplementary workload also fails the original limits; simulated host work does not make the feature
qualified:

| MQTT | Clients | Polling ops/s | Reactor ops/s | Throughput change | Reactor p99 change |
| --- | ---: | ---: | ---: | ---: | ---: |
| 3.1.1 | 1 | 18,182 | 15,418 | -15.2% | +38.9% |
| 5 | 1 | 18,207 | 15,733 | -13.6% | +25.3% |
| 3.1.1 | 32 | 71,454 | 66,716 | -6.6% | +31.8% |
| 5 | 32 | 56,391 | 52,976 | -6.1% | +8.1% |

[Exploratory trial summaries and source snapshots](readiness-results/linux-bounded-trials.json.gz) retain the
rejected free-slot/batch variants, the 15-run unused-path check of the larger prototype, the 10-run MQTT 5
comparison of both free-slot variants, and the seven-run allocation-only comparison. Per-operation latency
arrays are omitted from that trial-summary archive; the two qualification-oriented matrices above retain them.
The allocation-only longer comparison observed +5.5%/−3.8% active throughput changes with 32 clients for MQTT
3.1.1/5, confirming the need to record both benefits and regressions.

**Acceptance remains deferred.** The host-loop recipe and bounded allocation change do not meet TODO42's
original release criteria. Preserve those failures and keep the feature disabled by default. Further work
requires an actual application workload and wake-latency/resource objectives, plus hot-peer, retained-memory,
and native platform evidence; it should not grow into scheduler machinery to match serial busy polling.

## Validation scope and remaining work

Historical Linux validation passed the core per-feature test matrix, default C tests,
minimal readiness and composed readiness/ordered-shutdown/tracing tests,
workspace checks, C/C++ headers, exported ABI checks, and package consumers for
the three readiness profiles. The native POSIX consumer exercised both MQTT
versions with dedicated and shared execution; the runnable host-loop example
also passed. Deterministic core tests cover publication/rearm/close races,
subscriber limits, pending-owner cancellation arbitration, partial event drains,
and retained outcomes. The shared-worker integration test verifies healthy peer
progress while a stalled client's existing event backpressure terminates it.

The broader native package suite failed the existing `native-auth-invalid`
assertion at `native_auth_invalid.c:141`. A separate CMake consumer linked
directly to the preserved pre-change baseline reproduced the same assertion;
this is a validation limitation, not a readiness test pass. Historical Clippy passed with
allowances for existing `double_must_use`, `doc_markdown`, and
`option_if_let_else` warnings outside this change.

The bounded pass was validated again on Linux on 2026-10-19. Full core/C tests passed in minimal
readiness and composed readiness/ordered-shutdown/tracing configurations, as did C feature-disabled tests
with and without core readiness feature unification. `cargo hack --each-feature --exclude-all-features
test --locked -p rumqttc-wrapper-core-next -p rumqttc-c-next --features readiness --lib` passed all 35
profiles (using `--manifest-path native-wrappers/Cargo.toml`). Strict Clippy with `-D warnings`,
`clippy::pedantic`, and `clippy::nursery` passed for both crates and all targets with minimal readiness,
without allowances. C/C++ header, export, and package-consumer checks passed through
`RUMQTTC_C_FEATURES=readiness native-wrappers/c/tests/abi/check.sh all`. The native readiness consumer,
runnable readiness example, and unload test all passed without skips. The larger native suite was not
rerun in this pass; its historical failure above remains recorded. The C invalidation test now exercises
paginated collection with an extended record stride and verifies that extension bytes remain untouched.

macOS and Windows adapters cross-compiled, and native tests are wired into their
CI jobs. Neither platform was executed here. This completion-focused benchmark
does not measure event bursts, long-lived retained-result memory, realistic
reactor work, or peer tail latency under a hot source. Those measurements and
native platform execution remain necessary before closing TODO42's acceptance
items. No default enablement or performance-gate waiver is implied.
