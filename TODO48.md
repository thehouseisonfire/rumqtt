# Detached C Manual-Acknowledgement Tokens

Status: planned.

## Goal

Let C retain acknowledgement authority independently of an incoming event so it
can release publication data before acknowledging deferred application work.

## Foundation and scope

Wrapper-core's `AckToken` is small and independent of `IncomingPublish` contents.
C's `acknowledge_impl()` consumes the token stored in the event and therefore
requires retaining the event and its payload. Existing ACK options and local
flush completions are already implemented and must be reused.

## Implementation requirements

- [ ] Add an opaque owned ACK-token handle with extraction and retain/release.
  Specify extraction as a transfer or shared-authority operation; choose one
  clear contract and preserve existing event-based acknowledgement semantics.
- [ ] All event/token copies for the same incoming flow must reference the same
  single-use authority. Do not create a second consumable acknowledgement by
  copying the core token or retaining a C handle. Preserve native retransmission
  deduplication and client/connection-generation validation.
- [ ] Add token-based immediate/tracked acknowledgement APIs, including the
  existing legal MQTT 5 reason/properties options. Reuse the same coordinator
  and admission gate as event-based calls; a packet identifier is not authority.
- [ ] Failed admission leaves authority retryable. Successful admission consumes
  all aliases, even while local flush completion is pending. Coordinate with
  [TODO41.md](TODO41.md) so cancellation before admission restores availability
  and cancellation after admission cannot submit a second ACK.
- [ ] Dropping a token never acknowledges automatically. Retained tokens keep
  only the small authority/context needed for validation, not topic, payload,
  native stream, client owner, or runtime. A stale token after reconnect/teardown
  must fail deterministically without accessing released state.
- [ ] Define concurrent extraction, event-based ACK, token-based ACK, and handle
  destruction rules. Serialize authority transitions without holding locks while
  invoking callbacks or waiting for native progress. Keep blocking restrictions.
- [ ] Use additive C APIs and stable opaque ownership. Preserve existing records,
  ACK options, error/delivery classification, and completion flush semantics.

## Verification and completion

- [ ] Extract authority from v4/v5 QoS 1/2 publications, destroy their events,
  and prove payload storage is released before successful acknowledgement.
- [ ] Race retained aliases, event/token callers, retransmissions, admission
  backpressure, reconnect, shutdown, and cancellation. Observe exactly one ACK
  and reject foreign-client, stale-generation, and already-consumed authority.
- [ ] Test missing authority on QoS 0/automatic ACK events, invalid handles,
  initialized outputs, retained tokens after client destruction, and owner release.
- [ ] Update the manual-ACK example, C header/exports, READMEs, `PARITY.md`, and
  `CHANGELOG.md`; run relevant core/C suites, native fixtures, and ABI checks.

Complete when C can safely discard a received event while preserving only its
deferred acknowledgement authority, with the same single-use guarantees as today.
