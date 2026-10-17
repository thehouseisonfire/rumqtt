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

These datasets precede the invalidation-snapshot and collection-visitor locking
fixes. Their hashes identify that earlier implementation; performance has not
been remeasured after those fixes, and the acceptance gate remains open.

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

## Validation scope and remaining work

Linux validation passed the core per-feature test matrix, default C tests,
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
this is a validation limitation, not a readiness test pass. Clippy passed with
allowances for existing `double_must_use`, `doc_markdown`, and
`option_if_let_else` warnings outside this change.

macOS and Windows adapters cross-compiled, and native tests are wired into their
CI jobs. Neither platform was executed here. This completion-focused benchmark
does not measure event bursts, long-lived retained-result memory, realistic
reactor work, or peer tail latency under a hot source. Those measurements and
native platform execution remain necessary before closing TODO42's acceptance
items. No default enablement or performance-gate waiver is implied.
