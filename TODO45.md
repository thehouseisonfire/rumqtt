# Structured Native Connection Failure Details

Status: planned.

## Goal

Retain actionable native failure classifications and safe scalar context through
wrapper-core and C, without exposing opaque Rust errors or secret-bearing text.

## Foundation and scope

The native connection/state error enums contain I/O causes, protocol variants,
packet identifiers, and actual/maximum limits. Many paths in
`native-wrappers/wrapper-core/src/backend/v4.rs` and `v5.rs` reduce them to
`MQTT connection failed`. Dedicated broker, publish, store, authentication,
redirect, transport, and TLS callback details already exist and must remain intact.

## Implementation requirements

- [ ] Audit native `ConnectionError`, `StateError`, `ProtocolViolation`, proxy,
  WebSocket, and enabled TLS causes. Record an explicit mapping inventory of
  useful typed variants and scalar fields, including unsupported/unknown cases.
  Match variants and source types; never classify by parsing Display/Debug text.
- [ ] Add owned structured detail to core errors and additive C accessors for
  failure domain/code, optional I/O kind and OS code, packet kind/identifier,
  and actual/limit values where native errors provide them. Define units and
  optionality; preserve unknown future values through stable fallback codes.
- [ ] Distinguish portable classifications from OS/backend-specific numbers.
  A raw OS code has a platform/domain and presence; do not invent one from an
  `ErrorKind` or imply identical errno/Windows meanings. Do not expose arbitrary
  third-party error objects, pointers, or source-owned strings.
- [ ] Preserve legacy status, kind, delivery ambiguity, retry eligibility,
  operation/generation context, and dedicated failure accessors. This is an
  observation extension, not permission to retry mandatory terminal failures.
  Keep existing terminal-result and broker-rejection semantics.
- [ ] Carry sanitized details through disconnected and terminal events, operation
  errors, retained reconnect last-failure summaries, and exhaustion errors where
  the source evidence exists. Preserve the failure's original provenance rather
  than attributing it to a later configuration revision or connection.
- [ ] Bound detail storage and strip source owners from diagnostic snapshots.
  Exclude packet bodies, credentials, paths, host-supplied text, AUTH data, TLS
  keys, and broker properties from automatic rendering. Record explicit absence
  when a third-party cause cannot be safely classified.
- [ ] Follow the C ABI policy with new accessors/versioned records; initialize
  failed outputs and retain details independently of client/source lifetime.

## Verification and completion

- [ ] Exercise connection refusal, DNS/I/O failure, missed PING response,
  unsolicited ACK, malformed packet, oversized input/output, and enabled TLS,
  proxy, and WebSocket failures. Use injected typed errors for hard-to-produce
  variants and deterministic native C fixtures for representative real failures.
- [ ] Assert exact scalar detail, absence/unknown behavior, unchanged retry and
  delivery classification, retained lifetime, and zeroed failure outputs.
- [ ] Place secrets in nested source errors and prove they and their owners do
  not survive in sanitized summaries or leak through automatic output.
- [ ] Update `PARITY.md`, C header/exports, error documentation, examples, and
  `CHANGELOG.md`; run feature-sensitive mapping tests and C/ABI checks.

Complete when C can distinguish common native transport/protocol failure causes
and inspect their safe scalar evidence without depending on formatted messages.
