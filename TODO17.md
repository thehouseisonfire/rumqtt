# Python Wrapper: Implementation and Validation Plan

Status: planned. Existing Python features are foundations, not evidence that
the remaining bindings or their acceptance tests are complete.

## Scope and sources

Deliver the C wrapper's completed feature scope through the typed `rumqttc-next`
CPython package, using Python values, `asyncio`, and the private PyO3 extension.
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
- [Python contract](native-wrappers/python/README.md), facade/extension/tests,
  and [wrapper CI](.github/workflows/native-wrappers-ci.yml).

Milestone A comprises PY-BASE and PY-WC-01 through PY-WC-13: equivalent behavior
and validation for that C range. Milestone B tracks current-core additions below.
Completing A does not establish complete parity with today's core. Keep supported,
partial, pending, and intentionally omitted decisions separate. Historical
omissions must not become claims that supported core features are unavailable.

## Architecture and compatibility

- Preserve `MqttClient`, `MqttClientOptions`, tracked operations, one `events()`
  consumer, and `async with`. Construction validates/copies; `connect()` starts
  native work and coalesces first-CONNACK observation. Keep first-use loop binding,
  early startup-event consumption, close-before-connect behavior, admission
  permits, and the independent ACK lane.
- Bind Rust wrapper-core directly. Implement adapters for `SessionStore`,
  `AsyncAuthenticator`, and `SrvResolver`. Do not route through the C ABI or
  construct protocol-client objects in the extension. MQTT state, checkpoint
  encoding, aliases, IDs, deadlines, admission and close remain core/native-owned.
  Reuse the core/codec fixes already delivered by the C work.
- Keep `pyo3-async-runtimes` and the production bounded blocking-work design.
  Follow the [rejected custom-bridge ADR](native-wrappers/python/experiments/explicit_python_bridge/ADR.md).
  Host callback dispatch is an adapter to the owning loop, not a new executor,
  per-operation thread, polling loop, or global client-registry redesign.
- Share Rust helpers where semantics and ownership are language-neutral. Python
  references, GIL handling, loop scheduling and exception conversion stay in the
  Python bridge. Establish/test missing core contracts before binding them.

## PY-BASE: values, errors, callbacks, and capabilities

### Public values and conversion

- Extend frozen dataclasses/enums in `_types.py`, callback `Protocol` definitions,
  exports and validation together. Use snake_case, compatible keyword options,
  `bytes` outputs and immutable tuples for retained sequences. A frozen record
  must not retain mutable nested input. Preserve constructors and defaults.
- Use seconds for public durations. Check finite values, documented zero modes,
  integer widths, aggregate MQTT lengths, MQTT UTF-8, protocol selectors and
  exclusive choices. Reject `bool` as an integer/duration. Validate before native
  allocation/startup where possible, with authoritative core validation as well.
- Preserve bytes-like input support; snapshot mutable/sliced buffers before
  suspension or retention. Keep `None` versus present-empty/zero and ordered,
  duplicate User Properties. Never represent User Properties as a dictionary.
- Prefer typed native conversion. If private JSON remains, version its shape
  and test bytes, wide integers, optional fields, ordering and errors. Callables
  require typed PyO3 registrations; JSON cannot carry their identity/ownership.
- Redact secret-bearing reprs (`repr=False` or audited custom repr), exceptions,
  callback exception chains and native output. Include passwords, keys, auth
  data, headers, checkpoints and payloads. Do not promise zeroization of Python,
  OS or TLS-library copies.

### Structured errors and results

Record each core-to-Rust-to-Python field mapping. Preserve existing exception
classes, code/kind, retryability, delivery, operation ID and broker reason. Add
typed optional store, auth, redirect/resolver failures and `ErrorContext`
protocol/phase/generation. Absence differs from zero. Caller observation timeout
must not replace a cached operation result. Messages are not the data contract.
Later failure domains are tracked in PY-WC-17.

Test local rejection, admitted failure, broker rejection, ambiguity and repeated
observation after client destruction. Richer results must preserve existing
coarse completion fields and their meaning. Retained results/events must not
keep drivers, execution contexts or callback registrations alive through ownership
cycles; test final release after both normal close and driver failure.

### Callback dispatch and ownership

Implement one bounded foundation before storage/auth/SRV:

1. Own requests and registrations before scheduling onto the client's loop.
   Invoke Python with the correct interpreter/GIL context and no core lifecycle
   or admission lock. Never synchronously block a native worker awaiting Python.
2. Define count/data bounds and overload results. Store/SRV calls serialize per
   client; shared owners may serve clients concurrently. Auth has one active
   response per client. Reusing a store object preserves its core owner/lease
   identity instead of creating an independent lease domain per registration.
3. Model pending/completed/cancelled calls. Exactly one native result wins.
   Dropping core observation requests cancellation on a live loop; cancellation
   is cooperative. Detached work owns its data and late results have no effect.
   A cancelled save/clear may have committed and must remain crash-consistent.
4. Cancelling an MQTT waiter never recalls admitted work. Document callback
   reentry: do not await MQTT completion, close or join that needs the callback
   to return. Keep callback completion independent of saturated MQTT admission.
   Permit nonblocking admission where core allows it; independent tasks may be
   scheduled without awaiting their progress from the active callback.
5. Retain owners through active invocations; release after quiescence, accounting
   for Python-held configuration references. Break client/task/registration
   cycles on driver failure. Finalizers request nonblocking cleanup. Explicit
   close retains its total budget and cancellation-safe escalation.
6. Cover failed start, graceful/immediate close, deadline, exception, last-reference
   drop, loop closure and interpreter shutdown. Do not schedule into dead loops
   or call Python after interpreter finalization. Consume late task exceptions
   without printing secrets. Dispatch authority failure notifications once while
   the loop is live; teardown must not depend on executing them on a dead loop.

Use barriers/subprocess deadlines to test completion/cancellation races,
never-returning and cancellation-suppressing callbacks, shared owners, reentry,
GC cycles, owner release and shutdown at every transition. Count native terminal
results separately from attempts to interrupt host work.

### Loaded-artifact capabilities

Provide a typed query without client startup: protocols, TLS backends/provider
build, WS, proxy kinds, Unix/platform network controls, custom/system SRV, SCRAM,
async callbacks and tracing. Later features have separate support indicators.
Distinguish compiled support, platform availability and broker negotiation.
Unknown future native flags must not break consumers. Unsupported selections
fail before socket creation, with no silent fallback.

## Milestone A feature slices

Each slice includes facade, bridge, mapping tests, Python host fixtures,
documentation and installed-artifact coverage. C fixture names below specify
behavior to exercise through Python, not substitutes for Python tests.

### PY-WC-01: Last Will and Testament

Add `LastWill` and typed v5 Will properties; copy topic/payload/properties at
construction and reject protocol mismatch. Test exact v4/v5 CONNECT values,
present-empty fields and overwritten caller buffers. Test independent clients
with changed/absent Wills: Python immutability replaces C set/replace/clear, but
ownership/default-restoration outcomes remain required. Use a real broker and
child process for graceful suppression and abrupt-exit delivery on both protocols
(`native_wire_options`, `native_will_process`).

### PY-WC-02: Durable sessions

Define an async store `Protocol`: `load(key)` returns opaque bytes or `None` for
not found; `save(key, checkpoint)` and `clear(key)` return no value. Use a frozen
key with protocol, explicit stable scope and client ID. Document supported format
metadata; applications store checkpoints unchanged rather than edit envelopes.
Expose callback timeout, checkpoint limit and v5 strict/broker-only resume policy.
Check loaded buffer size before an additional native copy; validate the envelope
again in core. Validate persistent identity/clean-session/start/expiry combinations.

Preserve per-client ordering, cross-client concurrency and one active core key
lease. Cross-process fencing/atomic replacement remain store obligations.
Corruption/version failure never silently clears data. This is protocol recovery,
not an application outbox. The native file adapter is PY-WC-20, not a replacement
for the generic protocol.

Acceptance follows `native_store_restart`, `native_store_atomic`,
`native_store_write_failure`, `native_store_ownership` and callback-race fixtures:
recover mixed subscriptions, outgoing QoS1/QoS2 IDs and incoming QoS2 without
redelivery on both protocols; cover session loss/resume policies, corrupt/wrong
version/protocol/oversized data, save/clear errors, interrupted atomic replacement,
lease conflicts/release, callback cancellation and process restart. Sample stores
must demonstrate atomicity and cleanup as well as successful persistence.

### PY-WC-03: Packet and runtime limits

Expose request/read batches, pending throttle, v4 input/output/inflight limits,
and separate v5 local input, advertised Maximum Packet Size and outgoing inflight
controls. Name default/unlimited/bounded modes. Zero request batch means one
request; zero read batch uses native adaptive behavior. Throttle paces pending
replay, not general publishing. Live tuning belongs in PY-WC-16.

Observe exact packet boundaries, local versus advertised limits, local/broker
inflight interaction, restored defaults and resource bounds. Test batches at
0/1/8 using preadmitted work, broker barriers and queue backpressure; successful
option conversion alone proves no runtime effect (`native_runtime_limits`,
`native_runtime_batching`).

### PY-WC-04: MQTT 5 CONNECT and aliases

Add `V5ConnectProperties`: Session Expiry, Receive Maximum, Maximum Packet Size,
Topic Alias Maximum, Request Response/Problem Information, Method/Data and ordered
User Properties. Resolve overlap with existing expiry/input options by normalization
or explicit conflict errors. Validate pairing, booleans, nonzero rules and presence.
Keep automatic outgoing alias policy separate from explicit PUBLISH aliases.
Test exact CONNECT values, defaults, copied inputs, v4 rejection and reconnect
under changed alias limits, including concrete-topic replay and unrecoverable
alias-only terminal results.

### PY-WC-05: Enhanced authentication

Adapt `AsyncAuthenticator` with async responses over owned context,
Start/Continue/final-Success challenges and Send/Complete actions. Preserve client
ID, exchange kind, generation, method, reason, data and ordered properties.
Expose tracked `reauthenticate()` and optional per-client SCRAM. Response properties
come from the sole configured authority; lifecycle events are observations.
The async core already exists; stale sync-only prose is not an implementation gap.

Acceptance must prove:

- Initial/multi-step/repeated reauth, async final server verification, exact
  AUTH/CONNECT properties, malformed AUTH/method changes and invalid SCRAM proof.
  Validate broker CONNACK capabilities before reporting authentication success.
- One deadline includes callbacks, broker waits and final verification. Even an
  immediately ready answer after expiry fails. Late/duplicate answers cannot
  revive an exchange; immediate close wakes pending callback work.
- An overlap gets its own operation ID and `Overlapping`/`NotAdmitted`, sends no
  extra AUTH and leaves the first exchange able to succeed. A later exchange
  becomes admissible after the active exchange finishes.
- Broker rejection, host exception and transport loss preserve typed failures
  and broker AUTH details. Active reauth reports Failed/ConnectionClosed before
  Disconnected/reconnect; authority failure notification occurs once. Preceding
  queued events remain observable before terminal cleanup.
- Retained events/outcomes survive teardown; failed start, GC, loop closure and
  cancelled callbacks release owners; captured output is redacted.

Use deterministic broker barriers as in `native_auth*` and core authentication
tests, not sleeps that merely make overlap likely.

### PY-WC-06: Fixed redirects and DNS SRV

Expose reject/bounded-follow policy and an async resolver with owned priority,
weight, port and target records. Custom resolution works independently of the
optional system resolver. Native code owns weighting, fallback, loops/deadlines.
Fixed follow isolates targets from origin credentials, store, proxy and headers.

Preserve source/reason/reference, decision/failure, selected endpoint,
attempt/limit/visited counts and candidate position/count. Unresolved SRV has no
selected endpoint; failed target attempts retain context. Test both redirect
sources, supported reference/transport forms, loops/exhaustion, empty/failing/
invalid answers, priority/weight conversion, fallback, cancellation, disabled
features and origin-store isolation (`native_redirect*`, `native_srv*`). Reuse
seeded Rust weighting evidence; avoid flaky host frequency tests. Current
application-directed redirects are PY-WC-14, not absent from core.

### PY-WC-07: Proxy transports

Add HTTP CONNECT/HTTPS/SOCKS5 records separating broker/proxy endpoints,
credentials, supported DNS policy and independent proxy TLS. Mirror actual core
DNS modes. Unsupported choices never fall back to direct networking. Test both
protocols over supported TCP/TLS/WS/WSS compositions, independent roots, auth,
recoverable/terminal failures, timeouts, reconnect and redaction
(`native_proxy_matrix`), including installed disabled-feature builds.

### PY-WC-08: Unix endpoints

Add an explicit Unix target with documented Python path normalization/encoding,
empty/NUL rejection and early unsupported-platform errors. Test missing paths,
timeouts, reconnect and both close modes; Unix and TCP configurations remain
independent. Custom connectors are supported in core and tracked in PY-WC-14.

### PY-WC-09: Declarative WebSocket edits

Expose copied ordered add/replace/remove edits, protected-header validation,
canonicalization/duplicate rules and redacted reprs. Test exact handshakes,
default restoration, reconnect/redirect isolation, WS/WSS/proxy composition and
invalid/disabled features. Current dynamic async authority is PY-WC-14.

### PY-WC-10: MQTT 5 DISCONNECT

Add typed reason/properties as compatible keyword options to `close()` and
`close_now()`. Copy before admission; first admitted payload wins and conflicting
later calls fail. Caller deadlines remain independent. Escalation/finalization
preserves the payload. Test exact packets, validation, explicit-default v5 values
on v4, concurrent callers, cancellation/escalation, repeatability and destruction
after custom close (`native_close_options`). Ordinary graceful drain must not
silently become ordered publish shutdown.

### PY-WC-11: Rich events

Map all supported CONNACK fields/rejection details, broker DISCONNECT, AUTH
properties/failure, redirect context and outgoing packet/operation IDs. Keep owned
frozen values, presence and ordered properties. Compare IDs against broker packets;
retain events after client destruction/subsequent receives. Saturate the bounded
queue with large events and prove close/terminal status progresses without an
unbounded side queue or silent incoming-message loss. Adapt `event_contract`,
`native_event_properties` and `native_event_queue` into Python value/type tests.
Default ACK internals stay completion-owned; opt-in observations are PY-WC-17.

### PY-WC-12: Network controls and observability

Expose connection timeout, buffers, TCP_NODELAY, bind address and supported
device/MPTCP settings. Preserve existing timeout rules. Observe socket effects,
broker bind ports, restored defaults, OS buffer adjustment, invalid devices,
MPTCP/fallback and unsupported-platform failure (`native_network_options`). Use
reference sockets/test-only native instrumentation; do not expose raw descriptors.
Document ambient tracing without replacing global subscribers. Audit reprs,
exceptions and process output. Owned structured logging is PY-WC-19.

### PY-WC-13: TLS backend and credentials

Extend `TlsOptions` with rustls/native selection, platform/custom roots, PEM or
PKCS#12 identity/password and ordered ALPN. Preserve PEM defaults and reject
contradictory legacy/nested settings. Broker, proxy and redirect TLS own separate
values. Validate malformed inputs/unavailable backends before sockets.

Test TLS/WSS for both protocols/backends: trusted/untrusted platform roots,
custom roots, PEM/PKCS#12 mutual TLS, ALPN, wrong hostname/password, malformed
credentials, failed-start cleanup, independent configurations, input mutation and
redaction (`native_tls_matrix`). Reuse isolated platform trust with unconditional
cleanup. Mocked cleanup tests do not prove OS trust. Advanced profiles/hooks are
PY-WC-15.

## Milestone B: current-core additions pending for Python

These entries extend A; they are not implicitly complete or intentionally omitted.
Audit actual support before each slice. A Python-only omission requires a specific
rationale and compatibility/test impact. Reuse the existing contracts below.

### PY-WC-14: Transport authorities

Define async connector/stream protocols with owned buffers, partial I/O, EOF,
concurrent read/serialized writer, real flush, native absolute deadline,
socket-policy acknowledgement, base/established modes and fresh streams after
reconnect/cancellation. Adapt `asyncio` stream buffering/`drain()` semantics
explicitly instead of treating a successful `write()` as a completed native flush.
Expose async WS authority while preserving protected fields and refreshing per
attempt. Application redirect authority is synchronous in core. Add and test a
shared async authority contract once for Python and JavaScript before exposing
deferred decisions; preserve the existing synchronous/C contract. Do not block a
driver waiting for the Python loop. Preserve explicit credential/session reuse,
decision deadlines, bounded requests, stale-response rejection and key leases.
Sources: core `transport.rs`, `websocket.rs`, `redirect.rs`, [TODO31](TODO31.md).

### PY-WC-15: Advanced TLS

Bind immutable profiles, combined roots, versions/pins, backend masks, cipher/SNI/
resumption policy and async supplemental verification/external identity/signing.
Preserve standard authentication, exact signing input/validation, independent
layers, absolute deadlines, cancellation and typed stage/reason/layer failures.
Reject unenforceable policy. Sources: [TODO27](TODO27.md) and core TLS contracts/
profile/advanced fixtures.

### PY-WC-16: Runtime configuration, reconnect, and recovery

Bind typed reconnect/backoff/budgets, atomic unchanged/replace/clear updates with
staging/activation receipts, and tracked explicit session abandonment/recovery.
Preserve fixed identity, discarded-work ambiguity, checkpoint-clear failures,
supersession and shutdown. Use typed update variants or a private unchanged
sentinel so `None` does not conflate unchanged and clear.
Sources: [runtime applicability](native-wrappers/wrapper-core/runtime-configuration.md),
[TODO34](TODO34.md), [TODO35](TODO35.md), [TODO36](TODO36.md).

### PY-WC-17: Observations and acknowledgements

Bind terminal broker ACK details on success/error, v5 manual ACK content, incoming
packet ID/generation, opt-in protocol/AUTH observations, diagnostic freshness and
structured native failures. Preserve default behavior, single-use authority and
retained outcomes without callback ownership. Python integers preserve wide IDs
without floating-point conversion.
Sources: [TODO28](TODO28.md), [TODO29](TODO29.md), [TODO37](TODO37.md),
[TODO43](TODO43.md), [TODO44](TODO44.md), [native failures](native-wrappers/native-failures.md).

### PY-WC-18: Admission and ordered shutdown

Bind publish policy/budget/recovery-pending and explicitly named ordered close.
Preserve finite replay/checkpoint accounting, native fences, conservative delivery,
pre-admission cancellation rollback and independent first-connect observation.
Prove which existing async admissions/ACK values satisfy C deferred/detached-handle
behavior rather than add redundant handles. Keep payload copies unless safe
immutable native storage has a measured use case; a read-only `memoryview` alone
does not prove its backing allocation cannot change.
Sources: [publish admission](native-wrappers/wrapper-core/publish-admission.md),
[TODO30](TODO30.md), [TODO32](TODO32.md), [TODO41](TODO41.md), [TODO46](TODO46.md),
[TODO47](TODO47.md), [TODO48](TODO48.md), [TODO51](TODO51.md).

### PY-WC-19: Execution and structured logging

Expose explicit shared execution with async shutdown/join, capacity and peer
independence; bounded typed logging with independent flush/quiescence. Benchmark
contexts are not a public API. Do not revive the rejected scheduler. Public
readiness/descriptors need a separate measured use case; arbitrary runtime
injection remains outside managed execution.
Sources: [TODO33](TODO33.md), [TODO40](TODO40.md), [TODO42](TODO42.md), Python bridge ADR.

### PY-WC-20: Maintained file stores

Expose optional native store/context values alongside the generic protocol,
bounded async inspect/quarantine/clear, leases, codec/allocation limits, cancellation
versus commit and joined teardown. Test C/Rust/Python checkpoint interoperability,
restart and failure. Sources: [file-store contract](native-wrappers/file-session-stores.md),
[TODO50](TODO50.md).

Application-owned TLS-layer injection remains the parked [TODO49](TODO49.md)
experiment. Browser/Wasm is [TODO23](TODO23.md). PyPy, free-threaded CPython,
subinterpreters and the limited API remain outside the documented runtime contract.

## Delivery and validation

1. Record actual Python exposure per core field/operation/event with mapping,
   status, restrictions/omission rationale and test references. Existing Session
   Present policy/raw-effective diagnostics are already bound; test their new
   persistence interactions rather than re-add them. Review fields and semantics,
   not only method names. The old inventory sentinel can skip execution;
   [TODO52](TODO52.md) tracks repair. A skipped audit is not parity evidence;
   maintain explicit boundary tests meanwhile.
2. Deliver PY-BASE; value options (01/03/04/07/08/09/10/12/13); storage (02);
   auth (05); SRV/redirects (06); final event audit (11). Event/error conversion
   needed by a slice lands with it. Deliver B separately from historical A.
3. Add Cargo feature forwarding/default policy and Maturin profiles. Preserve
   current default wheels. Exercise isolated minimal, rustls, native, mixed,
   rustls-proxy, native-proxy and mixed-proxy builds, plus focused AWS-LC/Ring,
   system-SRV, SCRAM, tracing/log compatibility and disabled features. Avoid
   workspace feature unification masking missing Python support. No-provider
   builds need a real provider-installation path; do not advertise unusable builds.
4. Test installed wheels in fresh environments outside the checkout: types and
   `py.typed`, exports, contents, native dependencies, source installs and all
   advertised tags. Retain the README/release CPython 3.10–3.14 and Linux/macOS/
   Windows matrix unless deliberately changed with evidence.
5. Extend existing broker/proxy/auth/DNS/WS fixtures for Python consumers. Require
   Mosquitto for all four v4/v5 graceful/abrupt Will cases in CI. Use barriers,
   bounded waits, fault injection, lifecycle subprocesses and supported native
   sanitizer/leak checks. C tests alone never verify the PyO3/loop boundary.
6. Update examples, Python README and repository `CHANGELOG.md` per implemented
   slice. Examples drain events, close with `async with`/`finally`, and show
   callback failure/cleanup. Plan-only edits do not claim shipped functionality.

From the repository root:

```bash
cargo fmt --manifest-path native-wrappers/Cargo.toml --all --check
cargo check --manifest-path native-wrappers/Cargo.toml --workspace
cargo test --manifest-path native-wrappers/Cargo.toml --workspace
```

From `native-wrappers/python`:

```bash
uv sync --group dev
uv run maturin develop --features panic-testing
uv run ruff check python tests
uv run ruff format --check python tests
uv run pyrefly check python tests
uv run pytest -q
uv run maturin build --release --out ../../target/python-wheels
```

Also run broker integration, lifecycle subprocesses, installed-wheel consumers
and feature/release matrices using existing CI commands. Core/native changes
require focused regressions, full affected crate tests and feature-sensitive CI
checks. The unit suite alone is insufficient.

Record revision, OS/architecture, Python version, features/provider, command,
pass/fail/skip and logs/JUnit artifacts for each slice/profile. Separate fixture
implementation, local execution, pending CI and unsupported configurations.
Historical C evidence at the endpoint was Linux-only for several matrices;
macOS/Windows wiring was not execution. C ABI/header/CMake checks are C obligations.

Milestone A requires every A mapping and lifecycle assertion above through Python
and execution evidence for the advertised wheel matrix. Record outstanding
validation as pending, never passed, and do not declare the milestone verified
while required execution remains outstanding. Complete current-core parity also
requires resolving B and newly audited additions as implemented or specifically
justified Python omissions.
