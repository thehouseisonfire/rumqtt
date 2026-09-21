# C Wrapper Feature Parity: Remaining Work

Complete the C11/C++17 bindings for the supported capabilities in
`native-wrappers/wrapper-core/PARITY.md`. Translate through wrapper-core;
do not construct MQTT v4 or v5 client values directly in the C crate. If a
required contract is missing in wrapper-core, settle and test it there first,
then update the parity matrix.

## Callback foundation

Design one asynchronous C callback-completion API for session storage,
authentication, and DNS resolution. Each registration needs a size-versioned
vtable, unchanged `user_data`, explicit retain/release rules, and an exactly-once
`destroy(user_data)` callback. Specify whether calls can overlap, which thread
invokes them, whether client operations may be called reentrantly, and which
request fields remain valid after the callback returns.

A callback must be able to complete synchronously or retain an opaque
completion for later use. Exactly one completion wins. Cancellation must wake
pending work; late or duplicate completion must return `RUMQTTC_INVALID_STATE`
without affecting the client. Never call application code while holding a
config, client, event, or callback-owner lock. Quiesce active calls before
invoking `destroy`. Define ownership through failed start, replacement,
graceful and immediate close, destroy timeout, abandonment, driver failure,
and shared-library unload.

Add structured error kinds, statuses, flags, and accessors for new callback,
persistence, authentication, resolver, redirect, proxy, and handshake failures
where wrapper-core provides structured data. Initialize every output on failure
and keep diagnostic secrets out of errors and traces.

## C bindings still needed

### C-WC-02: Durable client-session storage

- Define a versioned store vtable and registration handle with asynchronous
  load, save, and clear. Load must distinguish not-found from failure.
- Pass owned key/checkpoint data and explicit checkpoint format version and
  protocol metadata. Specify per-key call ordering, cross-client overlap,
  atomic-save expectations, one-active-client-per-key behavior, cancellation,
  timeouts, and maximum checkpoint size.
- Add configuration for store registration and removal, copied scope,
  checkpoint size, and MQTT 5 broker-session-resume policy. Report the C
  callback capability only when this path is usable.
- Decide whether to package the file-store adapter as a separately linked C
  option. A path-only file-store setter must not replace the callback API.
- Verify v4 and v5 QoS restart recovery, corrupt/version/protocol/oversize
  checkpoints, callback failures and late completion, and ownership across
  replacement, failed start, close, destroy timeout, and abandonment. Include
  an end-to-end file-store consumer if the adapter ships.

### C-WC-05: MQTT 5 enhanced authentication and reauthentication

- Bind the selected wrapper-core authentication authority through a versioned
  authenticator vtable or generation-bound event token. Preserve exchange kind,
  method, reason code/string, ordered User Properties, and authentication data.
  Borrow secret views only for the documented callback lifetime unless copied
  through an explicit helper.
- Add nonblocking and tracked client-initiated reauthentication operations,
  completion kinds, event kinds, and typed accessors. Event tokens, if used,
  must reject reuse, cross-client use, stale generations, and responses after
  timeout. A configured callback must not also produce a consumable token.
- Keep raw, mechanism-neutral authentication available if a SCRAM convenience
  handle is added. Redact credentials, proofs, and authentication data.
- Test initial, multistep, and repeated authentication with a deterministic
  broker, including rejection, overlap, timeout, reconnect, cancellation,
  callback failure, duplicate/late completion, close, abandonment, and SCRAM
  when enabled.

### C-WC-06: MQTT 5 redirects and DNS SRV

- Bind fixed redirect policies. Add an asynchronous resolver vtable returning
  owned priority, weight, port, and target records. Select the system resolver
  only when supported by the loaded library.
- Expose redirect reason, source, advertised Server Reference, decision,
  selected endpoint, and attempt/loop metadata through owned event accessors.
  Provide copy helpers for values needed after event destruction.
- Test reference forms, weighted/empty/failing DNS responses, rejected and
  looping redirects, shutdown during resolution, late callbacks, missing build
  features, event lifetime, and session-store scope interaction.

### C-WC-07: Proxy transports

- Add versioned proxy options with protocol, endpoint, DNS policy, and optional
  username/password bytes, plus set and clear functions. Keep broker and proxy
  endpoints and their security settings distinct. Add a separate proxy TLS
  record if the supported core contract requires one.
- Reject unsupported proxy kinds before making a direct broker connection.
- Test HTTP and SOCKS fixtures for both MQTT versions and supported transport
  combinations, including authentication, DNS policy, reconnect, timeout,
  redaction, and static/shared package consumers with each feature build.

### C-WC-11: Rich events

- Expose complete CONNACK, broker DISCONNECT, authentication, and redirect
  details. Add outgoing packet identifier and operation ID accessors where
  present, without changing existing signatures.
- Use event-owned views and count/at accessors for ordered MQTT 5 properties.
  Each accessor must initialize outputs, reject the wrong kind, and handle
  documented optional outputs and absent versus present-empty fields.
- Test accessor-kind matrices, borrowed-view lifetime and copy helpers, large
  event backpressure, and broker fixtures for every mapped event class.

## Verification gaps

These items are test and documentation work unless a failing test reveals a
binding defect.

- **C-WC-01, C-WC-03, C-WC-04:** Verify exact v4/v5 Last Will and MQTT 5
  CONNECT packets at a broker, graceful versus ungraceful Will behavior,
  replacement/clear ownership, all packet-limit and batching boundaries,
  default/reset semantics, MQTT version mismatches, and alias behavior across
  reconnects. Cover missing size prefixes, selectors, reserved fields,
  count/pointer pairs, and integer overflows.
- **C-WC-08, C-WC-09, C-WC-10:** Verify Unix path and shutdown behavior on
  supported platforms and runtime rejection elsewhere; WebSocket header
  add/replace/remove ordering, protected-header rejection, reconnect behavior,
  WSS/proxy composition, and disabled-feature errors; MQTT 5 close packets,
  conflicting concurrent options, escalation, timeout, and repeated completion.
- **C-WC-12:** Verify portable socket settings on supported platforms and
  unsupported-platform errors elsewhere. Exercise capability bits across
  feature builds, unknown-bit forward compatibility, network behavior, and
  captured-output redaction. Document how embedders connect Rust tracing or
  log output without installing a process-global subscriber implicitly.
- **C-WC-13:** Add C fixtures for platform and custom trust, rustls PEM and
  native PKCS#12 mutual TLS, ALPN, hostname and wrong-root rejection,
  malformed credentials, replacement/clear, failed-start cleanup, and
  redaction. Exercise Rustls-only, native-only, mixed, and disabled builds;
  verify TLS and WSS behavior and unchanged legacy signatures. Complete
  static/shared CMake and pkg-config consumer coverage for each supported
  backend combination.

## Requirements for every remaining slice

### ABI and ownership

Follow `docs/c-abi-compatibility.md`. Prefer additive functions and opaque
handles. Give new extensible input records `struct_size`, zeroed reserved
fields, fixed-width selectors, and C11/C++17 initializer macros. Accept
published prefix sizes where documented and reject unknown nonzero reserved
data. Keep existing signatures and loader identity unless the compatibility
policy explicitly requires a new ABI line.

Copy configuration and operation inputs before returning, except documented
callback borrows. Validate pointer/count pairs, sizes, numeric conversions,
MQTT lengths, UTF-8 where required, and optional-field presence before
allocation. Clear `*error_out` on success; on failure, initialize output
parameters and return one owned error where the function promises diagnostic
detail. Keep returned views tied to their owner. Give every new owned
handle a NULL-safe destroy function; never require C callers to free Rust
memory directly. State concurrency and cancellation rules in the header.

Represent passwords, private keys, authentication data, proxy credentials,
cookies, and authorization headers as bytes where appropriate. Redact them
from debug/error/tracing output and zeroize wrapper-owned copies when the core
owner supports it. Do not claim to erase caller, operating-system, TLS-library,
or broker-client copies.

### Documentation and package work

For each slice, update the public header, `native-wrappers/c/README.md`,
`CHANGELOG.md`, and any affected C example or recipe. Document units,
presence, borrowed views, callback threading, shutdown, and ownership. Update
CMake/pkg-config options and dependencies for feature-specific packages.
Examples must compile warning-free as C11, use the C++17-compatible header,
check fallible calls, destroy owners, and use local fixtures. Callback examples
must show shutdown and exactly-once context cleanup.

### Testing and completion

- Add focused Rust FFI tests for malformed inputs, output initialization,
  error ownership, panic containment, callback cancellation, and C-to-core
  field preservation.
- Add deterministic native C broker, proxy, DNS, authentication, TLS,
  WebSocket, and persistence fixtures for the remaining behaviors above.
  Stress callback overlap, duplicate/late completion, one event receiver,
  close, destroy timeout, abandonment, and shared-library unload rules.
- Extend C11/C++17 header smoke, header/export/loader/ABI checks, static and
  shared CMake consumers, pkg-config consumers, and feature-build jobs on each
  supported OS. Compare with an authenticated published ABI baseline when one
  exists. Use sanitizers for callback ownership where supported and Miri for
  new Rust-side unsafe vtable ownership.
- Add fault injection where practical for allocation failure, callbacks that
  never complete, corrupt checkpoints, broker disconnect, DNS/proxy failure,
  and shutdown during callback transitions.

The completed workspace must pass:

```bash
cargo fmt --manifest-path native-wrappers/Cargo.toml --all --check
cargo check --manifest-path native-wrappers/Cargo.toml --workspace
cargo test --manifest-path native-wrappers/Cargo.toml --workspace
native-wrappers/c/tests/abi/check.sh ffi-header
native-wrappers/c/tests/abi/check.sh exports
native-wrappers/c/tests/abi/compare-release.sh
```

Run the CMake/CTest native and package-consumer suites on every supported OS.
A slice is complete only when its C values reach wrapper-core intact, its
ownership and lifecycle contracts hold on success and failure, native tests
cover behavior and shutdown, and its header, examples, package metadata,
exports, and ABI contract agree with the library. Overall parity requires all
supported wrapper-core capabilities to be bound or an explicit C-only omission
recorded in the parity matrix.
