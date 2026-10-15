# C Application-Owned TLS Layer Connectors

Status: planned; requires a composition prototype before publishing the new ABI.

## Goal

Allow an application TLS implementation to wrap a library-established byte
stream while preserving native proxy, MQTT, and WebSocket composition.

## Foundation and scope

`rumqttc_core::TlsHandshakeConnector` accepts an owned stream, server name,
port, and deadline and asynchronously returns an authenticated stream. Rust
`TlsConfiguration::Connector` exposes it; wrapper-core/C currently expose only
backend-built TLS profiles and specific verification/identity hooks.

The existing Established custom transport is a valid alternative when the host
owns every transport layer. This task fills the narrower TLS-layer extension
point. Do not pass Rustls/native-TLS object pointers through C or install a
process-global crypto provider. Keep ordinary profile verification unchanged.

## Implementation requirements

- [ ] Prototype a C TLS engine using memory BIOs or an equivalent portable byte
  interface over the native connector boundary. Demonstrate asynchronous lower
  stream I/O and a returned plaintext stream without socket extraction, nested
  runtime blocking, or native poll cancellation before finalizing public types.
- [ ] Add an explicit external-connector policy alongside existing backend-built
  profiles. A connector is the TLS authentication authority for its selected
  layer; do not silently combine it with pins/verifier/identity settings that it
  cannot enforce. Reject ambiguous combinations transactionally.
- [ ] Expose retained registrations, owned handshake requests, cancellation,
  and opaque lower-stream ownership. Provide bounded asynchronous read/write,
  flush, and shutdown primitives with owned buffers and explicit completion.
  Reuse existing transport contracts where sound; never lend Rust stack buffers
  across callbacks or expose a native socket/DynAsyncReadWrite pointer.
- [ ] Require exclusive ownership of a lower stream for one handshake/returned
  stream chain. Permit one read and one serialized writer family; define EOF,
  short transfers, flush, half-close, and how discarding a chain retires pending
  work. Bound retained operations/bytes, including cancelled host-retained work.
- [ ] Preserve the original absolute attempt deadline and check cancellation
  before accepting success. Reject stale/duplicate responses and previously used
  streams. Handshake callbacks must yield promptly; timeouts cannot preempt host
  code. Join/unload observation must account for active calls and retained owners.
- [ ] Supply correct TLS authority, port, layer, and attempt identity for broker
  TLS, WSS, HTTPS proxies, redirects, and SRV-selected targets. The host must
  complete certificate/hostname/signature authentication before reporting success.
  Document ALPN and resumption responsibility explicitly.
- [ ] Keep failures typed and redacted; distinguish verification/policy failures
  from eligible transport failures so reconnect/SRV fallback cannot bypass failed
  authentication. Integrate source details with [TODO45.md](TODO45.md).
- [ ] Define build support against the native connector's current TLS feature
  gates, disabled-feature behavior, and separate C capability bits. A host TLS
  implementation must not be advertised as backend-free unless that build works.
- [ ] Keep setters additive and retained-profile replacement coherent with
  [TODO35.md](TODO35.md). No callback or destructor may run under wrapper locks;
  registration release alone does not prove host TLS work has quiesced.

## Verification and completion

- [ ] Run real native C handshakes for v4/v5 TLS/WSS, independent HTTPS-proxy
  layers, custom base streams, redirects, and reconnect with changed identities.
- [ ] Cover wrong hostname/chain, invalid signature, host rejection, partial I/O,
  deadline expiry, cancellation during each stage, late/duplicate results,
  retained-operation limits, and exactly-once cleanup after client destruction.
- [ ] Prove peer progress on shared execution, bounded buffering, and safe unload
  after joining host work. Keep existing declarative TLS/profile tests passing.
- [ ] Update core/C feature manifests, header/exports, a portable host TLS example,
  READMEs, `PARITY.md`, and `CHANGELOG.md`; run composition, package, and ABI checks.

Complete when C can implement the native TLS connector boundary through a safe
owned byte-stream contract without taking over MQTT or the remaining native layers.
