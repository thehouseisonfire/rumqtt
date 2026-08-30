# Validate the Shared Native-Wrapper Boundary

## Goal

Validate `rumqttc-wrapper-core-next` through a second native host-language
wrapper. The second wrapper must consume the shared crate for MQTT protocol
selection, command admission, tracked completion, event delivery, error
classification, and client shutdown.

Choose either the JavaScript/TypeScript wrapper described in `TODO6.md` or the
Python wrapper maintained in `native-wrappers/python/`. Do not create a
disposable validation wrapper solely to satisfy this requirement.

## Integration requirements

The wrapper must:

- ship MQTT 3.1.1 and MQTT 5 support through one host-language package;
- select exactly one protocol for each client instance;
- translate host-language values at the wrapper boundary without duplicating
  MQTT v4/v5 translation or lifecycle logic;
- expose nonblocking or asynchronous admission appropriate to the host runtime;
- distinguish admission from MQTT completion;
- continuously drain the bounded event stream;
- surface terminal event-buffer overflow without silently dropping publishes;
- preserve opaque, single-use manual-acknowledgement tokens;
- map structured errors without matching formatted Rust error messages; and
- provide bounded, idempotent cleanup that is safe for the host runtime's
  finalization and shutdown behavior.

Keep host-runtime scheduling, callbacks, memory ownership, exceptions, futures
or promises, package loading, and finalizer hooks in the wrapper. Do not add
Python, Node-API, JavaScript-engine, or C ABI types to
`rumqttc-wrapper-core-next`.

## Validation

Add broker-backed integration tests for both MQTT versions covering:

- connection and session-present reporting;
- publish completion at QoS 0, QoS 1, and QoS 2;
- subscribe and unsubscribe completion;
- MQTT 5 broker rejection details;
- recoverable disconnect and reconnection;
- request-channel backpressure;
- event-buffer overflow and terminal failure delivery;
- manual acknowledgement, including token reuse rejection;
- cancellation or abandonment of a host waiter without cancellation of
  admitted MQTT work;
- graceful and immediate shutdown; and
- repeated construction and destruction without leaked driver threads or
  callbacks into a terminated host environment.

Run the wrapper's native, host-language, packaging, and lifecycle test suites
on every supported target. Continue running:

```text
cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next
cargo test -p rumqttc-v4-next
cargo test -p rumqttc-v5-next
```

## Boundary review

During integration, record every place where the wrapper must work around,
duplicate, or bypass the shared crate. Change the shared boundary only for a
correctness invariant or semantics required by at least two wrappers. Keep
single-wrapper conveniences in the host wrapper.

After the second wrapper passes its integration and lifecycle suites, perform
the evidence-driven boundary review in `TODO11.md`.

## Completion criteria

This TODO is complete when:

- a second production wrapper consumes `rumqttc-wrapper-core-next`;
- both wrappers pass MQTT 3.1.1 and MQTT 5 integration tests;
- shared protocol translation and lifecycle behavior are not duplicated in the
  wrappers;
- overload, completion, manual-acknowledgement, and shutdown semantics map
  cleanly into both host APIs; and
- any integration findings are captured as tests or as concrete follow-up work
  in `TODO11.md`.
