# C Binding Parity: Remaining Work

## Callback concurrency and ownership

- Add native C stress fixtures for per-client callback serialization,
  cross-client overlap, and ordered save/clear calls for a shared store key.
- Test callback ownership during registration replacement, failed start,
  destroy timeout, and driver failure, including shutdown while a callback
  function is still executing. Assert that `destroy(user_data)` runs exactly
  once, after active calls return and the final retained completion is released.
- Race callback completion against cancellation for authentication, resolution,
  and store operations. Assert that the losing completion cannot change client
  state and returns `RUMQTTC_INVALID_STATE` when cancellation wins.
- Exercise reentrant nonblocking client admission from callbacks and safe
  shared-library unload after all callback owners and completions are released.

## C-WC-02: Durable client-session storage

- Add native C restart fixtures for restored subscriptions and incoming QoS 2
  state on MQTT 3.1.1 and MQTT 5. Cover broker session loss and both MQTT 5
  broker-session-resume policies.
- Inject save and clear callback failures, timeouts, and late completions.
  Assert that pending operations resolve and retained callback data is released.
- Interrupt an atomic checkpoint save and verify that a subsequent load returns
  the preceding complete checkpoint.
- Test store identity and scope across registration replacement and restart.
  Attempt concurrent client starts with the same key and assert that only one
  client can acquire it; verify that distinct keys can operate independently.

## C-WC-05: MQTT 5 enhanced authentication

- Submit overlapping client-initiated reauthentication requests and verify the
  ordering of tracked completions and authentication lifecycle events.
- Add native C fixtures for invalid callback responses, authentication-method
  changes, and malformed broker AUTH properties. Check typed errors,
  pending-operation completion, initialized outputs, and captured-output
  redaction of credentials and proofs.
- Supply an invalid SCRAM server proof and assert that authentication fails
  terminally without exposing secret data.

## C-WC-06: MQTT 5 redirects and DNS SRV

- Test Server Reference address and URI forms from CONNACK and broker
  DISCONNECT, including malformed or disallowed targets and exhaustion of the
  redirect attempt limit across distinct targets.
- Supply deterministic multi-record DNS answers to test SRV priority, weighted
  selection, unusable targets, target exhaustion, and candidate/attempt metadata.
- Check borrowed redirect views while the event is retained and owned copies
  after event destruction.
- Test redirects with resolver or target transport features disabled and verify
  that redirected clients never read or write the original session-store scope.

## C-WC-07: Proxy transports

- Add native C broker TLS and WSS fixtures through HTTP CONNECT, HTTPS, and
  SOCKS5 proxies. Vary broker and proxy trust independently and assert that a
  valid trust policy for one endpoint cannot validate the other.
- Inject proxy negotiation and connection timeouts and recoverable proxy
  failures. Verify pending-operation results and reconnection after failure.
- Reject unsupported proxy kinds before a direct broker connection can occur.
  Capture process output and check credential and authorization-header redaction.
- Cover static and shared CMake and pkg-config consumers for the proxy/TLS
  feature combinations on Linux, macOS, and Windows.

## C-WC-11: Rich events

- Add native broker fixtures for the accessor-kind matrix of CONNACK, broker
  DISCONNECT, authentication, redirect, and outgoing events. Check wrong-kind
  errors, output initialization, optional outputs, and absent versus
  present-empty values.
- Check ordered MQTT 5 property count/at accessors, outgoing packet IDs,
  operation IDs in completions and errors, borrowed-view lifetimes, and copy
  helpers against broker wire data for these event classes.
- Stress large event queues while retaining event owners, applying backpressure,
  and destroying owners during shutdown.

## C-WC-01, C-WC-03, C-WC-04: Will and connection options

- Verify exact MQTT 3.1.1 and MQTT 5 Will fields and MQTT 5 CONNECT properties
  at a broker, including property order and absent versus present-empty values.
- Test graceful and ungraceful Will behavior and ownership after replacing or
  clearing Will and CONNECT configuration.
- Exercise packet-size, inflight, and batching boundaries; default/reset
  semantics; and topic-alias behavior across reconnects.
- Add malformed-input cases for Will, CONNECT, and runtime-limit configuration:
  missing size prefixes, invalid selectors, reserved fields, inconsistent
  count/pointer pairs, and integer overflow.

## C-WC-08, C-WC-09, C-WC-10: Unix, WebSocket, and close options

- Add native C Unix-path and shutdown fixtures on supported platforms and
  runtime-rejection fixtures elsewhere.
- Verify WebSocket header add/replace/remove order, protected-header rejection,
  header behavior across reconnects, WSS/proxy composition, and disabled-feature
  errors.
- Verify MQTT 5 close reason/properties on the wire. Race conflicting close
  options and graceful-to-immediate escalation, checking the admitted payload
  and each caller's deadline and completion.

## C-WC-12: Socket settings and capabilities

- Verify native C socket settings through observable network effects and test
  unsupported-platform errors.
- Check capability bits across feature builds, unknown-bit forward
  compatibility, and captured-output redaction of network configuration errors.

## C-WC-13: TLS

- Add native C fixtures for platform trust, rustls PEM and native PKCS#12 mutual
  TLS, ALPN, hostname mismatch, wrong roots, and malformed credentials.
- Test TLS configuration replacement/clear, failed-start cleanup, and
  captured-output redaction.
- Cover TLS and WSS with Rustls-only and mixed backends, mutual TLS with the
  native backend, and disabled-backend rejection.
- Cover static and shared CMake and pkg-config consumers for the TLS backend
  combinations on Linux, macOS, and Windows.

## Validation of remaining changes

- Fix binding defects exposed by these fixtures through wrapper-core and add
  focused Rust FFI regression tests for the affected inputs, outputs, ownership,
  and C-to-core field mapping.
- Run the new deterministic native fixtures and package consumers on Linux,
  macOS, and Windows. Include the callback ownership races in sanitizer runs
  where supported.
- Validate each closed parity gap against the header, documentation, examples,
  package metadata, exports, and ABI contract, and update
  `native-wrappers/wrapper-core/PARITY.md`.
- Run the workspace and ABI checks after the changes:

```bash
cargo fmt --manifest-path native-wrappers/Cargo.toml --all --check
cargo check --manifest-path native-wrappers/Cargo.toml --workspace
cargo test --manifest-path native-wrappers/Cargo.toml --workspace
native-wrappers/c/tests/abi/check.sh ffi-header
native-wrappers/c/tests/abi/check.sh exports
native-wrappers/c/tests/abi/compare-release.sh
```
