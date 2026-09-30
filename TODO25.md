# C Wrapper Custom Transport Connectors

## Goal and implementation status

Expose application-provided asynchronous connections and byte streams through
wrapper-core and C for both MQTT versions. The public implementation is now in
place: a native C consumer supplies byte operations while native MQTT performs
CONNECT, framing, tracked publication, recovery and shutdown. The host does not
implement MQTT or depend on Rust layouts.

Real socket/tunnel execution remains pending in this session: the sandbox
rejects local socket creation with `EPERM`. Those fixtures compile and are
required in CI. This is an execution limitation, not a missing implementation;
do not treat compilation or CI wiring as evidence of a passing network test.

## Public contract

- `CommonConfig::connector` owns a `TransportConnectorConfig` with a
  `TransportConnector` and declared mode. Both protocol adapters delegate to
  the existing native `set_socket_connector`.
- `TransportMode::Base` supplies bytes before native proxy/TLS/WebSocket layers.
  `Established` supplies MQTT-ready bytes and requires TCP without native proxy
  or layered redirect profiles. Custom connectors cannot use Unix targets.
- Every connect receives protocol, configured client ID, actual dial target
  (proxy endpoint when configured), client-local socket-attempt generation,
  applicable network settings and the exact native absolute deadline. C gets
  the remaining budget in nanoseconds. Native negotiation shares this budget.
- Hosts must explicitly report all network settings applied, or not applicable
  only for defaults. Unsupported settings fail; foreign streams are never
  reported as socket-configured by the wrapper.
- Owned `TransportIo` read/write/flush/shutdown futures yield promptly. Reads
  own their results; writes own their inputs. Transfers and buffered write
  acceptance are bounded at 16 KiB. One read may overlap the serialized writer
  family. Short I/O is normal; an empty successful read is permanent EOF.
  Reading advances pending buffered writes. Flush waits for accepted output;
  shutdown drains writes and flushes before closing writing.
- Native TLS has a private asynchronous-flush bridge: BIO flushes are deferred
  to a read boundary, where Pending can be represented correctly. Handshake
  completion and outer flush/shutdown still wait for real underlying output.
- C registrations and streams are opaque independent owners. Size-versioned
  vtables/requests/responses use fixed-width selectors, reserved fields, and
  the existing retained `rumqttc_callback_completion_t`. Stream creation uses
  the originating connect completion, binding the stream to that attempt.
  Each stream is single-use; reconnect obtains a fresh one.
- Connect metadata is borrowed during the callback. Copy it for deferred work.
  Retain a completion before returning with pending work; write input stays
  valid until its retained tokens are released. Read completion copies bytes
  during the call. Duplicate/cancelled/stale results are rejected before reading
  response views. Dropping the last host token wakes the observer as abandoned.
- Cancellation invalidates observation before calling
  `cancel(user_data, operation_id)` outside wrapper locks. The host must stop
  and wake work promptly; cancelled connections are discarded. Retained buffers
  and owners survive until released. Registration-wide unique operation IDs
  prevent cross-client cancellation collisions.
- Each registration limits live operations, including host-retained cancelled
  work: 1–65536, initialized to 256. Token clones share an operation slot.
  Exhaustion is a terminal typed failure, preventing unbounded retained work
  across connections. Hosts remain responsible for releasing tokens.
- Callbacks must be thread safe and return promptly. A timeout cannot preempt a
  blocking C callback. Destructors run once on final release, on any thread,
  and must neither block nor unwind. Release all owners before library unload.
- `RUMQTTC_CAP_TRANSPORT_CALLBACKS` reports the C API. Fixed transport failure
  details never expose arbitrary host strings or credentials.

See [wrapper-core](native-wrappers/wrapper-core/README.md#custom-transports),
[the C contract](native-wrappers/c/README.md#custom-transports), and
[the runnable C example](native-wrappers/c/examples/custom_transport.c).

## Implementation checklist

- [x] Owned connector/stream abstractions and adapters for both protocols.
- [x] Opaque retained C registration, stream and deferred-operation handles;
  versioned records, selectors and ABI generation/exports.
- [x] Exact attempt metadata/deadlines and explicit socket-setting policy.
- [x] Short I/O, EOF, pending work, errors, wakeups and direction serialization.
- [x] Owned/copying buffers, per-transfer bounds and retained-operation limits.
- [x] Prompt callbacks outside lifecycle/admission locks; documented blocking
  and cancellation obligations.
- [x] Duplicate/late/stale rejection, cancellation and connection discard.
- [x] Independent owner lifetimes and exactly-once destruction.
- [x] Native layering and established-stream validation; fresh reconnects.
- [x] Capability reporting and typed redacted failures.
- [x] In-memory and byte-tunnel C demonstrations for both protocols.
- [x] Lifecycle, race, partial-I/O and TLS/proxy/WebSocket regression fixtures.
- [x] Header generation, exports, READMEs, examples, PARITY, changelog and CI.

## Verification

The private `transport-proof` feature compiles real C callbacks into adapter
unit tests. The fixture's automatic-read readiness claim and byte capture happen
under the same lock; callback delivery happens outside it. Delayed completion
cannot drain or signal EOF on a later read.

```sh
cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next --lib \
  --features transport-proof transport::
cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next --lib \
  --no-default-features --features transport-proof transport::
cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-c-next --lib ffi::transport::tests::
cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next \
  --no-default-features --features use-rustls-ring,use-native-tls,websocket,proxy \
  --test custom_transport_memory
cmake -S native-wrappers/c/tests/native -B native-wrappers/target/rumqttc-c-native
cmake --build native-wrappers/target/rumqttc-c-native
ctest --test-dir native-wrappers/target/rumqttc-c-native -R custom-transport --output-on-failure
```

Current Linux session evidence:

- 20 C adapter proof tests plus exact-deadline and future-destruction panic
  regressions pass with defaults and without them. All 52 wrapper-core unit
  tests with the proof pass.
- Five public C bridge tests pass: abandonment, cancellation/unpolled drop,
  retained cancelled-operation budget, stale-stream rejection, owned deferred
  write buffers, oversized reads and socket-setting rejection.
- The public native C memory consumer passes for both protocols: tracked QoS1,
  short deferred I/O, EOF/reconnect, graceful/immediate close, retained late
  result rejection and exactly-once final owner release, plus failed construction
  and managed connect timeout with retained late work.
- The in-memory composition matrix passes with defaults and with Ring/native
  TLS/proxies/WebSockets together (48 profiles, each connecting and reconnecting),
  and rejects untrusted brokers over TLS/WSS for both protocols/backends.
  Managed-client timeout cancellation, failed construction, automatic termination
  on terminal connector/stream failures, pending-operation failure and retryable
  reconnects pass without sockets for both protocols. Public C tests verify all
  terminal failure selectors stop after one connect callback, and a timeout
  retries before a terminal failure stops the driver. The native TLS BIO flush
  regression passes.
- Native TLS handshake regressions cover typed read/write/flush failures,
  automatic termination, pending-operation failure and retryable I/O reconnection
  for both protocols (24 cases).
  The bridge also preserves custom error payloads and OS codes when a TLS backend
  discards its source chain. Linux execution and Windows cross-compilation pass;
  Wine execution is blocked by the sandbox's socket permissions, and actual
  Windows execution remains a CI requirement.
- Header/FFI equality, exports, C11/C++17 and relocated static pkg-config
  consumers pass. All native C fixtures/examples compile with strict warnings;
  the optional error-output audit covers 97 APIs. Wrapper workspace checks and
  targeted strict Clippy and the Rust 1.88 native-TLS profile check pass. Native
  feature Clippy passes all 35 configurations
  excluding SCRAM: its unrelated build script writes into the sandbox's read-only
  Cargo registry cache.
- Rust/C ASan and C UBSan pass for the adapter and C bridge with leak scanning
  disabled. LSan runs all assertions successfully but cannot perform its final
  ptrace-based leak scan in this sandbox. CI retains mandatory LSan and native
  Valgrind/macOS leaks checks, including the new transport fixtures.
- Real sockets and the transparent byte-tunnel trust/composition matrix await
  execution on a permitted host. macOS/Windows execution also awaits CI.

Historical private-proof ASan/LSan evidence predates this public implementation;
[PARITY.md](native-wrappers/wrapper-core/PARITY.md) records execution separately
from coverage. The ABI additions preserve existing declarations/records and the
unpublished ABI line under [the C policy](docs/c-abi-compatibility.md).
