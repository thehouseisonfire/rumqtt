# C Application-Owned TLS Layer Connectors

Status: parked after corrected Linux measurements and bounded profiling.
There is no confirmed application consumer, and the agreed performance gate
is unmet. Public C ABI implementation and further bridge redesign are deferred.

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

Initial external-connector selection is construction-only, including explicit
proxy and redirect profiles. Live replacement of a TLS connector or a transition
between backend-built and external TLS authorities is deferred to a separate
applicability, ownership, and session-cache audit. Existing supported backend-built
profile updates remain available under [TODO35.md](TODO35.md).

## Prototype and delivery gate

The selected reference consumer owns its OpenSSL context and TLS policy while
retaining native HTTPS-proxy and WebSocket composition. This is the agreed
reference workload, not a claim about an existing production deployment.
The user confirmed that no real application is currently blocked on this feature.
Acceptance requires every measured configuration to reach at least 75% of
equivalent native TLS throughput and no more than 1.5 times its p99 latency.
Retain the current native TLS feature gates; backend-free builds remain deferred.
The private prototype uses limits of 64 chains, 256 live reservations, 8 MiB of
buffer reservations, and 16 KiB transfers. Metadata/host-allocation accounting
boundaries are described in the evidence below; these are not public ABI types.

- [ ] Confirm a real application requirement for an application-owned TLS engine
  that existing profile, verifier, external-signing, and Established-transport
  APIs do not adequately meet. A reference workload alone does not satisfy this gate.
- [x] Define a reference workload and resource/overhead acceptance budgets before
  treating prototype measurements as evidence for a public ABI.
- [x] Prototype a C TLS engine using memory BIOs or an equivalent portable byte
  interface over the native connector boundary. Prove both host-initiated I/O on
  a Rust-owned lower stream and native-initiated I/O on the returned host-owned
  plaintext stream. Demonstrate handshake and steady-state encrypted traffic
  without socket extraction, nested runtime blocking, or native poll cancellation.
- [x] Exercise external HTTPS-proxy TLS underneath external broker TLS, including
  WSS and a custom base stream. Demonstrate reads that require lower-stream writes
  (for example TLS control traffic), partial transfers, concurrent read/write
  progress, flush, and TLS shutdown. Define TLS-engine serialization so a waiting
  read cannot prevent writes or cancellation from advancing either TLS layer.
- [ ] Measure copies, allocations, task/worker counts, throughput/latency, and peak
  wrapper-retained bytes/operations against an equivalent native TLS composition.
  Include slow peers, shared execution, repeated reconnect, and cancelled work
  retained by the host. Report host TLS buffering separately; wrapper limits cannot
  bound arbitrary host allocations. Confirm acceptance budgets or revise the
  design/scope before finalizing public types. Retain the evidence with the plan.

## Implementation requirements

These requirements remain a design record. They are not the next implementation
phase while this task is parked.

- [ ] Add an explicit external-connector policy alongside existing backend-built
  profiles. A connector is the TLS authentication authority for its selected
  layer; do not silently combine it with pins/verifier/identity settings that it
  cannot enforce. Reject ambiguous combinations transactionally.
- [ ] Expose retained registrations, owned handshake requests, cancellation,
  and opaque lower-stream ownership. Provide bounded asynchronous read/write,
  flush, and shutdown primitives with owned buffers and explicit completion.
  Reuse existing transport contracts where sound; never lend Rust stack buffers
  across callbacks or expose a native socket/DynAsyncReadWrite pointer.
  The existing adapter calls host-owned I/O; the reverse bridge must keep the
  Rust-owned lower stream polled while handshake or plaintext work awaits it.
- [ ] Require exclusive ownership of a lower stream for one handshake/returned
  stream chain. Permit one read and one serialized writer family; define EOF,
  short transfers, flush, half-close, and how discarding a chain retires pending
  work. Bound retained operations/bytes, including cancelled host-retained work.
  Distinguish authenticated TLS close from lower-stream EOF/truncation; define
  close-notify and pending-read cancellation behavior without waiting indefinitely
  for a peer response. Discarding either nested layer must retire the whole chain.
- [ ] Preserve the original absolute attempt deadline and check cancellation
  before accepting success. Reject stale/duplicate responses and previously used
  streams. Handshake callbacks must yield promptly; timeouts cannot preempt host
  code. Join/unload observation must account for active calls and retained owners.
  The attempt deadline covers connection establishment; it must not become a
  permanent expiry for established-stream I/O.
- [ ] Supply correct TLS authority, port, layer, and attempt identity for broker
  TLS, WSS, HTTPS proxies, redirects, and SRV-selected targets. The host must
  complete certificate/hostname/signature authentication before reporting success.
  Document ALPN and resumption responsibility explicitly.
- [ ] Keep failures typed and redacted; distinguish verification/policy failures
  from eligible transport failures so reconnect/SRV fallback cannot bypass failed
  authentication. Integrate source details with [TODO45.md](TODO45.md).
  Audit buffered/deferred I/O and nested-layer error propagation: the existing
  `transport::SavedFailure` retains only I/O kind and `TransportFailure`, so reuse
  must explicitly preserve new TLS classifications, layer, and attempt provenance.
  Authentication/policy failures must carry native terminal-failure semantics
  before SRV candidate selection, not only when converted into wrapper errors.
- [ ] Define build support against the native connector's current TLS feature
  gates, disabled-feature behavior, and separate C capability bits. A host TLS
  implementation must not be advertised as backend-free unless that build works.
- [ ] Keep setters additive and retained-profile replacement coherent with
  [TODO35.md](TODO35.md). Reject unsupported live authority changes transactionally,
  including mixed updates, without altering desired/effective revisions. An active
  chain retains its selected registration and policy until all work releases them.
  No callback or destructor may run under wrapper locks; registration release
  alone does not prove host TLS work has quiesced.

## Verification and completion

- [ ] Run real native C handshakes and steady-state traffic for v4/v5 TLS/WSS,
  independent HTTPS-proxy layers, nested external TLS, custom base streams,
  redirects, and reconnect with changed identities. Verify host-owned ALPN and
  resumption policy, including identity/trust isolation across reconnect/targets.
- [ ] Cover wrong hostname/chain, invalid signature, host rejection, partial I/O,
  deadline expiry, cancellation during each stage, late/duplicate results,
  retained-operation limits, and exactly-once cleanup after client destruction.
  Include post-handshake TLS control traffic, close-notify/truncation, shutdown
  with a pending read, nested-chain cancellation, and expired-attempt deadlines
  after successful connection establishment.
- [ ] Inject authentication/policy and eligible transport failures through each
  buffered/deferred and nested I/O path. Assert original classification/provenance
  survives and authentication failures stop reconnect/SRV candidate fallback.
  Verify rejected live authority changes leave supported profile updates intact.
- [ ] Prove peer progress on shared execution, bounded buffering, and safe unload
  after joining host work. Keep existing declarative TLS/profile tests passing.
- [ ] Update core/C feature manifests, header/exports, a portable host TLS example,
  READMEs, `PARITY.md`, and `CHANGELOG.md`; run composition, package, and ABI checks.

Complete when C can implement the native TLS connector boundary through a safe
owned byte-stream contract without taking over MQTT or the remaining native layers,
and the recorded consumer workload meets the prototype's resource/overhead budgets.

## Prototype delivery record

The private `tls-connector-proof` development feature compiles a C/OpenSSL
memory-BIO engine and Rust host-reactor glue. It exercises the native connector
boundary, independently polled owned lower I/O, and returned plaintext streams.
It exposes no new C ABI or external-TLS capability bit. See the
[method, coverage and retained evidence](native-wrappers/wrapper-core/benches/tls-connector.md).

The corrected baseline includes the buffered-stream flush fix and retains
seven interleaved repetitions of 10,000 tracked QoS 1 publications for every
configuration. Threaded scheduling passes 0/16 workload gates; native-runtime
scheduling passes 8/16. The gate remains unchanged at 75% native throughput and
at most 1.5 times native p99 latency in every configuration. Earlier measurements
are retained as historical evidence, not substituted for the corrected baseline.

Separate CPU profiles cover single/nested TLS with 1 KiB messages and nested TLS
with 16 KiB messages. In the selected nested-TLS/1 KiB case, process CPU time per
publication is approximately 12.5 us native, 73.4 us threaded, and 22.1 us inline.
Threaded work incurs about 15.6 voluntary context switches per publication;
inline work still performs roughly 120 Rust allocations versus 16 native.
These whole-process observations include the fixture/client and do not isolate
each cost. They support investigating operation/actor overhead if a real need
emerges; they do not establish a small optimization that meets every gate.

No bridge algorithm or public API was changed during this investigation. The
only added Rust entry point is an ignored single-workload profiling test.
Buffered-stream, control-write serialization, deadline, cancellation, and owned
operation correctness coverage remains in place. Observed reservation peaks do
not establish a total heap/RSS bound or include host OpenSSL allocations.

The measurement requirement remains unchecked because its full delivery gate
has not passed. Public profiles/registrations, typed failure/provenance,
runtime-update rejection, C completion ownership, redirect/SRV integration and
public join/unload/package/ABI validation remain unimplemented.

Reopen only with a confirmed application need that the existing APIs cannot
adequately serve, representative acceptance workloads, and evidence supporting
a bounded improvement or a justified redesign. Do not lower the agreed gate to
promote this prototype, or publish an ABI in anticipation of later optimization.
