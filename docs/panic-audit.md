# Panic audit

This audit covers `panic!`, `assert!`, `expect`, and `unwrap` in the library
targets of `mqttbytes-core`, `rumqttc-core`, `rumqttc-v4`, `rumqttc-v5`, and
`rumqttc-next`. It was checked against the default workspace build and every
individual crate feature. Line numbers below identify the audited revision;
the invariant named in each row is the authoritative description if code moves.

`unwrap_or`, `unwrap_or_else` without a panic, and `Result::unwrap_err` in tests
are not panic sites on the successful library paths under review. The
`unreachable!` sites found during the same search are covered separately below.

## Production sites

### Compatibility entry points

These functions preserve existing infallible APIs. Each delegates directly to
the listed fallible API, and is therefore classified as temporary compatibility
behavior rather than a justified library invariant.

| Site | Classification | Recommendation |
| --- | --- | --- |
| `rumqttc-core/src/lib.rs:122`, `TlsConfiguration::default_rustls` | Compatibility | Prefer `try_default_rustls`; deprecate the infallible constructor in the next API cleanup. |
| `rumqttc-v4/src/transport.rs:69,177`, `rumqttc-v5/src/transport.rs:69,179` | Compatibility | Prefer `try_tls_with_default_config` and `try_wss_with_default_config`; deprecate the infallible constructors together. |
| `rumqttc-v4/src/client.rs:540,600`, `rumqttc-v5/src/client.rs:583,644` | Compatibility | Prefer `ClientBuilder::try_build` and `AsyncClientBuilder::try_build`; retain `build` only for source compatibility, then deprecate it in a coordinated release. |
| `rumqttc-v4/src/lib.rs:813,903,926,1094` | Configuration/user input | Prefer `try_set_client_id`, `try_set_clean_session`, `try_set_session_mode`, and `try_set_inflight`, which return `ConfigError`; deprecate the panicking setters and their builder wrappers together. |

The compatibility functions have documented `# Panics` sections and a fallible
replacement today, so callers do not need to parse panic text.

### MQTT v4 internal invariants

| Sites | Invariant | Recommendation |
| --- | --- | --- |
| `rumqttc-v4/src/eventloop.rs:852,1142,1168,1325,1350,1397,1412,1464` | `network` is `Some` only inside the connected polling phase. It is private and initialized before these helpers run. | Keep as a descriptive internal `expect`. If the event-loop phase model is refactored, replace the `Option` with a connected-state type rather than adding a public error. |
| `rumqttc-v4/src/eventloop.rs:1158,1178,1197,1477` | A state transition queues its corresponding outgoing `Event` before the event loop removes it. | Keep as a descriptive internal `expect`. |
| `rumqttc-v4/src/eventloop.rs:1576` | The non-empty branch of `next_request` still owns a front item after the cancellation-safe delay. | Keep as a descriptive internal `expect`. |
| `rumqttc-v4/src/state.rs:624,631` | Packet identifiers produced by the bounded MQTT bitset fit in `u16`, and all tracking vectors are grown together. | Keep as an internal invariant. |
| `rumqttc-v4/src/state.rs:1159,1196,1203,1256` | An acknowledged packet identifier was first found in the parallel publish/PUBREL tracking collection, whose notice vector has identical capacity. | Keep as an internal invariant. A missing primary entry already returns `StateError::Unsolicited`. |
| `rumqttc-v4/src/state.rs:1625,1708` | Collision replay occurs only after releasing the identifier; the identifier count is decremented only when the bitset says it is reserved. | Keep as an internal invariant. Preserve the checked arithmetic as a corruption detector. |

### MQTT v5 internal invariants

| Sites | Invariant | Recommendation |
| --- | --- | --- |
| `rumqttc-v5/src/eventloop.rs:971,1272,1298,1502,1527,1583,1601,1619,1678,1684` | `network` is present only in connected polling/drain code. It is private and established before these helpers run. | Keep as a descriptive internal `expect`; prefer a connected-state type if this code is reorganized. |
| `rumqttc-v5/src/eventloop.rs:1288,1308,1334,1703` | A successful state transition has queued the `Event` immediately removed by the event loop. | Keep as a descriptive internal `expect`. |
| `rumqttc-v5/src/eventloop.rs:1666` | `send_pending_disconnect` is called only after the caller has observed a pending disconnect. | Keep as a descriptive internal `expect`. |
| `rumqttc-v5/src/eventloop.rs:1762` | The non-empty branch of `next_request` retains its front item across the cancellation-safe delay. | Keep as a descriptive internal `expect`. |
| `rumqttc-v5/src/state.rs:971` | Packet identifiers yielded by the MQTT-sized bitset fit in `u16`. | Keep as an internal invariant. |
| `rumqttc-v5/src/state.rs:1583,1623` | SUBACK/UNSUBACK handlers check the pending map before removing the same entry, with no intervening mutation. | Keep as an internal invariant; protocol mismatches already return `StateError::ProtocolViolation`. |
| `rumqttc-v5/src/state.rs:1909,2531` | Collision replay reserves an identifier only after its previous flow releases it; the reserved count is decremented only for a set bit. | Keep as an internal invariant. |
| `rumqttc-v5/src/state.rs:2416` | AUTH packets created by the state machine always contain the properties installed by the authentication exchange. | Keep as an internal invariant. |

There are no production `assert!` sites and no remaining protocol
decode/encode sites using the four audited operations. Codec failures are
returned through `mqttbytes_core::Error`, v4/v5 `mqttbytes::Error`, or
`StateError`. `rumqttc-next` is a re-export facade and has no production panic
sites.

## Removed during this audit

The following sites did not need to retain panic behavior:

- `mqttbytes-core::parse_fixed_header` now returns `InsufficientBytes` instead
  of unwrapping the iterator after its length check.
- `mqttbytes-core::valid_filter` is total and no longer unwraps `split`.
- v4 and v5 WebSocket setup use a static `HeaderValue` instead of parsing and
  unwrapping the constant `mqtt` subprotocol.
- TLS backend mismatches return `UnsupportedBackendConfiguration` instead of
  `panic!`/`unreachable!`.

These are defensive changes. They do not change errors for valid input.

## Test and example sites

Every remaining site under any `tests/`, `examples/`, or `benches/` directory,
and every remaining site inside a `#[cfg(test)]` module in `src/`, is classified
as test-only or example-only. Assertions and unwraps there express fixture
preconditions or fail the test with local context. They must not be moved into
production code unchanged; otherwise no migration is recommended.

This path/module rule is exhaustive for the test and example sites and avoids
duplicating thousands of mechanically equivalent line entries.

## Supplemental `unreachable!` review

Although it was not explicitly named in the task, `unreachable!` also panics.
The production instances fall into these groups:

- v4/v5 codec matches over values already validated by the fixed-header/parser
  layer;
- v4/v5 framed-reader branches where `check` has already established a complete
  frame;
- graceful-disconnect request variants intercepted by the event loop before
  state dispatch;
- Unix transport match arms that follow an earlier Unix-specific return.

Those are private control-flow invariants. TLS backend mismatch arms were the
exception and now return a typed error because feature combinations can make
multiple public backend variants constructible.

## Ongoing policy

Run the following when adding a panic-like operation to a library path:

```text
cargo hack --each-feature --exclude-all-features --optional-deps url clippy \
  -p rumqttc-v4-next -p rumqttc-v5-next --lib -- \
  -W clippy::unwrap_used -W clippy::expect_used -W clippy::panic
```

New user/configuration failures belong in `ConfigError` or an existing builder
error. New wire-data failures belong in the codec/state error taxonomy.
Internal invariant panics should state the invariant and remain unreachable
through public API misuse.
