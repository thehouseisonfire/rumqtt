# Detached C Manual-Acknowledgement Tokens

Status: implemented and validated on Linux.

## Goal

Let C retain acknowledgement authority independently of an incoming event so it
can release publication data before acknowledging deferred application work.

## Foundation and scope

Wrapper-core's `AckToken` is small and independent of `IncomingPublish` contents.
Before this change, C's `acknowledge_impl()` consumed only the token stored in
the event, requiring retention of the event and its payload. Detached handles
now reuse the existing ACK options, coordinator, admission gate and local flush
completions.

Keep this a focused C ownership/API addition. Wrapper-core's existing coordinator
remains the single-use authority; no new authority registry or execution facility
is needed. Extraction is opt-in and must not add a per-event allocation to the
existing event-based path. This work can ship before and independently of
[TODO41.md](TODO41.md); deferred admission and its cancellation machinery belong
there.

## Implementation requirements

- [x] Add an opaque owned ACK-token handle with extraction and retain/release.
  Extraction transfers the token out of that event atomically with respect to
  event-based ACK calls and other extractions. Success leaves the event without
  an available token, so further extraction or event-based ACK fails; publication
  data remains readable for the event's lifetime. Failed extraction leaves the
  event unchanged. Preserve existing event-based behavior when no extraction
  occurs. Retain returns an independently owned reference to the same detached
  authority, not another acknowledgement opportunity.
- [x] All event/token copies for the same incoming flow must reference the same
  coordinator-enforced single-use authority. Copying the existing core token is
  safe because it identifies that authority; never manufacture independently
  consumable authority from a copy or packet identifier. Preserve retransmission
  deduplication and client/connection-generation validation, including aliases
  obtained from separate retransmission events.
- [x] Add token-based immediate/tracked acknowledgement APIs, including the
  existing legal MQTT 5 reason/properties options. Reuse the same coordinator
  and admission gate as event-based calls. Both forms retain immediate admission
  behavior; tracked completion observes local ACK flush, not broker receipt or
  completion of the entire QoS 2 exchange.
- [x] Failed admission leaves otherwise-valid authority retryable through its
  current owner, including after malformed options or backpressure. It does not
  transfer a detached token back into its source event. Successful admission
  consumes authority for every alias, even while local flush completion is
  pending or if flushing later fails. Dropping a completion cannot undo admission.
  When TODO41 adds deferred admission, cancellation/failure before admission must
  restore availability if the generation remains valid; cancellation after
  admission must not permit a second submission.
- [x] Dropping a token never acknowledges automatically. Retained tokens keep
  only the small authority/context needed for validation, not topic, payload,
  properties, native stream, client owner, or runtime. Retaining a token does not
  extend its connection generation or provide durable/offline ACK authority.
  Token retain/release remains safe after client destruction. ACK calls require
  a live client handle; stale or foreign authority fails through existing
  validation without dereferencing a destroyed owner. This does not make a
  destroyed client pointer valid to use.
- [x] Define concurrent extraction, event-based ACK, token-based ACK, and handle
  destruction rules. Retained handles may compete to ACK, but each owned handle
  must remain live throughout any call using it; destruction must not race use of
  that same handle. Serialize authority transitions without holding locks while
  invoking callbacks or waiting for native progress. Keep blocking restrictions
  and existing C live-handle requirements; do not promise detection of arbitrary
  dangling, forged, or wrong-type pointers.
- [x] Use additive C APIs and stable opaque ownership. Preserve existing records,
  ACK options, error/delivery classification, and completion flush semantics.

## Verification and completion

- [x] Extract authority from v4/v5 QoS 1/2 publications, destroy their events,
  and successfully ACK through detached handles. Prove the handles retain no
  publication storage, with a controlled lifetime test showing release before
  ACK when no other buffer owners remain; do not infer release from RSS or
  require shared backing storage to disappear while other owners retain it.
- [x] Test successful and failed extraction, repeated extraction, event-based
  ACK before/after extraction, readable publication data after extraction, and
  independently released retained handles. Dropping an unconsumed handle must
  send no ACK; dropping an admitted ACK's observers must not cancel it.
- [x] Race retained aliases, event/token callers, retransmissions, admission
  backpressure, reconnect, and shutdown. Prove at most one successful application
  ACK admission per authority, with one when a valid contender succeeds. Preserve
  native protocol-required retransmission responses rather than promising exactly
  one wire ACK. Reject foreign-client, stale-generation, and consumed authority;
  verify rejected options/backpressure allow retry without inheriting ACK content.
  Add cancellation arbitration coverage with TODO41 when that facility exists.
- [x] Test missing authority on QoS 0/automatic ACK events and wrong event kinds,
  NULL handles, initialized failure outputs, safe token retain/release after client
  destruction, rejection through a different live client, and final owner release.
  Prove retained tokens do not prevent client/execution teardown or retain callback
  owners. Test stale-token rejection after reconnect and shutdown with live handles.
- [x] Update the manual-ACK example, C header/exports, READMEs, `PARITY.md`, and
  `CHANGELOG.md`; run relevant core/C suites, native fixtures, and ABI checks.

Complete when C can safely discard a received event while preserving only its
deferred acknowledgement authority, with the same single-use guarantees as today.


## Implementation and validation evidence

The additive C API exposes `rumqttc_event_take_ack_token`,
`rumqttc_ack_token_retain`/`release`, and four immediate/tracked token ACK forms
matching the existing event-based APIs. Only explicitly extracted handles
allocate shared token slots; events keep their existing inline mutex. Core
remains the authority across independently extracted retransmission aliases.
The C header, READMEs and manual-ACK example document the ownership contract.

C `ack_token` tests use controlled in-memory transports and flush barriers for
both protocols and execution modes. Owner drop counters prove topic, payload
and property storage releases before ACK. Tests cover extraction and ACK races,
retained/retransmission aliases, backpressure, pending/failed flushes, observer
release, reconnect races, stale/foreign authority and teardown with live tokens.
Connector destruction and context joining prove tokens retain no callback or
execution owners. The native fixture verifies QoS 1/2 reason/properties and
concurrent retained aliases, including native PUBREC retransmission after
application authority has been consumed.

Validation on Linux:

- Full core/C crate suites passed using `cargo test --locked --manifest-path
  native-wrappers/Cargo.toml -p rumqttc-c-next -p rumqttc-wrapper-core-next`.
  The existing explicitly ignored cases remain ignored.
- Full C tests passed without default features and with
  `--features ordered-shutdown,tracing-log-compat`. All ten detached-token
  scenarios also passed without default features after the final race additions.
- All five affected native cases passed: `rumqttc-native-integration`,
  `rumqttc-native-acknowledgement-options`,
  `rumqttc-native-acknowledgement-results`, `rumqttc-native-ack-tokens`, and
  `rumqttc-example-manual_acknowledgement`. The token fixture also passed after
  adding the protocol retransmission assertion.
- `native-wrappers/c/tests/abi/check.sh all` passed current header/generated
  declarations, exports, C11/C++17 consumers, and relocated pkg-config checks.
  Optional error-output coverage passed for all 227 APIs.
- Strict C Clippy passed with `--all-targets --no-deps -- -D warnings
  -W clippy::pedantic -W clippy::nursery`, including the composed
  ordered-shutdown/tracing profile. Dependency-inclusive strict Clippy encounters
  an existing `clippy::option_if_let_else` warning in
  `wrapper-core/src/backend/mod.rs` (disconnect timeout selection); it was not
  changed by this task. Rust formatting and `git diff --check` passed.

macOS/Windows execution remains in the existing CI platform matrix. Deferred
admission and cancellation arbitration remain TODO41 work; this feature adds
no pending-operation facility.
