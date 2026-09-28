# C Binding Parity: Remaining Work

## C-WC-05: Overlapping reauthentication ordering

- Add a deterministic native C fixture that controls broker AUTH progress and
  observes tracked completions and authentication lifecycle events together.
  Assert the required completion and event ordering for overlapping requests;
  waiting for the completions sequentially must not serve as proof of their
  resolution order.
- Assert the complete failed-exchange lifecycle through disconnection, including
  any reconnect boundary. Use bounded waits and reject missing, duplicated,
  reordered, or unexpected success events.

## C-WC-01, C-WC-03, C-WC-04: Configuration ownership and runtime boundaries

- Test nonempty-to-nonempty replacement of Will configuration for MQTT 3.1.1 and
  MQTT 5, and CONNECT properties for MQTT 5. Mutate or release both sets of
  caller-owned buffers after their setters return, then start and restart the
  client. Assert that only the replacement values reach the broker, including
  binary payloads and ordered properties.
- Add observable assertions for request and read batching boundaries at zero,
  one, and a larger configured value. Use controlled request admission and
  broker traffic to distinguish the configured behavior and its documented
  default semantics.
- Test the MQTT 3.1.1 outgoing packet-limit reset, MQTT 5 outgoing inflight-limit
  clear, and MQTT 5 advertised maximum-packet-size clear without setting another
  value before starting the client. Verify effective defaults through broker
  wire data and operation results, including behavior beyond each removed limit.

## C-WC-11: Event properties and view lifetimes

- Verify authentication user-property names, values, and order against broker
  wire data, including duplicate names and present-empty values.
- Retain authentication events with nonempty string and byte properties and
  broker DISCONNECT events with nonempty string properties across client
  destruction. Verify borrowed views while their event owner remains alive,
  copy the views with the public helpers, and verify the copies after event
  destruction.
- Compare outgoing-event packet IDs with broker-observed packet IDs rather than
  asserting only that an ID is present and nonzero.
- Complete the native accessor matrix for CONNACK scalar/string selectors and
  authentication/DISCONNECT outputs. Cover wrong event kinds, invalid selectors,
  independently omitted optional outputs, and initialized outputs on failure.

## C-WC-12: Socket settings after clearing

- Start a client after clearing its local bind address and TCP buffer overrides
  and disabling TCP_NODELAY. Verify the resulting socket settings and
  broker-observed connection using platform-appropriate assertions; do not
  overwrite the cleared configuration before starting the client.
- Add Windows socket inspection for configured buffer sizes and TCP_NODELAY.
  Account for operating-system buffer adjustments without accepting unapplied
  options.

## C-WC-13: macOS and Windows platform trust

- Add native C platform-trust fixtures for macOS and Windows with isolated trust
  stores or disposable runners. Exercise each supported TLS backend for both
  MQTT versions, verify acceptance of a trusted broker and rejection of an
  untrusted broker, and clean up trust-store changes on success and failure.

## Cross-platform validation and parity evidence

- Run the native fixtures and all seven feature-package profiles on macOS and
  Windows. Validate installed static/shared CMake and pkg-config consumers,
  including disabled-feature rejection and TLS/proxy combinations.
- Provision Mosquitto on macOS and Windows runners and execute the graceful and
  abrupt-exit Will fixtures. Require execution of the supported cases rather
  than treating a missing broker as completed validation.
- Run the fixtures added or changed for these tasks on Linux, macOS, and Windows.
  Include the ownership, cancellation, and event-lifetime cases in sanitizer
  runs where supported; resolve failures and retain platform-specific evidence.
- Fix defects exposed by the remaining assertions and add focused Rust FFI or
  wrapper-core regressions for the affected behavior. Update user-facing
  documentation and `CHANGELOG.md` for any resulting behavior changes.
- Update `native-wrappers/wrapper-core/PARITY.md` so coverage claims match the
  actual assertions and platform results. Distinguish pending execution,
  unsupported cases, and unavailable ABI baselines from verified coverage.
- Compare the ABI contract with an applicable published release baseline when
  one is available. Report historical compatibility as unverified while no
  applicable baseline exists; a skipped comparison is not compatibility proof.
- Run the workspace, header, and export checks after the remaining changes, and
  the historical ABI comparison on supported hosts. Use the platform-specific
  header and export scripts on Windows:

```bash
cargo fmt --manifest-path native-wrappers/Cargo.toml --all --check
cargo check --manifest-path native-wrappers/Cargo.toml --workspace
cargo test --manifest-path native-wrappers/Cargo.toml --workspace
native-wrappers/c/tests/abi/check.sh ffi-header
native-wrappers/c/tests/abi/check.sh exports
native-wrappers/c/tests/abi/compare-release.sh
```
