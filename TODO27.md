# C Wrapper Advanced TLS Configuration

## First stage: owned, enforceable policy

The first stage implements owned profiles, version policy, combined roots,
built-in certificate/public-key pinning, and backend capability reporting.
It does not promise injected Rust-object parity.

- [x] Immutable owned C TLS profiles and additive setters preserve existing
  public record layouts and loader identity. Profiles copy caller inputs,
  validate without networking, and may be destroyed before client start.
- [x] Platform, supplied-only, and combined trust policies apply independently
  to broker TLS/WSS, HTTPS proxies, and explicitly configured MQTT 5 redirects.
- [x] Typed allowed-version policies are enforced where the selected backend
  supports them. Unsupported requests fail before networking. Rustls TLS 1.2-only
  support is opt-in through `tls12`; existing default features are preserved.
- [x] Rustls leaf-certificate and full-SPKI SHA-256 pins supplement standard
  chain, validity, hostname, and handshake-signature verification. Up to 32 pins
  provide alternative accepted identities for rotation; zero disables pinning.
- [x] Pinned profiles disable TLS resumption so every reconnect revalidates the
  presented certificate. Unpinned profiles retain backend defaults.
- [x] Per-backend capability masks distinguish enforceable policy from build
  features and negotiated availability. Native TLS rejects pinning, Apple
  native TLS rejects TLS 1.3-only, and older/unknown OpenSSL implementations
  reject explicit restrictions that they cannot reliably enforce.
- [x] Sanitized diagnostics and existing secret-copy zeroization are preserved.
  Configuration replacement and failure leave profile ownership explicit.

## Verification for the first stage

Native C consumers exercise version restrictions, combined trust, certificate
and SPKI pins, TLS/WSS, independent proxy policies, redirect profiles, input
ownership, reuse, transactional setter failures, and configuration replacement.
Rust fixtures additionally test wrong names/chains, expiry, invalid signatures,
backup pins, same-key certificate renewal, key rotation, and ticket-enabled
reconnects. Header/export, feature packaging and ABI checks cover the additions.
Platform execution evidence belongs in `native-wrappers/wrapper-core/PARITY.md`;
Linux results do not imply macOS or Windows execution.

## Deferred advanced extensions

The following require separate consumers, prototypes and acceptance criteria;
the full advanced TLS proposal remains incomplete.

- [ ] Audit remaining backend-specific cipher, SNI, identity-selection and
  resumption customization points without promising portable parity.
- [ ] Add enforceable cipher selection where justified by actual consumers.
- [ ] Prototype verification callbacks before defining their C records. Make
  supplementation versus replacement explicit and preserve handshake-signature
  and hostname validation unless equivalent verification is supplied.
- [ ] Prototype external identity selection/signing against actual backend
  traits. Define algorithms, signature bounds, owner retention and typed failure
  without exporting keys or treating foreign keys as Rust pointers.
- [ ] Respect synchronous backend hooks. Deferred signing needs a demonstrated
  sound execution adapter; callbacks cannot wait for their own driver progress.
- [ ] Demonstrate an external signer with native C consumers, including failure,
  reconnect, cancellation, ownership and secret-redaction coverage.

Arbitrary injected Rustls `ClientConfig` or native `TlsConnector` objects cannot
be represented by the C profile API. Fully external TLS streams can use custom
transport connectors, which are transport integration rather than native TLS
configuration parity.
