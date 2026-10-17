# Opt-In Native Protocol Observation Events

Status: complete (Linux x86_64 validation; platform and historical ABI limits below).

## Goal

Expose precise native packet activity and intermediate incoming control packets
for diagnostics, while keeping MQTT state transitions and completion authoritative.

## Foundation and scope

Wrapper-core filters incoming control packets and maps outgoing PUBACK, PUBREC,
PUBREL, and PUBCOMP into one activity. PINGREQ/PINGRESP also share a category.
Terminal ACK details from [TODO29.md](TODO29.md) are already implemented; this
task adds optional event observations, including successful intermediate QoS 2
PUBREC/PUBREL and incoming PINGRESP. Incoming publish identifiers are covered by
[TODO43.md](TODO43.md).

This is an observation API over events the native clients already emit, not a
complete wire transcript. Native processing can suppress duplicate PUBLISH events
or reject packets without emitting an incoming event. Outgoing events generally
provide a kind and optional identifier, not the packet's reason or properties.
Full outgoing packet contents, additional native emission points, and handshake
history are outside this task.

The public API scope is wrapper-core and C. Keep Python and JavaScript compatible
with shared-core changes; exposing observation selection or new details in their
public APIs is a separate follow-up.

## Implementation requirements

- [x] Inventory the native events actually emitted by both clients and define
  an availability matrix by protocol, direction, and event kind. Define a typed
  observation model with direction, exact packet kind, optional packet ID,
  connection generation, and available reason codes and legal packet-level
  properties. Represent unavailable fields explicitly; do not infer outgoing
  contents from defaults, submitted commands, or current protocol state. Preserve
  ordered duplicate User Properties, per-filter reason-code order, and absent
  versus present-empty values where native events retain those distinctions.
- [x] Treat `AwaitAck` as non-packet scheduling activity. Preserve its existing
  coarse event without inventing an MQTT packet kind. An enum variant's existence
  alone does not establish that a native client emits it on a supported path.
- [x] Preserve existing coarse outgoing events and accessors. Add precise detail
  through additive accessors and an explicit startup observation selector, disabled
  by default. Support separate selection of previously filtered incoming control
  packets, precise outgoing packet activity, and additional raw AUTH observations.
  Do not change numeric constants or default event sequences. For outgoing packets,
  use the following interaction with `emit_outgoing_events`:

  | `emit_outgoing_events` | Precise outgoing selected | Delivery per native outgoing packet event |
  | --- | --- | --- |
  | false | false | No event |
  | true | false | One existing coarse outgoing event |
  | false | true | One outgoing event with coarse activity and precise detail |
  | true | true | The same single enriched outgoing event; no duplicate |

  Keep non-packet `AwaitAck` delivery governed by `emit_outgoing_events`. Reuse
  existing incoming event representations where they already expose a packet;
  selecting incoming control observations must not duplicate existing PUBLISH,
  CONNACK, or broker DISCONNECT events. Raw AUTH observation is selected separately.
- [x] Map actual native observations before their information is discarded.
  Incoming events describe native processing; outgoing events describe the native
  event's existing scheduling boundary, not necessarily a successful socket flush
  or peer receipt. Do not invent CONNECT/AUTH/other history that Rust did not emit.
  Define connection-generation applicability before accepted CONNACK, during
  reconnect attempts, and for replay. Follow TODO43's accepted-connection semantics;
  packet IDs can be reused within a generation and are not unique operation keys.
- [x] Keep AUTH response authority with the configured authenticator. AUTH
  lifecycle events already expose some broker reason/properties; preserve those
  accessors and their existing delivery regardless of the new selector. Additional
  raw AUTH observations require explicit selection and must never create competing
  challenge-response tokens or allow responses through observation handles.
- [x] Deliver through the existing bounded event channel and its explicit overflow
  failure contract. Document that enabling observations increases queue pressure:
  a full queue can pause native polling and terminate the driver after the delivery
  timeout. This is reliable delivery within that contract, not nonblocking or
  best-effort telemetry. Define ordering among observations and existing events
  from the same native stream, plus their relationship to completions; do not
  promise cross-channel total ordering or wait for an observation consumer before
  making an already available terminal result observable. Avoid duplicate native
  polling. A separate channel would require revising this plan with bounded
  loss/accounting and teardown semantics before implementation.
- [x] Retain immutable owned packet details only while an event is queued, being
  delivered, or retained by a caller. Release them with the final owner, including
  after client close. Share storage with terminal outcomes when practical, without
  a completed-packet cache, delayed completions, or new cross-channel coordination
  solely to eliminate a copy. Keep the disabled path free of added property cloning
  and history allocation. Expose no mutable packets, raw ACK submission, or exact
  original wire encoding promise.
- [x] Redact newly retained packet properties and AUTH data from Debug, formatted
  errors, and automatic logs, including enclosing event/detail types. Explicit
  accessors may return event-owned borrowed data with a documented owner lifetime.
- [x] Update exhaustive Python/JavaScript event mappings as needed for shared-core
  compatibility. Keep new selection disabled for their existing configurations and
  preserve their public event formats and default sequences.

## Verification and completion

- [x] Consult both spec digests in `docs/spec/`; verify exact QoS 1/2 incoming and
  outgoing packet kinds, intermediate success and rejection, QoS 2 replay, and
  incoming PINGRESP with deterministic native C consumers. Assert unavailable
  outgoing reason/properties remain unavailable, including PUBCOMP generated with
  `PacketIdentifierNotFound`. Cover non-packet `AwaitAck` and native suppression
  boundaries without requiring events the native clients do not emit.
- [x] Verify existing default event sequences and terminal ACK results, opt-in
  filtering, every outgoing-selector combination above, absence of duplicate
  existing incoming events, and preservation of AUTH lifecycle/property accessors.
  Verify selected raw AUTH observations carry no response authority.
- [x] Verify retained views, generation before accepted CONNACK and across
  reconnect/replay, full queues and delivery-timeout termination, shutdown, and
  shared-worker progress. Check event-stream order and independent completion
  observation with queued diagnostics. Do not infer flush timing from outgoing
  events or assert a total order between independent observation channels.
- [x] Measure disabled and enabled allocation/retention costs without adding
  unbounded history. Cover property-heavy packets and slow consumers. Exercise
  property redaction, event ownership after close, and final-owner reclamation.
- [x] Update header/exports, C examples, READMEs, `PARITY.md`, and `CHANGELOG.md`;
  document the availability matrix, selection rules, scheduling boundary, and queue
  tradeoff. Run core/C feature-sensitive suites, ABI compatibility checks, and
  Python/JavaScript event/completion compatibility checks.

Complete when C can opt into native protocol observations with exact kinds and
available contents, while default event traffic, protocol ownership, and terminal
ACK classification remain unchanged. Enabled observations follow the documented
queue-pressure contract and do not promise a complete wire transcript.


## Implementation evidence

Wrapper-core adds default-disabled `ProtocolObservationSelection`, immutable
`ProtocolObservation` / `ObservationProperties`, and shared event-owned storage.
Both backend mappings preserve emitted incoming control packets and enrich the
existing outgoing event once. Native outgoing contents remain unavailable;
AwaitAck has no packet metadata. Raw AUTH is independently selected and preserves
existing lifecycle properties and callback authority. Accepted generation is
captured at mapping and is absent during pre-CONNACK handshakes, including retries.

C adds selection flags, event kind 12, a size-versioned metadata record, and
borrowed reason/property/filter/AUTH accessors. Existing constants, records,
completion classification, and public Python/JavaScript formats are preserved.
Language encoders filter the new internal variant. The
[availability and ownership contract](native-wrappers/protocol-observations.md)
and [C example](native-wrappers/c/examples/protocol_observations.c) document use.

Selected delivery services ready operation notices before and during queue waits
without polling MQTT again. It retains the existing overflow and shutdown
contract. A boxed auxiliary completion loop keeps combined-feature driver futures
bounded; disabled delivery does not allocate it. Ordered shutdown retains at most
one boxed staged event until teardown. Packet properties move into incoming
observations; existing native terminal notices keep their independent ownership,
without a completed-packet cache or cross-channel matching history.

Core unit/integration tests and portable C fixtures cover exact kinds, all selector
combinations, absent/unavailable/present-empty fields, duplicate property order,
filter codes, QoS 2 success/rejection/replay, suppressed duplicate PUBLISH,
unknown-PUBREL PUBCOMP contents remaining unavailable, PINGRESP, AUTH lifecycle
and rejection, generations, retained views/copies, final-owner release, redaction,
queue overflow, completion progress with queued diagnostics, immediate/ordered
shutdown, and a peer client sharing a single execution worker. A broker PUBLISH
fence checks default event sequences and absence of duplicate observation events.

Validation completed with:

- Root workspace check and wrapper workspace tests; core/C tests with the combined
  ordered-shutdown, proxy, SCRAM, tracing/log compatibility, TLS 1.2, native TLS,
  and system SRV features. Targeted observation tests also pass with ordered
  shutdown and tracing enabled.
- `cargo hack test --each-feature --exclude-all-features` and strict
  `cargo hack clippy --all-targets --each-feature --exclude-all-features` for core
  and C: all 35 configurations passed in each matrix. Strict workspace Clippy and
  the combined-feature core/C Clippy check passed.
- All 83 native CTest cases passed with combined optional features. The observation,
  AUTH observation/rejection, and example consumers also passed against the default
  C build. The existing invalid-AUTH consumer had a callback-count race reproduced
  against the pre-change build: closing could also cancel a new initial exchange.
  Its assertion now counts only the reauthentication exchange under test; five
  repeated default-build runs and a pre-change-library run passed.
- `native-wrappers/c/tests/abi/check.sh all`, 15 ABI helper tests, and structural
  ABI containment against the pre-change HEAD header/library passed. The optional
  error-output scanner covers all 236 APIs, including the new accessors.
- Rebuilt JavaScript/Python bindings: Node API tests, TypeScript checks, the Node
  broker behavior suite, Python unit/API tests (118 passed), and Python broker/
  lifecycle tests (69 passed). Standalone Python tests skip fixture-dependent
  cases, which run in the separate broker fixture.
- Formatting and `git diff --check` passed.

The release [allocation/retention harness and results](native-wrappers/wrapper-core/benches/protocol-observations.md)
compare disabled and enabled behavior against pre-change HEAD, including 8 KiB
MQTT 5 properties, bounded caller retention, slow consumers, and final-owner
release. No unbounded history or disabled-path property cloning was introduced.
Whole-process allocation totals vary with scheduling; these measurements do not
claim zero total overhead or a throughput guarantee.

Validation was performed on Linux x86_64. The HEAD comparison is a development
ABI containment check, not a published-release baseline. No historical release
comparison or execution on other platforms is claimed.
