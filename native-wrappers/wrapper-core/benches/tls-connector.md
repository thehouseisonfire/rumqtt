# Application-owned TLS composition prototype

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
```

Use a fresh output directory. The runner builds once before measurement and writes
raw `.txt` runs, validated CSV summaries and selected environment/source hashes.
Both scheduling variants use the same executable. Preserve evidence outside
`target/` when retaining it in the repository; raw results are excluded from the
published wrapper-core package. The measured source hashes, rather than later
documentation edits, identify the executable's implementation.

Retained results: [final Linux 2026-10-19](tls-connector-results/linux-2026-10-19-final/).
The [initial measurements](tls-connector-results/linux-2026-10-19/) precede the
blocked-flush/control-write correction; their source hashes distinguish them.
The [validation logs](tls-connector-results/validation-2026-10-19/) cover that
measured source, including the forced key-update regression. These retained
measurements and logs predate the buffered-stream flush fix. That fix adds
automatic lower flushes after drained ciphertext flights; the matrix must be
rerun before using its results to assess the current implementation's overhead.

## Retained Linux result before the buffered-stream flush fix

Neither scheduling variant passes every agreed workload gate:

| Host scheduling | Passing configurations | Native throughput fraction | Native p99 latency ratio |
| --- | --- | --- | --- |
| Separate reactor thread | 0/16 | 0.249–0.499 | 1.719–5.843 |
| Native-runtime task | 8/16 | 0.521–0.863 | 0.674–1.846 |

Ranges are minima/maxima of the 16 per-configuration median comparisons. The
largest remaining throughput gap is nested TLS with 1 KiB publications. The
native-runtime variant removes thread handoffs but retains owned-operation
allocation and scheduling between stream drivers. These observations motivate
further bridge design work; they do not isolate each source of overhead.

Observed instrumented buffer reservations peak at 200 KiB with threaded host
work and 216 KiB with native-runtime scheduling. Reservation counts peak at
11 and 10 respectively; the nested workloads retain two TLS chains. These are
workload observations, not worst-case bounds. Allocation samples report roughly
66–160 external-path Rust allocation/reallocation calls per publication across
the matrix, including the peer and native client. Inspect the separate allocation
CSVs for native comparisons and cumulative byte counts.

The retained full wrapper-core suite passed 421 tests with three intentional
ignores (this manual release benchmark, a manual diagnostics benchmark, and the
Mosquitto-dependent will test). All 12 then-existing focused tests passed in
threaded and inline mode, with default features and with native TLS only. Strict
Clippy with pedantic and nursery checks passed with `tls-connector-proof,readiness`;
readiness avoids an existing unrelated feature-dependent const suggestion in the
runtime.

After the buffered-stream fix, the full suite passes 424 tests with the same
three intentional ignores. All 15 focused tests pass in both scheduling modes
with default and native-only features; strict Clippy and formatting checks pass.
These correctness checks do not replace the pending performance remeasurement.

## Promotion boundary

Composition feasibility alone does not authorize publishing an ABI. TODO49's
public profile/registration types, reverse-I/O C completions, typed failure and
attempt provenance, runtime-update rejection, public join/unload observation,
redirect/SRV integration and package/ABI tests remain delivery work after the
prototype meets the agreed gate. The ordinary wrapper TLS/profile contracts are
unchanged by the private development feature.
