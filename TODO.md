# Wrapper Architecture TODO

## Objective

Keep one `rumqttc-wrapper-core` architecture and one host-language wrapper API for MQTT 3.1.1 and
MQTT 5, while making the protocol boundary stricter and making direction-specific MQTT 5 publish
properties explicit.

The completed structure should have:

- protocol-neutral lifecycle, admission, completion, shutdown, diagnostics, and event-delivery code;
- one internal enum-dispatched backend with separate MQTT 3.1.1 and MQTT 5 implementations;
- no direct dependency on `rumqttc_v4` or `rumqttc_v5` types outside the backend boundary; and
- distinct types for MQTT 5 properties sent by a client and properties received from a broker.

## Invariants and Non-Goals

- [ ] Preserve a single `rumqttc-wrapper-core-next` package supporting both protocols.
- [ ] Preserve a single `rumqttc-c-next` library and C ABI supporting both protocols.
- [ ] Keep protocol selection explicit, immutable for a client's lifetime, and represented by
      `ProtocolConfig`/`ProtocolVersion`.
- [ ] Continue rejecting protocol-incompatible options before request-channel admission.
- [ ] Preserve all existing admission, backpressure, completion, manual-acknowledgement, reconnect,
      and shutdown guarantees.
- [ ] Do not create independent v4 and v5 wrapper-core implementations.
- [ ] Do not introduce a dynamic backend trait or erase protocol types behind `dyn Trait`.
- [ ] Do not force the two typed event-loop polling implementations into a generic loop. Extract only
      small helpers whose semantics are demonstrably identical.
- [ ] Do not add protocol Cargo features unless separate measurements establish a material binary-size
      or build-time benefit.

## Phase 1: Establish the Backend Boundary

- [ ] Rename `rumqttc-wrapper-core/src/adapter/` to `rumqttc-wrapper-core/src/backend/` to reflect that
      it owns native clients and drivers, not only value conversion.
- [ ] Rename the MQTT 3.1.1 implementation from `v4.rs` to `v311.rs`; retain `v5.rs` for MQTT 5.
- [ ] Move `ProtocolClient` from `handle.rs` into `backend/mod.rs` and rename it `BackendClient`.
- [ ] Rename `AdapterDriver` to `BackendDriver` and keep construction and driver dispatch in
      `backend/mod.rs`.
- [ ] Move `PreparedAck` and its matches over native `ManualAck` values into the backend module.
      The acknowledgement coordinator may store the resulting opaque internal value, but it must not
      name either protocol crate or inspect native acknowledgement variants.
- [ ] Replace the protocol-specific methods on `Shared` that accept native publish types with a
      protocol-neutral handoff using the backend-owned prepared-ack representation.
- [ ] Move all direct calls to `rumqttc_v4::AsyncClient` and `rumqttc_v5::AsyncClient` behind methods
      on `BackendClient`. Cover at least:
  - [ ] publish admission and tracked completion;
  - [ ] subscribe admission and tracked completion;
  - [ ] unsubscribe admission and tracked completion;
  - [ ] manual acknowledgement preparation and admission;
  - [ ] graceful disconnect, including an optional timeout;
  - [ ] immediate disconnect; and
  - [ ] best-effort finalizer shutdown.
- [ ] Have backend admission methods accept wrapper-core command values and return protocol-neutral
      `Result` values and completion futures/notices suitable for the existing operation registry.
- [ ] Keep the backend implementation enum-dispatched. A single match per backend operation is
      acceptable and preferred over a generic or object-safe trait hierarchy.

### Phase 1 acceptance criteria

- [ ] `handle.rs`, `acknowledgement.rs`, `runtime.rs`, and all other files outside `backend/` contain no
      `rumqttc_v4::` or `rumqttc_v5::` paths.
- [ ] The following audit returns no matches:

  ```console
  rg -n 'rumqttc_v[45]::' rumqttc-wrapper-core/src \
      --glob '!**/backend/**'
  ```

- [ ] `ClientHandle` is responsible for lifecycle/admission ordering and delegates protocol work to
      `BackendClient`; it does not match on native client types.
- [ ] The v3.1.1 and v5 event loops remain separately typed and independently readable.

## Phase 2: Separate Incoming and Outgoing MQTT 5 Publish Properties

- [ ] Replace the shared `V5PublishProperties` model with two explicit types:
  - [ ] `V5OutgoingPublishProperties` for client-originated PUBLISH packets; and
  - [ ] `V5IncomingPublishProperties` for broker-originated PUBLISH packets.
- [ ] Define `PublishProtocolOptions::V5` in terms of `V5OutgoingPublishProperties`.
- [ ] Define `IncomingPublish::v5_properties` in terms of
      `Option<V5IncomingPublishProperties>`.
- [ ] Include only properties legal for a client-originated PUBLISH in
      `V5OutgoingPublishProperties`. In particular, do not expose Subscription Identifiers on the
      outgoing type.
- [ ] Preserve all observable incoming MQTT 5 properties, including Subscription Identifiers, on
      `V5IncomingPublishProperties`.
- [ ] Split the MQTT 5 conversion functions into directionally named conversions, such as
      `to_outgoing_publish_properties` and `from_incoming_publish_properties`.
- [ ] Remove validation whose only purpose was rejecting fields that the outgoing type can no longer
      represent.
- [ ] Retain semantic validation that types alone cannot enforce, including MQTT UTF-8 constraints,
      encoded length limits, payload-format consistency, topic syntax, property cardinality, and
      negotiated Topic Alias restrictions.

### C wrapper migration

- [ ] Update the C wrapper's outgoing publish-property parser to construct
      `V5OutgoingPublishProperties`.
- [ ] Update incoming event accessors to read `V5IncomingPublishProperties`.
- [ ] Preserve the existing direction-specific C API behavior: outgoing property structures must not
      gain a Subscription Identifier, while incoming event accessors must continue exposing received
      Subscription Identifiers.
- [ ] Regenerate `rumqttc-c/include/rumqttc.h` with `cbindgen` and verify that no unintended ABI change
      occurred.

### Phase 2 acceptance criteria

- [ ] It is impossible to construct a client-originated publish containing a Subscription Identifier
      through the wrapper-core Rust model.
- [ ] Incoming publishes continue to retain and expose all received Subscription Identifiers.
- [ ] MQTT 3.1.1 publishes cannot carry MQTT 5 properties.
- [ ] Version-neutral MQTT 5 publishes continue to work without allocating a default property value.

## Phase 3: Localize Validation and Translation

- [ ] Keep genuinely shared validation in the protocol-neutral layer, including:
  - [ ] nonempty publish/subscribe/unsubscribe inputs where required by both protocols;
  - [ ] common topic and filter syntax checks;
  - [ ] common MQTT UTF-8 and two-byte encoded-length constraints; and
  - [ ] wrapper-level channel, timeout, transport, and TLS configuration invariants.
- [ ] Move MQTT 5-only publish, subscribe, per-filter subscription, and unsubscribe validation into
      `backend/v5.rs` or a private child module of that backend.
- [ ] Keep MQTT 3.1.1-only configuration and authentication validation either in `backend/v311.rs` or
      in clearly identified protocol-specific branches of `ClientConfig::validate`.
- [ ] Ensure backend-specific errors are normalized to wrapper `ErrorKind`, `DeliveryStatus`, and
      `BrokerReason` values before leaving the backend.
- [ ] Keep `VersionNeutral` behavior explicit; never silently discard MQTT 5 options when the selected
      client is MQTT 3.1.1.

## Phase 4: Tests and Regression Coverage

- [ ] Run the existing wrapper-core test suite before refactoring and record a clean baseline.
- [ ] Retain and expand `rumqttc-wrapper-core/tests/protocol_parity.rs` so every shared operation is
      exercised against both protocol backends where broker behavior overlaps.
- [ ] Add or retain focused tests for:
  - [ ] protocol-incompatible options being rejected before admission;
  - [ ] v3.1.1 and v5 publish, subscribe, and unsubscribe completions;
  - [ ] v5 broker reason-code preservation;
  - [ ] incoming v5 Subscription Identifier preservation;
  - [ ] manual ACK token creation, retransmission deduplication, retry restoration, and completion;
  - [ ] reconnect invalidation of connection-scoped ACK state;
  - [ ] graceful and immediate shutdown ordering for both protocols;
  - [ ] event-buffer overload behavior for both protocols; and
  - [ ] diagnostics and terminal status under both backends.
- [ ] Add a source-boundary check to CI or the repository's validation script that fails if direct
      `rumqttc_v4::` or `rumqttc_v5::` paths appear outside `rumqttc-wrapper-core/src/backend/`.
- [ ] Verify the generated C header and run the C wrapper's Rust-side ABI tests.

### Required validation commands

- [ ] `cargo fmt --all --check`
- [ ] `cargo test -p rumqttc-wrapper-core-next`
- [ ] `cargo test -p rumqttc-c-next`
- [ ] `cargo check --workspace`
- [ ] If feature-sensitive client code changes during the work, run:

  ```console
  cargo hack --each-feature --exclude-all-features test \
      -p rumqttc-v4-next -p rumqttc-v5-next
  ```

## Phase 5: Documentation and Release Notes

- [ ] Update `rumqttc-wrapper-core/README.md` to describe the backend boundary and directional MQTT 5
      publish-property types.
- [ ] Update wrapper-core API documentation and examples to use the new incoming/outgoing type names.
- [ ] Update `CHANGELOG.md` because renaming the published wrapper-core property type is a user-visible
      pre-stable API change.
- [ ] Document any intentional C source or ABI changes. If none are intended, explicitly record that
      the generated header was compared and remained compatible.
- [ ] In the final change description, state that the refactor does not alter protocol selection,
      runtime ownership, admission semantics, or the one-package support contract.

## Completion Criteria

- [ ] Both protocols are supported by the same wrapper-core and C wrapper packages.
- [ ] All native protocol-crate types and calls are confined to `backend/`.
- [ ] Shared lifecycle and concurrency machinery has one implementation.
- [ ] Protocol differences remain explicit at configuration, command, event, completion, and error
      boundaries.
- [ ] Incoming and outgoing MQTT 5 publish properties use distinct, directionally correct models.
- [ ] All required tests, formatting checks, workspace checks, documentation, and changelog updates are
      complete.
