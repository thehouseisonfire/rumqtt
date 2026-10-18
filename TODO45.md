# Structured Native Connection Failure Details

Status: complete (Linux validation on 2026-10-17).

## Goal

Retain actionable native failure classifications and safe scalar context through
wrapper-core and C, without exposing opaque Rust errors or secret-bearing text.

## Foundation and scope

The native connection/state error enums contain I/O causes, protocol variants,
packet identifiers, and actual/maximum limits. Many paths in
`native-wrappers/wrapper-core/src/backend/v4.rs` and `v5.rs` reduce them to
`MQTT connection failed`. Dedicated broker, publish, store, authentication,
redirect, transport, and TLS callback details already exist and must remain intact.

Scope is wrapper-core and C. Python/JavaScript exposure is separate work. Prioritize
common actionable failures; the native-variant audit does not require a public code
for every dependency variant or changes to native errors to supply missing evidence.

## Implementation requirements

- [x] Audit native `ConnectionError`, `StateError`, `ProtocolViolation`, proxy,
  WebSocket, and enabled TLS causes. Record a mapping inventory by protocol and
  feature, including useful typed variants, available scalar fields, and deliberate
  fallback/absence cases. Match variants and source types; never classify by parsing
  Display/Debug text. Bound any nested-source inspection.
- [x] Define a small, stable, wrapper-owned failure taxonomy. Share equivalent
  classifications across v4/v5 without flattening meaningful differences. Do not
  expose Rust/dependency enum discriminants as public codes. Define precedence
  between the primary failure and nested cause, including TLS/proxy/WebSocket I/O
  and redirect failures. Existing dedicated failure details remain authoritative
  and may coexist with supplementary native cause details.
- [x] Add owned structured detail to core errors and additive C accessors for
  failure domain/code, optional I/O kind and OS code, packet kind/identifier,
  and actual/limit or expected values where native errors provide them. Define
  presence and semantics per code: byte lengths versus counts, full packet versus
  remaining length, and maximum versus expected equality. Do not invent missing
  packet kinds, identifiers, limits, or failure stages from current configuration.
- [x] Define explicit absence and fallback behavior for unsupported features,
  unavailable evidence, and unknown native variants. Preserve a known domain with
  a stable domain-specific unknown code when possible; do not promise a numeric
  identity for an unknown Rust variant. DNS/connect failures may expose only generic
  I/O evidence because the native dialer returns both as `io::Error`. V4 incoming
  `PayloadSizeLimitExceeded` supplies remaining length but no limit; preserve that
  distinction rather than synthesizing a full packet size or maximum.
- [x] Distinguish portable classifications from OS/backend-specific numbers.
  Map I/O kinds explicitly into wrapper-owned codes. A raw OS code has a
  platform/domain and presence; do not invent one from an `ErrorKind` or imply
  identical errno/Windows meanings. Do not expose arbitrary third-party error
  objects, pointers, or source-owned strings.
- [x] Preserve legacy status, kind, delivery ambiguity, retry eligibility,
  operation/generation context, and dedicated failure accessors. This is an
  observation extension, not permission to retry mandatory terminal failures.
  Keep existing terminal-result and broker-rejection semantics.
- [x] Carry sanitized details through disconnected and terminal events, operation
  errors, retained reconnect last-failure summaries, and exhaustion errors where
  the source evidence exists. Preserve the failure's original provenance rather
  than attributing it to a later configuration revision or connection. Keep
  exhaustion distinct from its last native cause; do not replace completed
  operation results or attach later connection failures to unrelated results.
- [x] Keep detail storage bounded and error cloning cheap. Prefer optional shared
  immutable detail storage over expanding every inline `Error`; allocate no detail
  record when absent. Check `Error` and terminal-result sizes against existing
  layout constraints. Strip source owners from diagnostic snapshots while retaining
  safe scalar details, and include those details in snapshot equality where relevant.
  Exclude packet bodies, credentials, paths, host-supplied text, AUTH data, TLS
  keys, and broker properties from automatic rendering. Retain no opaque source
  owners or free-form source text in the new detail record.
- [x] Follow the C ABI policy with new accessors/versioned records; initialize
  failed outputs and retain details independently of client/source lifetime.

## Verification and completion

- [x] Exercise connection refusal, DNS/I/O failure, missed PING response,
  unsolicited ACK, malformed packet, oversized input/output, and enabled TLS,
  proxy, and WebSocket failures. Use injected typed errors for hard-to-produce
  variants and deterministic native C fixtures for representative real failures.
  Cover nested TLS/WebSocket causes surfaced as I/O and redirect target failures,
  checking precedence and preservation of dedicated failure details.
- [x] Assert exact scalar detail, units, independent field presence, and documented
  unsupported/unavailable/unknown behavior. Cover known wrapper I/O kinds with no
  raw OS code, genuine OS codes with their domain, and partial size evidence.
- [x] Assert unchanged legacy and classified retry decisions, mandatory terminal
  behavior, delivery classification, and broker/operation terminal outcomes. Prove
  details survive error cloning, diagnostic sanitization, event/completion C
  conversion, reconnect retention, and exhaustion after client destruction. Include
  a later configuration revision/connection to detect provenance reassignment.
  Check snapshot equality, error/result size constraints, and zeroed failure outputs.
- [x] Place secrets in nested source errors and prove they and their owners do
  not survive in sanitized summaries or leak through automatic output.
- [x] Update `PARITY.md`, C header/exports, error documentation, examples, and
  `CHANGELOG.md`; run feature-sensitive mapping tests and C/ABI checks.

Complete when C can distinguish common native transport/protocol failure causes
and inspect their safe scalar evidence without depending on formatted messages.

## Implementation and validation evidence

- Core `Error::native_failure()` and C `rumqttc_error_native_failure_details()`
  expose the owned scalar record. See the
  [mapping and ownership inventory](native-wrappers/native-failures.md).
- Wrapper workspace tests passed, including retained provenance across a credential
  revision/reconnection and scalar evidence retained after client destruction.
- All 19 configurations in the wrapper-core `cargo hack test --each-feature
  --exclude-all-features` matrix passed. Combined rustls Ring/native TLS/WebSocket/
  proxy mapping tests and strict pedantic/nursery Clippy passed.
- The native C suite passed: 84 executed tests, with two feature-disabled examples
  skipped. All 10 packaged C feature profiles passed with native consumers,
  including the new deterministic `native-failure-details` fixture.
- C ABI/header/export/package checks, optional error-output coverage, Rust formatting
  and whitespace checks passed. Linux x86_64 layout comparison against the original
  definitions confirmed unchanged `Error`/`TerminalOutcome` sizes (120/192 bytes).
- macOS/Windows execution remains with the existing CI matrix; only Linux execution
  is claimed here.
