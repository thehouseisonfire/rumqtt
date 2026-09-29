# C Wrapper Runtime Configuration Updates

## Goal

Allow safe updates to supported client configuration after startup, including
credential/TLS rotation for later connections and runtime batching controls.
Expose effective and pending configuration rather than mutable native objects.

## Current foundation and feasibility

Rust exposes options through the event loop and explicit network-option setters.
The wrapper consumes `ClientConfig` at startup and exposes no live update API.
Mutating a Rust options field does not prove that all derived runtime state
changes correctly, so setters need individual applicability audits.

Updates are feasible as serialized driver commands with defined activation
boundaries. Protocol version, active packet-ID state, and queue ownership must
not change by replacing an entire options object while connected.

## Implementation requirements

- [ ] Inventory native setters and classify each as construction-only,
  immediately applicable, next-attempt, or requiring an explicit session reset.
  Check actual reads and derived state in both event loops, not just visibility.
- [ ] Initially support audited batching/throttle controls and next-attempt
  credentials, TLS profiles, network settings, and handshake authorities.
  Expand only after proving native behavior for additional fields.
- [ ] Add typed partial update objects and tracked C commands. Copy all values
  and retain registrations at admission; validate the complete proposed change
  before modifying driver configuration.
- [ ] Apply updates atomically on the driver. Assign a monotonic configuration
  revision and distinguish accepted, staged, and effective revisions.
- [ ] Define whether a connection attempt already in progress uses its original
  snapshot or is explicitly cancelled and restarted. Never combine old broker
  credentials with new TLS or authentication identity inadvertently.
- [ ] Let callers request activation at a subsequent reconnect or a separately
  admitted controlled reconnect. Preserve native cleanup and recovery for that
  transition; configuration update alone must not silently drop work.
- [ ] Keep existing callback/secret owners until active work releases them.
  Wipe retired owned secret buffers and reject completions for stale generations.
- [ ] Reject protocol changes, live queue resizing, store-owner/scope changes,
  client-ID changes, and session-policy changes unless an explicit safe native
  transition has been implemented. Route destructive changes through
  [TODO36.md](TODO36.md) or require a new client.
- [ ] Reject unsupported fields with a typed error and no partial update.
  Do not accept a setter whose effect is only a changed diagnostic value.
- [ ] Expose revision and activation status through
  [TODO37.md](TODO37.md). Keep startup configuration handles independent of
  already running clients and preserve their existing semantics.

## Verification and completion

- [ ] Rotate broker credentials and TLS identities across reconnect and verify
  the peer sees one coherent revision with no old-secret reuse.
- [ ] Verify effective batching/throttle changes and network options on newly
  created sockets for both protocols.
- [ ] Cover invalid-update rollback, concurrent commands, pending callbacks,
  failed activation, shutdown races, and retired owner release.
- [ ] Update C header/exports, README, rotation example, `PARITY.md`, and root
  `CHANGELOG.md` with an explicit field applicability table.

Complete for the documented supported field set. Do not advertise arbitrary
live `MqttOptions` mutation as a safe C capability.
