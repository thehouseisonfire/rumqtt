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

## Advanced extensions

- [x] Backend audit and capability reporting distinguish Rustls cipher selection,
  supplemental verification, external identities and resumption restrictions
  from portable SNI policy. Unsupported native policies fail before networking.
- [x] Ordered, privately cloned Rustls cipher allowlists reject unavailable,
  duplicate and version-incompatible suites; provider algorithm queries expose
  the supported IANA cipher and external signature scheme IDs.
- [x] Verification adapters were proved before defining their C records. They
  supplement standard chain, validity, hostname, signature and pin enforcement.
  Typed failures preserve stage/reason/layer without host diagnostic text.
- [x] External identity selection/signing uses copied immutable certificate/key-ID/
  scheme catalogs and owned synchronous registrations. Every signature is
  verified against the selected leaf before use; no foreign cryptographic object
  is imported. Resource bounds and successful-construction ownership are explicit.
- [x] Synchronous hooks retain owners across handshakes and clients, check the
  original connection deadline before/after host work, and cannot use deferred
  completion or wait for their own driver. Pins and hooks disable resumption.
  Fresh handshake state prevents failed selection becoming anonymous TLS and
  terminal callback failures stop SRV fallback.
- [x] Native C/OpenSSL EVP consumers demonstrate RSA-PSS/ECDSA mutual TLS/WSS,
  independent proxies and redirects, typed failures, reconnects, nonblocking
  reentrant admission, cancellation during an active callback, profile sharing
  and exactly-once destruction. An optional runnable signer example and a
  required-dependency CI job keep OpenSSL out of Rustls production linkage.

## Advanced verification

Rust fixtures additionally prove optional client-auth failure guarding, bad
external signatures, verifier panics/vetoes, pins before supplemental policy,
SNI/cipher negotiation, default/disabled reconnect resumption, deadlines and
terminal SRV fallback. C registration tests prove failed-construction ownership
and retained profiles. Feature, installed-package, generated-header/export,
ABI containment and Rust 1.88 checks cover the additions. Platform execution
is recorded separately in `native-wrappers/wrapper-core/PARITY.md`.

Arbitrary injected Rustls `ClientConfig` or native `TlsConnector` objects cannot
be represented by the C profile API. Fully external TLS streams can use custom
transport connectors, which are transport integration rather than native TLS
configuration parity.
