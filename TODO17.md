# Protocol-Specific Wrapper Command Extensions

## Decision

Do not redesign the current wrapper command API solely to replace
`PublishCommand::v5_properties: Option<V5PublishProperties>`. One contained
MQTT 5-only field does not justify a breaking abstraction by itself.

When the wrapper next adds a substantial MQTT 5-only command capability,
replace nullable protocol-specific fields with explicit discriminated extension
types. Apply the change consistently to every affected command in the same
breaking release.

This TODO is intentionally limited to command value design. Driver ownership,
admission, completion, shutdown, protocol state, and module organization belong
to `TODO16.md`.

## Trigger

Implement this TODO when any of the following is approved for the shared
wrapper API:

- MQTT 5 subscription options such as No Local, Retain As Published, or Retain
  Handling;
- MQTT 5 SUBSCRIBE or UNSUBSCRIBE properties;
- MQTT 5 DISCONNECT reason/properties exposed through wrapper commands;
- MQTT 5 AUTH or reauthentication commands; or
- a second independent MQTT 5-only field on an existing protocol-neutral
  command.

Do not add another public field named `v5_*` or another independent
`Option<V5...>` to a protocol-neutral command before implementing this TODO.

## Required shape

Keep common operation data in common structs and place version-specific data in
an explicit enum. Do not duplicate topic, payload, QoS, retain, or filter values
between v4 and v5 variants merely to obtain a discriminant.

One acceptable publish shape is:

```rust
pub struct PublishCommand {
    pub topic: String,
    pub payload: Bytes,
    pub qos: QoS,
    pub retain: bool,
    pub protocol: PublishProtocolOptions,
}

pub enum PublishProtocolOptions {
    VersionNeutral,
    V5(V5PublishProperties),
}
```

`VersionNeutral` means that the command contains no protocol-specific publish
data and may be used by either MQTT version. It does not mean MQTT 3.1.1
specifically. `V5` is valid only for a client configured for MQTT 5.

Use the same pattern for other operations, with operation-specific enums rather
than one untyped property bag:

```rust
pub struct SubscribeCommand {
    pub filters: Vec<Subscription>,
    pub protocol: SubscribeProtocolOptions,
}

pub struct Subscription {
    pub filter: String,
    pub qos: QoS,
    pub protocol: SubscriptionProtocolOptions,
}
```

Command-level SUBSCRIBE properties belong in `SubscribeProtocolOptions`.
Per-filter flags belong in `SubscriptionProtocolOptions`. Do not flatten both
scopes into one structure or correlate parallel vectors by index.

Only define extension enums for operations that actually have protocol-specific
data. Do not create empty abstractions for acknowledge, diagnostics, or another
operation without a concrete version difference.

## Semantic requirements

- The selected client protocol remains immutable and authoritative.
- `VersionNeutral` commands retain identical v4/v5 behavior where MQTT
  semantics genuinely overlap.
- A v5 extension submitted to a v4 client is rejected before request-channel
  admission with `DeliveryStatus::NotAdmitted`.
- No protocol-specific value may be silently ignored, cleared, or converted to
  a weaker version-neutral command.
- MQTT 5 defaults must be represented by the v5 option type when selecting the
  `V5` variant; they must not be inferred from an unrelated absent field.
- Validation and wire conversion remain in the relevant protocol adapter or
  client layer, not in the public enum itself.
- Adding a future protocol-specific extension uses a new enum variant or a
  field in the relevant version-specific options type, subject to the package's
  compatibility policy.

Keep command and completion types separate. Protocol-specific request options
must not alter the existing distinction between admission and tracked MQTT
completion.

## Native wrapper mapping

C, Python, JavaScript, and other bindings should expose idiomatic discriminated
options and translate them into the Rust extension enums. They must not expose
Rust enum layout directly.

For the C ABI, use stable selectors and opaque or size-versioned input
structures. Preserve existing ABI declarations unless a separately approved ABI
revision is required. Unknown selectors and v5 data on a v4 client must return a
stable invalid-argument or protocol-option error.

For typed language wrappers, prefer discriminated unions so incompatible
options are rejected by the type checker where possible. Runtime validation
remains mandatory for untyped callers and foreign values.

## Migration

When the trigger occurs:

1. Add the required version-specific option types for the approved feature.
2. Replace `PublishCommand::v5_properties` with
   `PublishCommand::protocol` in the same breaking Rust API change.
3. Update all wrapper construction, protocol mapping, tests, examples, and
   documentation without retaining two equivalent public representations.
4. Add convenience constructors such as `PublishCommand::version_neutral(...)`
   only if they materially reduce boundary boilerplate.
5. Update `CHANGELOG.md` with mechanical migration examples.

Do not keep the old field and new enum indefinitely. A short deprecation period
is acceptable only if the release policy requires one and there is one
authoritative conversion path.

## Tests

For each affected operation, test:

- version-neutral behavior on v4 and v5;
- v5 extensions on a v5 client;
- rejection of every v5 variant on a v4 client before admission;
- preservation of all properties through conversion to the v5 packet type;
- default v5 option behavior;
- validation failures retaining `NotAdmitted` delivery status; and
- native wrapper conversion, including unknown discriminants where applicable.

Retain protocol-parity tests for the version-neutral path. Keep v5-only behavior
in explicitly named v5 tests rather than manufacturing a false v4 equivalent.

## Completion criteria

This TODO is complete when the trigger feature is implemented and:

- affected common commands contain no independent nullable `v5_*` fields;
- common data is stored once;
- protocol-specific data is explicitly discriminated and scoped to the correct
  operation or filter;
- incompatible variants are rejected before admission;
- all native wrappers map the new shape without exposing Rust layout; and
- tests, examples, API documentation, and `CHANGELOG.md` describe the migration
  and protocol behavior.

