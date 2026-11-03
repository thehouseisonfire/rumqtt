# Native JavaScript Wrapper: Implementation and Validation Plan

Status: planned. Existing JavaScript features are foundations, not evidence that
the remaining bindings or their acceptance tests are complete.

## Scope and sources

Deliver the C wrapper's completed feature scope through `@rumqtt-next/rumqttc`,
using JavaScript/TypeScript values, promises, and the private Node-API addon.
The reference is the inclusive `b9060db0` through `7c16d43e` range, including
shared-core fixes, fixtures, packaging, and CI. The original `TODO16.md` alone
did not describe the resulting scope.

Use these sources together:

- [Core parity inventory](native-wrappers/wrapper-core/PARITY.md), public core
  contracts, and [core documentation](native-wrappers/wrapper-core/README.md).
- [C contracts](native-wrappers/c/README.md), [native fixtures](native-wrappers/c/tests/native/),
  and [package profiles](native-wrappers/c/tests/package_feature_matrix.py).
  Inspect these at `7c16d43e` for historical scope; current files include later
  additions. `git show 7c16d43e^:TODO16.md` retains the final validation record.
- [JavaScript contract](native-wrappers/js/README.md), facade/addon/declarations/tests,
  and [wrapper CI](.github/workflows/native-wrappers-ci.yml).

Milestone A comprises JS-BASE and JS-WC-01 through JS-WC-13: equivalent behavior
and validation for that C range. Milestone B tracks current-core additions below.
Completing A does not establish complete parity with today's core. Keep supported,
partial, pending, and intentionally omitted decisions separate. Historical
omissions must not become claims that supported core features are unavailable.

## Architecture and compatibility

- Keep `MqttClient`, promise-based operations, one bounded `events()` iterator,
  ESM/CommonJS entry points, and the same native artifact under Node.js, local
  Deno and Bun. Construction only validates/copies. The first `connect()` is the
  sole startup boundary and concurrent calls coalesce; preserve existing method
  guards, first-CONNACK semantics, and close-before-start behavior.
- Bind Rust wrapper-core directly. Implement adapters for `SessionStore`,
  `AsyncAuthenticator`, and `SrvResolver`. Do not route through C or construct
  protocol-client objects in the addon. MQTT state, checkpoint encoding, aliases,
  IDs, deadlines, admission and close remain core/native-owned. Reuse existing
  core/codec fixes instead of recreating their algorithms in JavaScript.
- Keep the maintained napi-rs/Tokio integration. Use bounded Node-API thread-safe
  dispatch to the owning environment; no per-operation threads, polling scheduler,
  or process-global JavaScript callback/client registry. A worker has its own
  registrations and teardown state, even if native execution can later be shared.
- Share Rust helpers only where ownership and semantics are language-neutral.
  Environment references, JS task scheduling and promise/error conversion belong
  in the addon. Establish/test missing shared contracts in core before binding.

## JS-BASE: types, errors, callbacks, and capabilities

### Public values and conversion

- Update `index.d.ts`, runtime validation, `index.cjs`/`index.js` exports and type
  tests together. Use camelCase, string-literal discriminants and focused records.
  Retain protocol-discriminated options: v5 settings on v4 are errors even when
  explicitly set to their defaults. Plain JS receives equivalent runtime checks.
- Use `Uint8Array` for binary values, accepting `Buffer` through that contract.
  Copy the exact view range, including nonzero offset, before suspension/admission
  or retention. Do not retain mutable input, detached backing stores, or unsafely
  shared memory. Retained bytes must be a stable snapshot; reject inputs for which
  that guarantee cannot be made. Returned buffers never alias live core state.
- Use `bigint` for operation IDs, generations, revisions and other potentially
  wide counters. Numbers require finite/safe-integer and core-width checks.
  Reject coercion from strings/booleans, negative/non-finite durations and overflow.
  Preserve current `Seconds`/`Ms` names; every new duration names its unit.
- Define omitted/`undefined`/`null` semantics per field; do not collapse absence,
  zero and present-empty bytes. Preserve ordered duplicate User Properties as
  arrays of pairs, not objects/maps. Check aggregate MQTT lengths and UTF-8.
  Use readonly types for retained records without claiming typed arrays are
  immutable; snapshot mutable data and return independently owned byte arrays.
- Prefer typed addon conversion. If private JSON remains, version the shape,
  preserve bytes/presence/order/structured errors, and encode wide integers
  losslessly rather than as JSON numbers. Functions require environment-owned
  registrations outside JSON. Keep the bridge private.
- Redact secrets from errors, nested `cause`, native diagnostics and automatic
  inspection. Secret-bearing public config owners need audited inspection where
  provided; do not claim to erase or hide caller-owned JS objects or Buffer copies.

### Structured errors and results

Record each core-to-addon-to-`MqttError` field mapping. Preserve code, kind,
retryability, delivery, operation ID and broker reason. Add typed optional
store/auth/redirect/resolver failures and protocol/phase/generation context.
Messages remain diagnostic, not machine-readable contracts. Later failure domains
are JS-WC-17. Do not replace cached terminal results with an observer timeout.

Test validation/admission rejection, admitted failure, broker rejection, ambiguity,
bigint preservation above the safe integer limit, and repeated terminal observation
after client teardown. New rich results preserve existing coarse completion fields.
Retained results/events must not keep drivers, execution contexts or callback
registrations alive through ownership cycles; test final release after both normal
close and driver failure.

### Callback dispatch and environment lifetime

Implement one bounded foundation before storage/auth/SRV:

1. Own requests and registrations before thread-safe dispatch. Invoke callbacks
   only on the owning environment, with no core lifecycle/admission lock held.
   Normalize immediate values, promises/thenables, throws and rejection through
   one completion path. Dispatch failure must complete/cancel native observation.
2. Define count/data bounds, queue saturation and per-family concurrency. Store
   and SRV serialize per client; shared owners may serve clients concurrently.
   Auth has one active response per client. Reusing a store object preserves its
   core owner/lease identity, rather than allocating a lease domain per registration.
3. Model pending/completed/cancelled calls. Exactly one native result wins; late
   settlements cannot revive cancelled work. Give callback contexts an
   `AbortSignal` or equivalent documented cancellation notification. Aborting
   observation does not prove host work stopped or that a save/clear did not commit.
4. Promise abandonment never recalls admitted MQTT work. Do not add implicit
   cancellation to existing operations. Callback reentry must not await an MQTT
   completion, close or join that requires that callback to return. Callback
   settlement cannot depend on saturated MQTT request-admission resources.
   Permit nonblocking admission where core allows it; independent work may be
   scheduled without awaiting its progress from the active callback.
5. Retain environment/function references through active dispatch, then release
   after quiescence. Break client/promise/registration cycles on driver failure.
   A never-settling promise must not prevent bounded native shutdown. Finalizers
   request cleanup; they are not proof of joined teardown.
6. Cover failed start, graceful/immediate close, deadline, GC, worker termination
   and environment cleanup. Stop dispatch before releasing references; cleanup
   cannot wait for JS callbacks on its own blocked thread. Do not change cleanup
   hook strategy without Node/Deno/Bun evidence: the existing synchronous hook
   works around a Deno async-hook limitation. Preserve its total cleanup budget;
   ordinary API close stays asynchronous. Consume late rejections without
   unhandled-rejection events or secret-bearing logs.

Use bounded broker barriers and worker/process tests for completion/cancellation
races, throws/rejections/thenables, callbacks that never settle, shared owners,
reentry, GC cycles, live-client exit and worker termination mid-call. Prove once-only
authority failure notification while the environment is live; never invoke JS
after teardown. Count native terminal results separately from host abort attempts.

### Loaded-artifact capabilities

Expose a typed query without connecting: protocols, TLS backends/provider build,
WS, proxy kinds, Unix/platform network controls, custom/system SRV, SCRAM, async
callbacks and tracing. Later features have separate support indicators. Distinguish
compiled capability, platform availability and broker negotiation. Ignore unknown
future native flags safely; declarations must agree with the loaded addon.
Unsupported selections fail before socket creation, without silent fallback.

## Milestone A feature slices

Each slice includes facade/addon/declarations, runtime/type mapping tests, JS host
fixtures, documentation and installed-artifact coverage. C fixture names specify
behavior to exercise through JS, not substitutes for language-boundary tests.

### JS-WC-01: Last Will and Testament

Add a typed last-will record with existing string/`Uint8Array` payload conventions
and v5 properties. Snapshot all mutable input at construction; reject mismatch.
Test exact v4/v5 CONNECT values, duplicate/present-empty properties, overwritten
and sliced caller buffers. Use separate changed/absent configurations to prove
default restoration and independence instead of adding C-style mutable setters.
Use a real broker and child process/worker for graceful suppression and abrupt
delivery on both protocols (`native_wire_options`, `native_will_process`).

### JS-WC-02: Durable sessions

Define a store interface: `load(key)` returns checkpoint bytes or `null` for not
found; `save(key, checkpoint)` and `clear(key)` return void. Methods may return
promises; invalid/undefined load results are not silently treated as missing.
Use owned key records with protocol, explicit stable scope and client ID. Document
supported format metadata without asking JS to parse/edit opaque envelopes.
Expose callback timeout, checkpoint limit and v5 strict/broker-only resume policy.
Check returned buffer length before an additional native copy; core validates
the envelope. Validate persistent identity/clean-session/start/expiry combinations.

Preserve per-client ordering, cross-client concurrency and one active core key
lease. Cross-process fencing/atomic replacement remain store obligations.
Corruption/version failure never silently clears data. This is protocol recovery,
not an application outbox. Native file-store support is JS-WC-20 and supplements
the generic store interface.

Acceptance follows `native_store_restart`, `native_store_atomic`,
`native_store_write_failure`, `native_store_ownership` and callback-race fixtures:
recover mixed subscriptions, outgoing QoS1/QoS2 IDs and incoming QoS2 without
redelivery on both protocols; cover session loss/resume policies, corrupt/wrong
version/protocol/oversized data, save/clear errors, interrupted atomic replacement,
lease conflicts/release, cancellation, process restart, unresolved promises and
worker exit. Sample stores must demonstrate atomicity and cleanup.

### JS-WC-03: Packet and runtime limits

Expose request/read batches, pending throttle with an explicit time unit, v4
input/output/inflight limits, and distinct v5 local input, advertised Maximum
Packet Size and outgoing inflight controls. Name default/unlimited/bounded modes;
do not overload zero. Zero request batch means one request; zero read batch means
native adaptive behavior. Throttle paces pending replay, not general publishing.

Observe exact packet boundaries, local/advertised independence, local/broker
inflight interaction, default restoration and resource bounds. Test batches at
0/1/8 using preadmitted work, broker barriers and queue backpressure; conversion
tests alone prove no runtime effect (`native_runtime_limits`,
`native_runtime_batching`). Live tuning belongs in JS-WC-16.

### JS-WC-04: MQTT 5 CONNECT and aliases

Add CONNECT properties for Session Expiry, Receive Maximum, Maximum Packet Size,
Topic Alias Maximum, Request Response/Problem Information, Method/Data and ordered
User Properties. Normalize overlaps with existing expiry/input options or reject
conflicts. Validate pairing, nonzero rules, widths and presence without coercion.
Keep automatic alias policy distinct from explicit PUBLISH aliases. Test exact
CONNECT values/defaults, copied inputs, runtime/type-level v4 rejection and replay
under changed reconnect limits, including concrete-topic and unrecoverable
alias-only terminal results.

### JS-WC-05: Enhanced authentication

Adapt `AsyncAuthenticator` with promise-based responses over owned context,
Start/Continue/final-Success challenges and Send/Complete actions. Preserve client
ID, exchange kind, generation, method, reason, data and ordered properties.
Expose tracked `reauthenticate()` and optional per-client SCRAM. The configured
authority exclusively supplies response properties; events remain observations.
The async core already exists; stale sync-only prose is not a missing contract.

Acceptance must prove:

- Initial/multi-step/repeated reauth, deferred final verification, exact AUTH/
  CONNECT properties, malformed AUTH/method changes and invalid SCRAM proof.
  Validate broker CONNACK capabilities before reporting authentication success.
- One deadline across callbacks, broker waits and final verification. An
  immediately settled answer after expiry still fails. Late/duplicate settlements
  have no effect; immediate close interrupts a pending native callback wait.
- Overlap gets its own bigint operation ID and `Overlapping`/`NotAdmitted`, sends
  no extra AUTH and leaves the first exchange able to succeed. A later exchange
  becomes admissible after the active exchange finishes.
- Broker rejection, thrown/rejected callbacks and transport loss preserve typed
  failures and broker AUTH detail. Active reauth reports Failed/ConnectionClosed
  before Disconnected/reconnect; failure notification occurs once. Preceding
  queued events remain observable before terminal cleanup.
- Retained values outlive client teardown; failed start, GC, worker termination
  and late promises release native owners without unhandled rejections/secrets.

Use deterministic broker barriers from `native_auth*` and core authentication
tests, not sleeps that merely make overlap likely.

### JS-WC-06: Fixed redirects and DNS SRV

Expose reject/bounded-follow policy and a promise-based resolver returning owned
priority/weight/port/target records. Custom resolution works independently of the
optional system resolver; native code owns weighting, fallback, loops/deadlines.
Fixed follow isolates origin credentials, store, proxy and header authority.

Preserve source/reason/reference, decision/failure, endpoint, attempt/limit/visited
counts and candidate position/count. Unresolved SRV has no selected endpoint;
failed target attempts retain context. Test both redirect sources and supported
reference/transport forms, loops/exhaustion, empty/failing/invalid answers,
priority/weight conversion, fallback, cancellation, disabled features and store
isolation (`native_redirect*`, `native_srv*`). Reuse seeded native weighting tests;
avoid flaky JS frequency tests. Current application-directed redirects are
JS-WC-14, not absent from core.

### JS-WC-07: Proxy transports

Add typed HTTP CONNECT/HTTPS/SOCKS5 settings separating broker/proxy endpoints,
credentials, supported DNS policy and independent proxy TLS. Mirror actual core
DNS modes; never fall back to direct connections. Test both protocols over
supported TCP/TLS/WS/WSS compositions, independent roots, auth, recoverable/terminal
failures, timeout/reconnect and captured-output redaction (`native_proxy_matrix`),
including installed disabled-feature builds.

### JS-WC-08: Unix endpoints

Add an explicit Unix target with documented path encoding, empty/NUL rejection
and early unsupported-platform errors. Test success, missing path, timeout,
reconnect, both close modes and Unix/TCP configuration independence. Custom
connectors are supported in core and tracked in JS-WC-14.

### JS-WC-09: Declarative WebSocket edits

Expose copied ordered add/replace/remove records, protected-header validation,
canonicalization/duplicate rules and secret-safe errors/inspection. Test exact
handshakes, default restoration, reconnect/redirect isolation, WS/WSS/proxy
composition and invalid/disabled features. Current dynamic authority is JS-WC-14.

### JS-WC-10: MQTT 5 DISCONNECT

Extend typed `CloseOptions` and add compatible options to `closeNow()`. Preserve
existing calls and `timeoutMs` semantics; copy reason/properties before admission.
First admitted payload wins; incompatible later calls fail without changing it.
Caller deadlines remain independent; escalation/finalization preserves the payload.
Test exact packets, validation, explicit-default v5 values on v4, type misuse,
coalescing, deadlines/escalation, repeatability and teardown after custom close
(`native_close_options`). Ordinary graceful drain never silently becomes ordered
publish shutdown.

### JS-WC-11: Rich events

Extend `MqttEvent` with all supported CONNACK fields/rejection details, broker
DISCONNECT, AUTH properties/failure, redirect context and outgoing packet/operation
IDs. Preserve ownership, presence and property order. Compare IDs with broker
packets; retain JS values after client teardown/subsequent receives and mutate
returned buffers to prove core independence. Saturate the single bounded iterator
with large events and prove close/terminal progress without an unbounded side queue
or silent incoming-message loss. Adapt `event_contract`, `native_event_properties`
and `native_event_queue` to runtime/type tests. Default ACK internals remain
completion-owned; opt-in protocol observations are JS-WC-17.

### JS-WC-12: Network controls and observability

Expose connection timeout, buffers, TCP_NODELAY, local bind and supported device/
MPTCP settings. Keep existing units. Observe actual socket effects, broker bind
ports, restored defaults, OS buffer adjustments, invalid devices, MPTCP/fallback
and unsupported platforms (`native_network_options`). Use reference sockets or
test-only native instrumentation; raw descriptors are not a public JS API.
Document ambient tracing without replacing global subscribers. Test inspection,
errors and process output for redaction. Owned structured logging is JS-WC-19.

### JS-WC-13: TLS backend and credentials

Extend TLS/WSS options with rustls/native backend, platform/custom roots,
PEM/PKCS#12 identity/password and ordered ALPN. Preserve PEM defaults and reject
contradictory legacy/nested settings. Broker, proxy and redirect TLS have independent
owned values. Reject malformed inputs/unavailable backends before sockets.

Test both protocols/backends over TLS/WSS: trusted/untrusted platform roots,
custom roots, PEM/PKCS#12 mutual TLS, ALPN, wrong hostname/password, malformed
credentials, failed-start cleanup, independent configuration, input mutation and
redaction (`native_tls_matrix`). Reuse isolated platform trust with unconditional
cleanup. Mock cleanup is not proof of actual OS trust. Advanced profiles/hooks
are JS-WC-15.

## Milestone B: current-core additions pending for JavaScript

These entries extend A; they are not implicitly complete or intentionally omitted.
Audit support before each slice. A JS-only omission needs a specific rationale and
compatibility/test impact. Reuse the existing contracts below.

### JS-WC-14: Transport authorities

Define promise-based connector/stream interfaces with owned buffers, partial I/O,
EOF, concurrent read/serialized writer, real flush, native absolute deadline,
socket-policy acknowledgement, base/established modes and fresh streams after
reconnect/cancellation. Do not assume Node streams satisfy the contract on Deno/Bun.
Expose dynamic async WS authority while preserving protected fields and refreshing
per attempt. Application redirect authority is synchronous in core. Add and test
a shared async authority contract once for JavaScript and Python before exposing
deferred decisions; preserve the existing synchronous/C contract. Do not block a
driver waiting for the JS environment. Preserve explicit credential/session reuse,
decision deadlines, bounded requests, stale-response rejection and key leases.
Sources: core `transport.rs`, `websocket.rs`, `redirect.rs`, [TODO31](TODO31.md).

### JS-WC-15: Advanced TLS

Bind immutable profiles, combined roots, versions/pins, backend masks, cipher/SNI/
resumption policy and async supplemental verification/external identity/signing.
Preserve standard authentication, exact signing input/validation, independent
layers, absolute deadlines, cancellation and typed stage/reason/layer errors.
Reject unenforceable backend policy. Sources: [TODO27](TODO27.md) and core TLS
contracts/profile/advanced fixtures.

### JS-WC-16: Runtime configuration, reconnect, and recovery

Bind typed reconnect/backoff/budgets, atomic unchanged/replace/clear updates with
staging/activation receipts, and tracked explicit session abandonment/recovery.
Preserve fixed identity, discarded-work ambiguity, clearing failures, supersession
and shutdown. Distinguish omitted fields from explicit clear without weakening
runtime validation. Sources: [runtime applicability](native-wrappers/wrapper-core/runtime-configuration.md),
[TODO34](TODO34.md), [TODO35](TODO35.md), [TODO36](TODO36.md).

### JS-WC-17: Observations and acknowledgements

Bind terminal broker ACK details on success/error, v5 manual ACK content, incoming
packet ID/generation, opt-in protocol/AUTH observations, diagnostic freshness and
structured native failures. Keep default behavior, single-use authority and
retained outcomes without callback ownership. Use bigint for wide observations.
Sources: [TODO28](TODO28.md), [TODO29](TODO29.md), [TODO37](TODO37.md),
[TODO43](TODO43.md), [TODO44](TODO44.md), [native failures](native-wrappers/native-failures.md).

### JS-WC-18: Admission and ordered shutdown

Bind publish policy/budget/recovery-pending and explicitly named ordered close.
Preserve finite replay/checkpoint accounting, native fences, conservative delivery,
pre-admission cancellation rollback and independent first-connect observation.
Prove where existing async admission/ACK methods satisfy C deferred/detached-handle
behavior rather than add redundant handles. JS buffers remain copied unless safe
immutable native storage has a measured use case; `Readonly<Uint8Array>` does not
make caller memory immutable. Sources: [publish admission](native-wrappers/wrapper-core/publish-admission.md),
[TODO30](TODO30.md), [TODO32](TODO32.md), [TODO41](TODO41.md), [TODO46](TODO46.md),
[TODO47](TODO47.md), [TODO48](TODO48.md), [TODO51](TODO51.md).

### JS-WC-19: Execution and structured logging

Expose explicit shared execution with async shutdown/join, capacity and peer
independence; bounded typed logging with independent flush/quiescence. Keep
environment references isolated and prove worker termination while peers run.
Public readiness/descriptors need a separate measured use case; managed execution
does not promise arbitrary runtime injection. Sources: [TODO33](TODO33.md),
[TODO40](TODO40.md), [TODO42](TODO42.md).

### JS-WC-20: Maintained file stores

Expose optional native store/context values alongside the generic interface,
bounded async inspect/quarantine/clear, leases, codec/allocation limits, cancellation
versus commit and joined teardown. Test C/Rust/JS checkpoint interoperability,
restart, errors and worker exit. Sources: [file-store contract](native-wrappers/file-session-stores.md),
[TODO50](TODO50.md).

Application-owned TLS-layer injection remains the parked [TODO49](TODO49.md)
experiment. Browser/Wasm is [TODO23](TODO23.md); this plan does not extend the
native package to browsers, Web Workers or runtimes unable to load Node-API addons.

## Delivery and validation

1. Record actual JS exposure per core field/operation/event with mapping, status,
   restriction/omission rationale and test references. Session Present policy and
   raw/effective diagnostics are already bound; test persistence interactions
   rather than re-add them. Review fields/semantics, not method names alone.
   The old inventory sentinel can skip execution; [TODO52](TODO52.md) tracks
   repair. A skipped audit is not evidence; retain explicit boundary tests.
2. Deliver JS-BASE; value options (01/03/04/07/08/09/10/12/13); storage (02);
   auth (05); SRV/redirects (06); final event audit (11). Event/error conversion
   needed by a slice lands with it. Deliver B separately from historical A.
3. Add Cargo feature forwarding/default policy and isolated addon build profiles.
   Preserve current defaults. Exercise minimal, rustls, native, mixed,
   rustls-proxy, native-proxy and mixed-proxy builds, plus focused AWS-LC/Ring,
   system-SRV, SCRAM, tracing/log compatibility and disabled features. Prevent
   workspace feature unification from masking missing addon support. No-provider
   builds need a real provider-installation path; do not ship unusable artifacts.
4. Test installed packages outside the checkout using ESM/CommonJS, exports,
   declarations, platform optional dependencies and loader diagnostics. Verify
   the same prebuilt addon under supported Node.js, local Deno and Bun versions
   on the README/release platforms, including glibc/musl and arm64 distinctions.
   Declare published artifact features explicitly; source-build test profiles
   must not silently create incompatible packages under one platform/version.
5. Extend shared broker/proxy/auth/DNS/WS fixtures with JS consumers; require
   Mosquitto for all four v4/v5 graceful/abrupt Will cases in CI. Use barriers,
   bounded waits, failure injection and worker/process teardown. Cover callbacks
   under all three runtimes; ordinary publish smoke tests do not prove their
   Node-API dispatch/cleanup behavior. Run supported sanitizer/leak checks.
6. Update runtime, declarations, type tests, examples, JS README and repository
   `CHANGELOG.md` per implemented slice. Examples continuously drain events and
   close in `finally`; callback examples demonstrate rejection/abort/cleanup.
   Plan-only edits do not claim shipped functionality.

From the repository root:

```bash
cargo fmt --manifest-path native-wrappers/Cargo.toml --all --check
cargo check --manifest-path native-wrappers/Cargo.toml --workspace
cargo test --manifest-path native-wrappers/Cargo.toml --workspace
```

From `native-wrappers/js`:

```bash
bun install --ignore-scripts
bun run build:native
bun run test:types
bun run test:node
bun run test:exports
```

Also run existing CI broker-backed Node/Deno/Bun behavior, live-client and worker
exit fixtures, package installation/archive checks and feature/release matrices.
Build the test-only panic profile for panic/lifecycle tests; production packages
must not expose test hooks. Core/native changes require focused regressions,
full affected crate tests and feature-sensitive CI checks. Unit/type tests alone
are insufficient.

Record revision, OS/architecture, JS runtime/version, features/provider, command,
pass/fail/skip and logs/JUnit artifacts for each slice/profile. Separate fixture
implementation, local execution, pending CI and unsupported configurations.
Historical C evidence at the endpoint was Linux-only for several matrices;
macOS/Windows wiring was not execution. C ABI/header/CMake checks are C obligations.

Milestone A requires every A mapping and lifecycle assertion through JavaScript
and execution evidence for advertised artifacts/runtimes. Record outstanding
validation as pending, never passed, and do not declare the milestone verified
while required execution remains outstanding. Complete current-core parity also
requires resolving B and newly audited additions as implemented or specifically
justified JavaScript omissions.
