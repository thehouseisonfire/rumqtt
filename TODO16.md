# C Wrapper Feature Parity: Remaining Work

This file tracks open C-binding parity work. Fix any binding defects revealed by
the remaining tests through wrapper-core, and update
`native-wrappers/wrapper-core/PARITY.md` as each gap closes.

## Callback lifecycle

- Stress per-client callback serialization, cross-client overlap, and store
  operations on the same key. Verify that a second active client cannot use the
  same store key and that save/clear calls remain ordered for each key.
- Cover callback ownership during registration replacement, failed client
  start, graceful and immediate close, destroy timeout, driver failure, and
  abandonment. Verify that active calls finish before `destroy(user_data)`,
  which must run exactly once after the final retained completion is released.
- Race completion against cancellation for each callback type. Verify that a
  late or duplicate completion returns `RUMQTTC_INVALID_STATE`, cannot change
  client state, and permits safe shared-library unload after the last owner is
  released. Exercise reentrant nonblocking client admission from callbacks.

## Feature-specific gaps

### C-WC-02: Durable client-session storage

- Add native C restart fixtures for restored subscriptions and incoming QoS 2
  state on both MQTT versions. Cover broker session loss and the MQTT 5
  broker-session-resume policy.
- Exercise save and clear callback failures, timeouts, and late completions.
  Use a fault-injected atomic store to verify that an interrupted save leaves
  the preceding complete checkpoint available to a subsequent load.
- Verify C-level store identity, scope, and one-active-client-per-key behavior
  across replacement, restart, and concurrent clients.

### C-WC-05: MQTT 5 enhanced authentication

- Exercise overlapping client-initiated reauthentication requests and the
  ordering of their tracked completions and lifecycle events.
- Test invalid callback responses, method changes, malformed broker AUTH
  properties, and callback failure. Verify typed errors, pending-operation
  completion, output initialization, and redaction of credentials and proofs.
- Add a native SCRAM failure fixture with an invalid server proof and verify
  that the authentication failure is terminal and contains no secret data.

### C-WC-06: MQTT 5 redirects and DNS SRV

- Exercise supported Server Reference forms from both CONNACK and broker
  DISCONNECT, including rejected targets and attempt exhaustion.
- Verify SRV priority and weighted selection, unusable or exhausted targets,
  and the reported candidate and attempt metadata with deterministic DNS
  answers.
- Verify borrowed redirect views during the event lifetime and owned copies
  after event destruction. Cover behavior without optional resolver or
  transport features and isolation of the original session-store scope after
  a redirect.

### C-WC-07: Proxy transports

- Add native C fixtures for broker TLS and WSS through each supported proxy
  kind. Verify that broker and proxy trust policies stay independent.
- Exercise proxy negotiation and connection timeouts, reconnect after a proxy
  failure, unsupported-kind rejection before any direct broker connection,
  and captured-output redaction of credentials and authorization headers.
- Run static and shared CMake and pkg-config consumers for each supported
  proxy and TLS feature combination on every supported CI platform.

### C-WC-11: Rich events

- Complete the accessor-kind matrix for CONNACK, broker DISCONNECT,
  authentication, redirect, and outgoing packet events. Verify wrong-kind
  errors, initialized outputs, optional outputs, and absent versus
  present-empty values.
- Verify ordered MQTT 5 property count/at accessors, outgoing packet IDs in
  events, operation IDs in completions and errors, borrowed-view lifetime,
  and copy helpers against broker wire fixtures for every mapped event class.
- Stress large event queues and backpressure while retaining and destroying
  event owners.

## Other C verification gaps

- **C-WC-01, C-WC-03, C-WC-04:** Verify exact v4/v5 Last Will and MQTT 5
  CONNECT packets at a broker; graceful versus ungraceful Will behavior;
  replacement and clear ownership; packet-limit and batching boundaries;
  default and reset semantics; MQTT-version mismatches; and topic-alias
  behavior across reconnects. Cover missing size prefixes, selectors,
  reserved fields, count/pointer pairs, and integer overflow.
- **C-WC-08, C-WC-09, C-WC-10:** Verify Unix paths and shutdown on supported
  platforms and runtime rejection elsewhere; WebSocket header add, replace,
  and remove order, protected-header rejection, reconnect behavior,
  WSS/proxy composition, and disabled-feature errors; MQTT 5 close packets,
  conflicting concurrent options, escalation, timeout, and repeated
  completion.
- **C-WC-12:** Verify portable socket settings and unsupported-platform
  errors. Exercise capability bits across feature builds, unknown-bit forward
  compatibility, network effects, and captured-output redaction.
- **C-WC-13:** Add C fixtures for platform and custom trust, rustls PEM and
  native PKCS#12 mutual TLS, ALPN, hostname and wrong-root rejection,
  malformed credentials, replacement and clear, failed-start cleanup, and
  redaction. Exercise Rustls-only, native-only, mixed, and disabled builds;
  verify TLS and WSS behavior and unchanged legacy signatures. Complete
  static and shared CMake and pkg-config consumer coverage for each supported
  backend combination.

## Completion gates

- Add focused Rust FFI tests for defects exposed by these cases, including
  malformed inputs, output initialization, error ownership, panic containment,
  and C-to-core field preservation.
- Run deterministic native C fixtures and package consumers on Linux, macOS,
  and Windows. Use sanitizers for callback ownership where supported; add
  fault injection for callbacks that never complete, interrupted checkpoints,
  broker disconnects, DNS/proxy failures, and shutdown races.
- Run the workspace and ABI checks after the remaining changes:

```bash
cargo fmt --manifest-path native-wrappers/Cargo.toml --all --check
cargo check --manifest-path native-wrappers/Cargo.toml --workspace
cargo test --manifest-path native-wrappers/Cargo.toml --workspace
native-wrappers/c/tests/abi/check.sh ffi-header
native-wrappers/c/tests/abi/check.sh exports
native-wrappers/c/tests/abi/compare-release.sh
```

Close a parity row only when its C values reach wrapper-core intact, native
tests cover success, failure, and shutdown, and the header, documentation,
examples, package metadata, exports, and ABI contract agree with the library.
