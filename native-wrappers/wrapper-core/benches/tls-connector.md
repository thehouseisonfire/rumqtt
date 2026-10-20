# Application-owned TLS composition prototype

**Decision: park TODO49's public C API and further bridge redesign.** The user
confirmed there is no application currently blocked on this extension. Corrected
measurements still fail the agreed gate, and bounded profiling has not established
a small change that would close every failing case. Keep the private experiment
and its correctness coverage; reopen only for a confirmed need and justified cost.

This is the private delivery gate for [TODO49](../../../TODO49.md). The reference
consumer owns its OpenSSL TLS context, trust and negotiation policy while retaining
native socket creation, HTTPS CONNECT, WebSocket framing and MQTT. It is a reference
workload selected for this task, not evidence of an existing production deployment.

The `tls-connector-proof` development feature compiles a C/OpenSSL memory-BIO engine
into Rust unit tests. Rust host-reactor glue drives its byte interface through the
native `TlsHandshakeConnector` boundary. There is no external-TLS public C API or
capability bit. This feature enables the existing native-TLS and HTTP-proxy gates;
it does not demonstrate a backend-free build. Linux execution requires OpenSSL 3
headers and libraries and a C11 compiler. Other platforms have not been validated.

## Ownership and progress

Each lower stream belongs to one asynchronous driver. It polls one read and one
serialized write/flush/shutdown independently. Owned operation state and buffers
survive observer cancellation until their final retained owner releases them.
Dropping an unresolved observer discards the chain. No stream pointer or borrowed
Rust buffer crosses into the C engine.

The plaintext endpoint uses the existing owned-stream adapter. It retains the host
registration through stream destruction. Admission limits are 64 TLS chains, 256
live reservations and 8 MiB of reserved buffer storage per registration, with
16 KiB transfers. Reservations include per-chain staging and an allowance for the
adapter's bounded write buffer. Operation metadata is separately bounded by the
chain/reservation counts; reserved bytes are not a measurement of total heap or RSS.
OpenSSL allocations and BIO buffering belong to the host and are not covered by
the wrapper buffer quota.

The ordinary prototype has one host reactor thread per registration and two
asynchronous drivers per TLS layer: lower-stream and host TLS. The `inline`
experiment schedules the same host driver on the native runtime and adds no host
thread. Both retain asynchronous lower I/O and the same ownership rules. This
variant tests whether removing thread handoffs is sufficient to meet the gate.

The focused suite exercises both protocols, independent proxy/broker roots, nested
HTTPS-proxy TLS, WSS, custom memory streams with short transfers, TLS key-update
requests, trust rejection, truncation, cancellation, retained-operation exhaustion,
close-notify with a pending read, and traffic after the establishment deadline.
Buffered lower streams are covered for handshake and read-generated control
output, final-handshake flush completion/failure, and timeout during a pending
flush. The driver flushes each drained ciphertext flight through the serialized
lower writer, independently of pending reads. Handshake success waits for that
flush; application flush/shutdown requests preserve the same serialization.
The prototype joins host work explicitly and waits for lower-stream reservations
to retire. This is not a validation of a public C join/unload ABI.

## Measurement method

Compare the C engine with native OpenSSL TLS, using TLS 1.3, the same OpenSSL
defaults and isolated supplied roots. Both connect to the same Tokio/Rustls
loopback fixture, disable server tickets, and use TCP_NODELAY. Separate proxy and
broker contexts have different roots. Each sample establishes fresh connections;
handshake/setup costs are outside the publication timing window.

The matrix covers MQTT v4/v5, single/nested TLS, 1 KiB/16 KiB payloads, and two
runtime configurations. `dedicated` means a current-thread runtime; `shared` means
a runtime with one worker. These labels describe native runtime scheduling in the
prototype, not execution of the wrapper's public `ExecutionContext` API. The peer,
producer and MQTT driver share that runtime; threaded host TLS uses its separate
reactor. WebSocket composition is tested functionally, outside the timing matrix.

After 100 warm-up publications, measure 10,000 sequential tracked QoS 1
publications to topic `a`. Every publication awaits its PUBACK; the peer checks
payload bytes and packet IDs. Admission capacity is 64. Alternate native/external
order across seven repetitions. Report median throughput and the median of the
seven per-run p99 publish-to-PUBACK latencies, not a pooled percentile.

Every configuration must reach at least **75% native throughput** and at most
**1.5 times native p99 latency**. The summary rejects incomplete matrices,
duplicate runs, malformed/nonfinite measurements and capacity violations. A
failed timing gate exits with status 1; correctness failures stop the run.

Allocation samples are separate one-repetition runs. They count Rust allocation/
reallocation calls and cumulative requested bytes during publication, including
the peer, producer, native client and bridge on all threads. They exclude startup,
shutdown, OpenSSL's C allocations and allocator overhead. Copy counters record
selected bridge staging/handoff volume; they do not count every dependency copy.
Their instrumentation is disabled during timing samples. Buffer-reservation
accounting remains active in both timing and allocation samples. Instrumented
samples cannot establish a timing-gate result.

This synthetic loopback workload isolates overhead. CPU affinity/frequency and
other desktop activity are not controlled; retained repetitions and environment
metadata support inspection of variability. These results are not production
sizing estimates.

## Reproduction and evidence

From the repository root:

```sh
cargo test --locked --manifest-path native-wrappers/Cargo.toml \
  -p rumqttc-wrapper-core-next --lib --features tls-connector-proof tls_connector_proof
RUMQTTC_TLS_PROOF_HOST_MODE=inline cargo test --locked \
  --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next \
  --lib --no-default-features --features tls-connector-proof tls_connector_proof
python3 native-wrappers/wrapper-core/benches/tls_connector_matrix.py \
  --allocations --output native-wrappers/target/tls-connector-measurements
python3 native-wrappers/wrapper-core/benches/profile_tls_connector.py \
  --baseline native-wrappers/target/tls-connector-measurements/environment.json \
  --output native-wrappers/target/tls-connector-profile-reports \
  --data-output native-wrappers/target/tls-connector-profile-data
```

Use a fresh output directory. The runner builds once before measurement and writes
raw `.txt` runs, validated CSV summaries and selected environment/source hashes.
Both scheduling variants use the same executable. Preserve evidence outside
`target/` when retaining it in the repository; raw results are excluded from the
published wrapper-core package. The measured source hashes, rather than later
documentation edits, identify the executable's implementation.

Current evidence: [corrected baseline](tls-connector-results/linux-2026-10-19-buffered-flush/)
and [separate profiles](tls-connector-results/linux-2026-10-19-profile/). The baseline
includes automatic lower flushing and final-handshake flush completion. Its exact
source hashes were checked before adding the ignored single-workload profiling
entry point; the bridge algorithm was unchanged during this investigation.

The [initial measurements](tls-connector-results/linux-2026-10-19/) and the
[previous final matrix](tls-connector-results/linux-2026-10-19-final/) predate the
buffered-stream fix. Their source hashes distinguish them. The earlier
[validation logs](tls-connector-results/validation-2026-10-19/) likewise describe
the earlier implementation and its then-existing tests.

## Corrected Linux result and decision

Neither scheduling variant passes the unchanged all-configurations gate:

| Host scheduling | Passing configurations | Native throughput fraction | Native p99 latency ratio |
| --- | --- | --- | --- |
| Separate reactor thread | 0/16 | 0.152–0.531 | 1.636–9.617 |
| Native-runtime task | 8/16 | 0.523–1.438 | 0.663–1.877 |

These are ranges across the 16 per-configuration median comparisons from seven
repetitions, not confidence intervals. CPU placement, frequency and desktop
activity remain uncontrolled. Differences from earlier runs do not isolate the
effect of the flushing fix. The runner exited 1 because the gate failed; all
448 timing and 64 separate allocation samples completed successfully.

For MQTT 5 on the current-thread runtime, nested TLS with 1 KiB publications reaches
15.2% native throughput and 9.62 times native p99 with threaded hosting. Inline
hosting reaches 54.0% throughput and 1.87 times p99. Large-message inline workloads
can amortize the overhead; this does not satisfy the every-configuration gate.

Across timing/allocation runs, observed buffer-reservation peaks are 209.4 KiB
threaded and 216 KiB inline, with 11 and 10 reservations respectively. Nested
samples retain two TLS chains. These are workload peaks, not total-memory bounds.

## Bounded profiling

After the timing matrix, profile MQTT 5 on the current-thread runtime with single
TLS/1 KiB, nested TLS/1 KiB, and nested TLS/16 KiB. Each of the nine native,
threaded, and inline cases publishes 100,000 messages after the usual warm-up.
Separate process runs collect CPU/resource counters and 499 Hz userspace CPU
samples with DWARF user callchains. The build retains release optimization/LTO
and adds debug information without stripping symbols. No profiler runs or builds
compete with the acceptance timing matrix.

| TLS / payload | Native CPU us/msg | Threaded CPU us/msg | Inline CPU us/msg | Threaded voluntary switches/msg |
| --- | ---: | ---: | ---: | ---: |
| Single / 1 KiB | 10.0 | 45.6 | 14.6 | 9.23 |
| Nested / 1 KiB | 12.5 | 73.4 | 22.1 | 15.62 |
| Nested / 16 KiB | 34.4 | 126.7 | 47.2 | 18.20 |

CPU includes user and system time for the whole workload process, plus the small
`perf stat` parent. It includes setup, the fixture and native MQTT client, and
teardown. Native/inline voluntary switches are approximately 0.0001/message.
They still perform asynchronous task scheduling within the runtime. These are
single profiling observations, not acceptance timings or isolated wrapper costs.

For nested TLS/1 KiB, inline userspace samples land approximately 20.0% in bridge
functions, 26.0% in runtime/channel/lock functions, 6.6% in allocator functions,
and 6.1% in memory copy/fill functions. TLS/crypto functions account for 24.3%.
Leading bridge frames include `Endpoint::submit`, `HostDriver::poll`,
`Budget::reserve`, and operation destruction; runtime/channel frames include
Flume polling and Tokio task/oneshot handling. Sample counts range from 367 to
4,081 across the nine profiles, with no lost samples.

The separate allocation run for that case records about 120 Rust allocation calls
per publication inline versus 16 native, and about 49.4 kB of cumulative
requested bytes versus 6.3 kB native (decimal units). Selected bridge copy
counters record 4,324 bytes/publication. These counters exclude OpenSSL's C
allocations and do not count every dependency copy.

Together, the observations identify expensive thread handoffs and persistent
operation/channel overhead after removing those handoffs. They do not identify
one dominant copy routine whose replacement would close the gate. Sample buckets
describe flat function labels: inlined allocation or synchronization work can
remain inside a bridge label. Kernel work is absent from userspace samples, and
the categories are not an additive causal decomposition of the extra CPU time.
Unresolved libc addresses were resolved with matching build-ID debug symbols;
the symbol map, analysis script hash and debug-file hash are retained.

The profile reports and [summary CSV](tls-connector-results/linux-2026-10-19-profile/summary.csv)
are retained. Large binary `perf.data` files stay under `target/`. The profiling
entry point is ignored by normal tests and uses the same fixture/client path as
the matrix. Its timings cannot be substituted into the acceptance summary.

No optimization or public ABI work followed. Pooling operation state, reducing
boxed futures/channel round trips, or collapsing actor boundaries are possible
future hypotheses, with ownership/cancellation/serialization consequences. There
is no confirmed consumer benefit to justify that redesign now.

## Historical measurements and validation

Earlier timing/allocation matrices and the 421-test validation record remain in
separate historical directories linked above. They describe the implementation
before the buffered-stream flush fix and its then-existing tests. Use the
corrected baseline and current profiling evidence for the decision recorded here.

## Validation

With the profiling entry point added, the full suite passes 424 tests with four
intentional ignores: three manual measurement/profiling tests and the
Mosquitto-dependent will test. All 15 focused tests pass in both scheduling modes
with default and native-only features; strict pedantic/nursery Clippy passes.

## Promotion boundary

Reopening requires an application whose requirements existing TLS profiles,
verification/signing hooks and Established transports cannot adequately serve.
Document representative workloads and acceptable cost, then validate a bounded
change or justify a redesign. Keep the agreed gate; do not publish an ABI on an
assumption that overhead can be fixed later.

Composition feasibility alone does not authorize publishing an ABI. TODO49's
public profile/registration types, reverse-I/O C completions, typed failure and
attempt provenance, runtime-update rejection, public join/unload observation,
redirect/SRV integration and package/ABI tests remain delivery work after the
prototype meets the agreed gate. The ordinary wrapper TLS/profile contracts are
unchanged by the private development feature.
