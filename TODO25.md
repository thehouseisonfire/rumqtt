# C Wrapper Custom Transport Connectors

## Goal

Expose application-provided asynchronous connections and byte streams through
the native wrapper core and C API for both MQTT versions. Support custom
tunnels, supplied connections, and simulated transports while retaining native
MQTT scheduling, framing, tracking, and recovery.

## Current foundation and feasibility

Both client crates implement `MqttOptions::set_socket_connector` in
`rumqttc-v4/src/lib.rs` and `rumqttc-v5/src/lib.rs`. The connector supplies the
base stream before configured proxy, TLS, and WebSocket layers. The omission in
`native-wrappers/wrapper-core/PARITY.md` concerns the foreign I/O contract, not
an inability of the clients to accept custom streams.

This is feasible after a sound stream adapter has been demonstrated. Do not
publish the C interface before partial I/O, wakeups, cancellation, and owner
release have explicit implementations. No dependency on a portable client
rewrite or the browser wrapper is required.

## Implementation requirements

- [ ] Add owned connector and stream abstractions in wrapper-core, with one
  backend adapter per protocol delegating to the existing native connector.
- [ ] Add opaque retained C registration, stream, and deferred-operation
  handles. Use size-versioned records and fixed-width selectors consistent
  with the existing callback registrations and C ABI policy.
- [ ] Pass the target, applicable network settings, connection generation, and
  deadline to connection callbacks. A connector owns socket creation and must
  explicitly apply or reject network settings; the wrapper must not pretend
  that it configured a foreign stream after creation.
- [ ] Define read, write, flush, and shutdown operations, including short
  transfers, EOF, recoverable pending work, permanent errors, and wakeups.
  Define whether read and write may overlap and prohibit accidental concurrent
  reads or writes on the same stream.
- [ ] Keep all memory alive across deferred I/O. Prefer library-owned operation
  buffers and copied completion results; never lend a Rust stack buffer to
  work that may outlive the callback. Bound buffer sizes and outstanding work.
- [ ] Make callbacks return promptly and permit completion on another thread.
  Run them without lifecycle or admission locks. A timeout cannot preempt a
  blocking C callback; document that obligation instead of claiming it can.
- [ ] Reject duplicate, late, and stale-generation completions. Cancellation
  must stop observation without creating another active operation on an
  incompletely cancelled stream; discard that connection when necessary.
- [ ] Specify retained operation, stream, registration, and client ownership
  independently. Destruction occurs exactly once after the last owner, and
  shutdown cannot call foreign code after its registration is released.
- [ ] Preserve transport composition. Explicitly distinguish an unencrypted
  base stream from one already providing TLS or proxy negotiation to prevent
  accidental double layering. Reconnect obtains a new stream.
- [ ] Add capability reporting and typed connector failures without exposing
  arbitrary callback error text or credentials.

## Verification and completion

- [ ] Demonstrate both protocols with an in-memory stream and a custom tunnel.
- [ ] Exercise short reads/writes, synchronous and deferred completion, EOF,
  missed-wakeup races, concurrent close, timeout, reconnect, and abandonment.
- [ ] Verify TLS/WebSocket composition and exactly-once destruction after
  failed construction and retained late completions.
- [ ] Update the C header generation, exports, README, examples, `PARITY.md`,
  and root `CHANGELOG.md` under `docs/c-abi-compatibility.md`.

Complete this TODO only when a native C consumer can supply its transport
without implementing MQTT or depending on Rust layouts. Exposing raw socket
descriptors alone does not complete custom stream support.
