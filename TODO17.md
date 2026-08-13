# Protocol-Specific Wrapper Command Extension Acceptance

## Goal

Complete the acceptance evidence for the protocol-specific publish, subscribe,
per-filter subscription, and unsubscribe command options exposed by
`rumqttc-wrapper-core-next` and `rumqttc-c-next`.

The acceptance suite must prove that protocol-specific values retain their
scope and discriminant across Rust and C boundaries, are rejected before
admission when incompatible with the selected client protocol, and reach the
MQTT 5 wire representation without loss.

## Rust wrapper tests

Add independent MQTT 3.1.1 rejection cases for every public MQTT 5 command
variant:

- `PublishProtocolOptions::V5`;
- `SubscribeProtocolOptions::V5`;
- `SubscriptionProtocolOptions::V5`; and
- `UnsubscribeProtocolOptions::V5`.

Each case must select only the variant under test, call the wrapper admission
API, and assert `ErrorKind::Admission` with
`DeliveryStatus::NotAdmitted`. The broker fixture must also prove that no
corresponding MQTT application packet was emitted.

Add wrapper-level validation cases for MQTT 5 subscription data, including:

- zero and out-of-range Subscription Identifiers;
- invalid and oversized SUBSCRIBE User Property names and values;
- invalid and oversized UNSUBSCRIBE User Property names and values; and
- No Local on a Shared Subscription.

Each validation failure must retain `DeliveryStatus::NotAdmitted` and must not
emit a packet. Keep these cases in explicitly named MQTT 5 tests.

## Native C acceptance

Exercise each C protocol-options boundary independently. Tests must cover:

- unknown publish, SUBSCRIBE, per-filter subscription, and UNSUBSCRIBE
  selectors;
- a version-neutral selector accompanied by MQTT 5 data;
- an MQTT 5 selector without its required size-versioned record;
- undersized records at every newly introduced record boundary;
- invalid boolean fields and every invalid Retain Handling value; and
- MQTT 5 publish, SUBSCRIBE, per-filter subscription, and UNSUBSCRIBE options
  submitted to an MQTT 3.1.1 client.

Foreign-input failures must return the stable invalid-argument or
protocol-option status appropriate to their boundary, initialize all supplied
outputs, and leave the request unadmitted.

Extend the broker-backed C test so it decodes or byte-checks the emitted MQTT 5
packets and verifies:

- every supported SUBSCRIBE property;
- No Local, Retain As Published, and all three Retain Handling values;
- default-valued MQTT 5 SUBSCRIBE and per-filter option records; and
- every supported UNSUBSCRIBE property.

Do not infer success solely from receipt of `SUBACK` or `UNSUBACK`; the test
must inspect the outbound packet contents.

## Migration documentation

Add mechanical before-and-after examples to `CHANGELOG.md` for:

- Rust publish construction using `PublishCommand::protocol`;
- Rust SUBSCRIBE construction with distinct command-level and per-filter
  protocol options;
- Rust UNSUBSCRIBE construction using `UnsubscribeCommand`; and
- the revised C publish, subscribe, and unsubscribe option records and function
  arguments.

Examples must identify the version-neutral form and the explicit MQTT 5 form.
Keep the examples concise and directly compilable after ordinary imports or
header inclusion.

## Verification

Run and record successful results for:

```text
cargo test -p rumqttc-v5-next -p rumqttc-wrapper-core-next -p rumqttc-c-next
cargo check --workspace
cargo clippy -p rumqttc-v5-next -p rumqttc-wrapper-core-next -p rumqttc-c-next --all-targets -- -D warnings
rumqttc-c/tests/abi/check.sh ffi-header
cmake --build target/rumqttc-c-native
ctest --test-dir target/rumqttc-c-native --output-on-failure
```

## Completion criteria

This TODO is complete when every protocol-specific variant and C selector has
independent positive and negative coverage, C wire tests inspect all supported
SUBSCRIBE and UNSUBSCRIBE extensions, every incompatible or malformed input is
proven unadmitted, and `CHANGELOG.md` contains mechanical Rust and C migration
examples.
