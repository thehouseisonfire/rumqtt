# C Wrapper Application-Controlled MQTT 5 Redirects

## Goal

Let applications approve individual advertised redirect targets and configure
their transport, client identity, CONNECT credentials, and explicitly permitted
reuse of authentication authority or network credentials. Add session reuse only
after its persistence and reconciliation requirements are proved. Retain finite
attempts, loop detection, and typed, credential-free redirect observations.

## Native foundation and scope

`rumqttc-v5/src/redirect.rs` provides a synchronous policy callback,
`RedirectContext`, and `RedirectTargetProfile` with explicit reuse decisions.
The wrapper retains fixed reject/follow settings and maps application decisions
to explicit profiles in `native-wrappers/wrapper-core/src/backend/redirect.rs`.

The synchronous decision callback is implemented in both wrapper-core and C. Deferred decisions are
feasible only after the native client exposes an asynchronous policy boundary;
do not synchronously wait on a foreign completion from the driver thread.

The existing native profile supports supplied CONNECT username/password
credentials and reuse of the current enhanced-authentication authority. It does
not support installing a replacement target-specific enhanced-authentication
authority. Keep that extension outside the initial contract.

Native timeouts currently cover SRV lookup and individual connection attempts
separately. Each connection attempt establishes a new absolute deadline for its
network/MQTT handshake. There is no existing deadline covering the entire
redirect chain to preserve or expose as a guarantee.

The wrapper session-store adapter now checks native keys against its exact
leased scope/client-ID. A per-driver factory prepares target-key leases before
applying decisions; native options retain the origin lease for restoration.
Weak cache entries release obsolete leases rather than retaining a redirect history.

## Stage 1: synchronous target approval and isolated sessions

- [x] Add an owned redirect authority to wrapper-core and a retained C
  registration. Keep the existing fixed reject/follow setters as convenience
  policies with unchanged defaults. Define setter replacement/precedence and
  transactional failure behavior; exactly one policy decides each redirect.
- [x] Present packet source, reason, one-based attempt number, and all validated
  advertised references in native order. Provide typed accessors for reference
  kind, scheme, host, explicit port, WebSocket resource, and SRV owner where
  applicable. Define borrowed-view lifetimes and explicit retained/copied views;
  never expose native Rust references or origin credentials.
- [x] Let the caller reject or select one advertised reference and build the
  native target profile. Bind selection to the current request, copy response
  inputs, and bound reference/response storage. Validate selection, client ID,
  credentials, transport, and enabled features before state changes; preserve
  native rejection of unadvertised or incompatible targets.
- [x] Expose fresh/reused/replaced client-ID policy and supplied target CONNECT
  credentials. Reusing a client ID alone does not approve session reuse. The isolated
  Stage 1 profile remains the default; explicit reuse is enabled only through
  the completed Stage 2 contract.
- [x] Expose independent approval of existing authentication-authority reuse
  and native network-credential reuse. Document exactly which CONNECT method/
  data, proxy configuration, and WebSocket modifiers each native decision
  retains. Do not imply separately selectable proxy/header reuse or replacement
  enhanced-authentication support where native profiles cannot express it.
- [x] When an authentication authority is reused with a changed client ID,
  ensure wrapper auth contexts report the effective identity rather than the
  adapter's cached initial identity. Reject unsupported combinations until
  both synchronous and asynchronous authorities satisfy that contract.
- [x] Make origin credential/store/proxy/header isolation the default. Every
  reuse decision must be explicit and map to an actual native profile field;
  do not infer trust from a hostname suffix or a redirect reason.
- [x] Select TLS policy per target using existing profiles and
  [TODO27.md](TODO27.md) facilities. Retain profile owners through use, including
  TLS hooks. Keep SRV resolution, weighted selection, candidate fallback, and
  endpoint validation in the native client; approval selects an advertised SRV
  reference, not an application-chosen replacement endpoint.
- [x] Run callbacks without wrapper lifecycle/admission locks. Require prompt,
  thread-safe callbacks and nonblocking destructors; permit documented
  nonblocking reentrant admission and prohibit waiting for driver work or
  joining/destroying the active client from its callback. Contain Rust panics and
  malformed responses as typed, redacted failures; document foreign callbacks'
  no-unwind contract and exactly-once owner release after the last reference.
- [x] Define a finite synchronous decision budget and reject late results before
  applying them. Check cancellation after callback return. State explicitly
  that elapsed-time checks cannot preempt a blocking foreign callback, so they
  do not bound driver stalls or shutdown latency.
- [x] Preserve native loop detection, candidate exhaustion, attempt bounds,
  and existing per-lookup/per-attempt timeouts. Callback work must not restart
  an already active deadline. A total redirect-chain deadline requires a
  separate native-client change and verification before it can be promised.
- [x] Add typed decision/error observations distinguishing application rejection,
  invalid selection/profile, callback failure/late result, loops, attempt
  exhaustion, DNS failure, and target connection failure. Preserve source,
  reason, selected advertised reference, and available candidate diagnostics.
  Extend native error support where needed rather than misclassifying callback
  failures as DNS/transport failures. Omit credentials from Debug/errors/logs.

## Stage 2: explicit session reuse and persistence ownership

- [x] Define session reuse separately from client-ID, authentication, and network
  reuse. Document the native distinction: reusing the current client ID and
  store scope can preserve live state; changing either key resets that state
  and may load the target key's checkpoint. This is not automatic checkpoint
  migration or proof that two brokers share an MQTT session.
- [x] Make wrapper store load/save/clear honor the effective native key. Define
  and implement transactional lease acquisition/conflict handling for target
  keys, retention of origin leases needed for restoration, bounded lease
  storage, and release of obsolete leases on rollback or permanent move, with
  final release on teardown. No target
  key may be accessed without its lease, and no changed key may silently route
  to the original checkpoint.
- [x] Audit native and wrapper behavior for origin checkpoints, packet-ID
  ownership, pending operation results, manual-ACK token generations, and
  topic-alias generations. Preserve native CONNACK reconciliation and strict
  session-resume rules; application approval cannot manufacture Session Present
  or bypass local-state validation.
- [x] Verify temporary redirect restoration and permanent move establishment,
  including nested redirects, failed target connections, checkpoint failure,
  and cancellation during store work. Keep session reuse unavailable until
  these guarantees pass native Rust and C integration tests.

## Separate extension: deferred decisions

- [ ] Add asynchronous redirect policy support to the native event loop first,
  using owned requests/futures, defined decision deadlines, cancellation-safe
  state transitions, and rejection of stale/late results. Only then expose a
  retained C completion API. Do not emulate deferral by synchronously waiting
  on a foreign completion. Deferred decisions and a total redirect-chain
  deadline are separate capabilities, neither required for Stage 1 completion.

## Verification and completion

- [x] Exercise reference selection/rejection, target-specific credentials,
  client-ID choices, retained TLS profiles, explicit authority/network reuse,
  and default isolation in native C.
- [x] Cover CONNACK/DISCONNECT redirects, SRV targets, unsupported transports,
  unadvertised/stale choices, loops, attempt exhaustion, callback late results,
  nonblocking reentrancy, cancellation during callbacks, registration sharing,
  setter replacement/failure, and exactly-once owner teardown.
- [x] Verify origin credentials, store access, proxy configuration, and header
  modifiers are isolated unless their corresponding reuse is approved. Test
  authentication and network reuse independently; Stage 1 must never replay
  origin session state to the target.
- [x] For Stage 2, test unchanged/changed scope and client ID, active target-key
  lease conflicts, exact checkpoint keys, broker resume/refusal, tracked QoS 1/2
  replay/results, manual ACKs, aliases, and origin restoration. Verify obsolete
  target leases are released on rollback and all leases on final owner teardown.
- [x] Update C header/exports, ABI checks, examples, README,
  `native-wrappers/wrapper-core/PARITY.md`, and root `CHANGELOG.md`. Record stage
  coverage and synchronous callback/deadline limits accurately; distinguish
  supported CONNECT credentials, authority reuse, and deferred decisions.

Stage 1 is complete when C can approve an advertised reference and configure
its supported isolated target profile with retained ownership and typed results.
Stage 2 is complete only after scoped reuse, store leases, and session recovery
are proved. In both stages, redirect validation and MQTT session reconciliation
remain native-client responsibilities.


## Implementation and validation (Linux)

Both agreed stages are implemented. Deferred decisions, a chain-wide deadline
and replacement enhanced-authentication authorities remain excluded.

- Client each-feature tests: 36/36 configurations passed.
- Wrapper-core and C each-feature tests: 33/33 configurations passed; complete
  native-wrapper workspace suites also passed with default and combined TLS,
  proxy, authentication and resolver features.
- Native C fixtures/examples: 60/60 passed. The application authority fixture
  additionally passes explicit auth/header reuse and isolation, effective C
  auth identities, target checkpoint failure and cancellation/late completion.
- Installed static/shared CMake and pkg-config consumers: all seven feature
  profiles passed, 49 consumer checks.
- Generated C/C++ header, exports and additive ABI containment passed; existing
  records/selectors retain their layouts/values. Optional error outputs cover
  136 functions on both success and failure.
- Strict native-wrapper workspace Clippy and MQTT 5 each-feature Clippy passed
  (19 configurations with `--no-deps`). The broader client lint command encounters
  five pre-existing pedantic/nursery lints in `rumqttc-core/src/tls.rs`; that
  unrelated TLS code was left unchanged.
- Rust 1.88 wrapper-core/C all-target compilation, both workspace formatting
  checks and diff whitespace checks passed.

`wrapper-core/tests/redirect_policy.rs` covers independent scope/ID changes,
active target-key conflicts, permanent lease release, established nested temporary
restoration, resumed/refused tracked QoS 1/2, alias renegotiation, stale manual-ACK
generations, application SRV selection and sync/async identity changes.
Native redirect tests also reject expired/invalid profiles before changing
origin options/store, and prioritize shutdown admitted by a failing callback.
Cross-platform execution remains pending in existing CI; synchronous callbacks
still cannot be preempted.
