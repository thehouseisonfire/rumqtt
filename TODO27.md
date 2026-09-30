# C Wrapper Advanced TLS Configuration

## Goal

Expose useful advanced TLS policy and extension points through typed C APIs,
including protocol/cipher policy, certificate verification or pinning, and
external client-key signing where the selected backend can support them.

## Current foundation and feasibility

`rumqttc-core/src/lib.rs` accepts injected Rustls `ClientConfig` and native
`TlsConnector` values. Wrapper-core's `TlsConfig` currently contains backend,
root policy, identity, and ALPN only; `backend/mod.rs` constructs the objects.

Typed options and selected callbacks are architecturally feasible. Arbitrary
Rust objects cannot be injected through C, and platform TLS implementations do
not necessarily support the same extensions. Implement a documented capability
set, not an unsafe pointer cast or a promise to represent every Rustls field.

## Implementation requirements

- [ ] Audit customization points actually supported by each enabled backend.
  Record TLS-version, cipher-selection, SNI, root composition, verification,
  client-identity selection, key-signing, and session-resumption capabilities.
- [ ] Introduce immutable owned TLS profile handles and additive option records
  or setters. Preserve existing TLS/WSS setters and the checked public layouts.
- [ ] Offer explicit platform roots, supplied roots, and supported combined
  trust policies. Validate certificates and all options before networking when
  possible; unsupported combinations must fail rather than be ignored.
- [ ] Provide typed version/cipher selectors only when the backend can enforce
  them. Report per-backend capabilities separately from build-time provider
  selection; do not silently weaken a requested policy.
- [ ] Define a verification extension that receives bounded certificate-chain
  and server-name views with an explicit trust decision. State whether it
  supplements standard validation or replaces it. Preserve handshake-signature
  verification and hostname checks unless the configured contract explicitly
  supplies equivalent verification.
- [ ] Prototype external identity selection and signing against the actual
  backend traits before freezing their C records. Expose algorithm selection,
  signature bounds, owner retention, and typed failures. Never require foreign
  keys to be represented as Rust pointers or exported as PEM.
- [ ] Respect synchronous backend hooks. Require bounded, prompt callbacks;
  support deferred signing only if the backend and execution adapter can do so
  soundly. Do not block a shared driver worker waiting on its own progress.
- [ ] Keep key material, callback inputs, and verification data out of ordinary
  logs. Define owner release through handshake failure, reconnect, cancellation,
  and client/configuration destruction.
- [ ] Apply profiles consistently to broker TLS, WSS, HTTPS proxies, and
  explicitly approved redirect targets, without merging their trust policies.
- [ ] Keep every unrepresentable backend customization listed as unsupported.
  A fully external TLS stream may use the C wrapper custom transport
  connectors; it must not be described as native backend configuration parity.

## Verification and completion

- [ ] Demonstrate enforced TLS version policy, certificate pinning, and one
  external signer on a backend that supports it, with native C consumers.
- [ ] Cover wrong names, chains, pins, signatures, unsupported algorithms,
  callback failure, reconnect, owner release, and secret redaction.
- [ ] Update capabilities, feature packaging, C header/exports, README,
  `PARITY.md`, and root `CHANGELOG.md` under the existing ABI policy.

Complete when the supported customization set has enforceable semantics and
backend-specific limits are explicit. Do not mark full injected-object parity
on the strength of a few new TLS switches.
