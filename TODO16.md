# Native Wrapper Core Architecture Consolidation

## Decision

Keep the current native-wrapper model and consolidate the responsibilities that
have crossed their intended ownership boundaries.

The retained model is:

- one `rumqttc-wrapper-core-next` package supporting MQTT 3.1.1 and MQTT 5;
- one immutable protocol selection per client;
- a dedicated, joinable driver thread with a current-thread Tokio runtime;
- a cloneable command handle and one owned event consumer;
- bounded, lossless ordinary event delivery with an independent terminal-status
  path;
- explicit separation between request admission and MQTT completion; and
- statically typed v4 and v5 event-loop integrations.

Do not rewrite the wrapper as a general actor system, replace the mature channel
implementation, merge the v4 and v5 MQTT state machines, or introduce dynamic
dispatch merely to reduce duplicated protocol-adapter code.

This work has five required outcomes:

1. `rumqttc-v5-next`, not the wrapper, becomes the sole owner of negotiated
   publish capabilities and connection-scoped outgoing Topic Alias admission
   state.
2. Wrapper shutdown commitment and operation completion use coherent internal
   state rather than the current distributed atomic/channel handshake.
3. Fallible rustls configuration construction moves into `rumqttc-core-next`.
4. The wrapper driver is divided into modules matching its actual architectural
   responsibilities without adding speculative abstraction layers.
5. Completion results become repeatably observable through a cloneable shared
   completion object, so native wrappers do not independently implement result
   caching and waiter coordination.

The command model and protocol-specific command extension shape are outside the
scope of this TODO.

## Current problems

### Duplicate MQTT 5 connection state

`rumqttc-wrapper-core` currently mirrors the broker's Topic Alias Maximum,
Maximum QoS, Retain Available flag, capability-known state, outgoing Topic Alias
mapping, and connection generation. `rumqttc-v5` independently owns the
authoritative protocol state and validates the same restrictions when requests
enter `MqttState`.

The split ownership requires the wrapper to call
`EventLoop::prepare_pending_topic_aliases_for_reconnect` after a connection
failure. That method exists to repair publishes admitted after the event loop's
cleanup drain but before an external producer gate observes the failure. This
is a correctness-preserving workaround, but it couples the wrapper to queue and
cleanup details that belong inside the v5 client.

### Distributed shutdown commitment

One wrapper shutdown currently spans `lifecycle`, `shutdown_kind`,
`shutdown_operation`, `shutdown_registration_ready`, the admission mutex, the
completion-registration channel, and the immediate-shutdown channel. The driver
may observe the underlying disconnect before it observes the corresponding
wrapper completion registration, so terminal processing spins with
`yield_now()` until the registration is published.

The behavior is tested, but a single logical transaction has too many partial
representations and ordering rules.

### Duplicated TLS construction

The wrapper parses PEM certificates and private keys, loads platform roots,
selects a rustls crypto provider, and constructs a `rustls::ClientConfig` even
though these responsibilities already belong to `rumqttc-core`. The duplication
exists because the shared TLS API can build platform roots without client
authentication or defer a custom-CA configuration until connection time, but
cannot yet fallibly construct every wrapper-supported configuration before the
driver starts.

### Overloaded driver module

`rumqttc-wrapper-core/src/driver.rs` contains public admission, native thread
ownership, event consumption, operation completion, shutdown coordination,
manual acknowledgement state, TLS construction, protocol construction, v4/v5
poll loops, event mapping, MQTT 5 validation, diagnostics, and error mapping.
These are not one module-level responsibility even though they cooperate in one
runtime.

### Per-wrapper completion caching

`CompletionHandle` currently owns a one-result receiver. Blocking and async
waits consume it, while polling borrows it. The C wrapper must add a mutex,
cache the terminal result, serialize access to the receiver, and separately
coordinate concurrent close callers. Other native wrappers would need the same
machinery to make an opaque completion safe for repeated or concurrent use.

## 1. Make MQTT 5 publish admission state authoritative in `rumqttc-v5`

### 1.1 Ownership

Add a v5 managed publish-admission component shared by builder-created
`AsyncClient` handles and their `EventLoop`. It must be the sole producer-side
owner of:

- whether negotiated publish capabilities are known for the active connection;
- broker Maximum QoS;
- broker Retain Available;
- broker Topic Alias Maximum;
- the outgoing manual Topic Alias mapping in producer admission order;
- the connection generation used to invalidate connection-scoped admissions;
  and
- notification of a transition that may unblock a waiting publish admission.

`MqttState` remains authoritative for wire protocol state, inflight exchanges,
replay, and the mapping actually established by requests it processes. The new
component is the authoritative admission boundary, not a second MQTT state
machine. Its values must be updated from the same CONNACK and connection-cleanup
transitions that update/reset `MqttState`.

Do not expose mutable capability fields to the wrapper. The wrapper may receive
typed admission outcomes and wait for an admission-state change, but it must not
read several independent atomics and reconstruct a capability snapshot itself.

### 1.2 Admission policy and API

Preserve the ordinary rumqttc use case in which applications queue work while
disconnected. Add an explicit builder policy for clients that require
producer-side negotiated validation. One acceptable public shape is:

```rust
pub enum PublishAdmissionPolicy {
    EventLoopValidated,
    RequireNegotiatedCapabilities,
}
```

`EventLoopValidated` preserves the existing general-client behavior.
`rumqttc-wrapper-core` must construct v5 clients with
`RequireNegotiatedCapabilities`.

Under `RequireNegotiatedCapabilities`, the client publish-admission path must:

1. perform static topic and property validation;
2. admit alias-free, non-retained QoS 0 while capabilities are unknown, because
   every MQTT 5 server supports that form;
3. return a typed transient result for QoS 1/2, retained, or Topic Alias
   publishes while capabilities are unknown;
4. validate QoS, retain, and Topic Alias values against one coherent negotiated
   snapshot;
5. reject an alias-only publish unless that alias has a mapping established by
   an earlier successfully admitted publish in the same connection generation;
6. update or rebind the producer alias mapping only after the corresponding
   request is successfully admitted to the request channel;
7. leave the mapping unchanged when validation or channel admission fails; and
8. serialize mapping changes with reconnect invalidation so an old-generation
   publish cannot establish state in the replacement generation.

The transient result must be distinguishable from bounded request-channel
backpressure even if the wrapper maps both to its public `ErrorKind::Backpressure`.
It must provide an async wait primitive or change notification that permits the
wrapper's `admit_async` path to wait without polling or sleeping, then retry the
complete admission transaction.

Dropping a pending async admission must not enqueue a request or mutate a Topic
Alias mapping. Once channel admission succeeds, dropping the caller's future or
completion handle must not cancel the MQTT operation.

### 1.3 Connection and replay transitions

On accepted CONNACK, install all negotiated values as one logical publication,
reset the outgoing alias map for the new network connection, advance the
generation, and wake capability waiters.

On connection loss, synchronize the producer admission boundary with event-loop
cleanup. The event loop must internally repair or reject requests admitted in
the cleanup race window before the new generation is opened. The wrapper must
not call `prepare_pending_topic_aliases_for_reconnect`, discard v5 request
queues, or supply an external producer lock after this change.

Replay requirements remain:

- an alias-bearing publish with a recoverable concrete topic is replayed with
  that topic and without the old connection's alias;
- an alias-only tracked publish whose topic cannot be recovered completes with
  a typed `TopicAliasReplayUnavailable` error;
- later alias rebindings cannot change the topic used to repair an earlier
  alias-only publish;
- replay preserves request order; and
- no Topic Alias crosses a network-connection boundary as active alias state.

The existing public reconnect-repair method may remain temporarily for source
compatibility, but the wrapper must stop using it. Deprecate it if no supported
external owner still needs the hook, and remove it only under the repository's
normal breaking-change policy.

### 1.4 Validation location

Move negotiated Maximum QoS, Retain Available, Topic Alias bound, and manual
alias-mapping validation out of `rumqttc-wrapper-core`. Keep wrapper-owned
validation only for wrapper boundary values that have not yet become rumqttc
types.

Static MQTT 5 publish-property validation should have one reusable source of
truth in the v5 client or codec layer. In particular, client-originated
Subscription Identifiers, payload-format UTF-8, Response Topic syntax, MQTT
UTF-8 field limits, and MQTT binary-data limits must not acquire divergent
wrapper-only rules. The v5 API used by the wrapper must reject these values
before request-channel admission.

Request-local invalidity discovered after channel admission must complete the
tracked notice with a typed request error. It must not terminate an otherwise
valid network connection merely because producer-side dynamic validation raced
a connection transition.

## 2. Consolidate operation completion and shutdown coordination

### 2.1 Completion cell and operation registry

Replace each operation's one-element Flume completion channel with an
`Arc`-backed internal completion cell. The driver or terminal cleanup completes
the cell exactly once with `Result<Completion>`.

The cell must provide:

- immutable `OperationId` identity;
- an initially pending state and one immutable terminal result;
- nonblocking observation;
- blocking wait;
- blocking wait with a caller deadline;
- async wait;
- wake-up of every blocking and async waiter after completion; and
- repeatable observation of the same cloned terminal result.

Use ordinary Rust synchronization plus Tokio notification already present in
the dependency graph. Do not add a channel, actor, or async-lock dependency for
this facility. No lock may be held while awaiting, blocking on unrelated work,
or invoking protocol code.

The driver-side operation registry must own pending cells until it completes or
fails them. Dropping every user-facing completion handle does not cancel an
admitted operation. Terminal driver failure and immediate shutdown must resolve
every registered operation with the existing delivery-ambiguity semantics
rather than leave a cell pending forever.

Completion remains distinct from admission. Creating an operation ID or cell
does not by itself mean the MQTT request was admitted.

### 2.2 Public completion contract

Make `CompletionHandle` cloneable and repeatably observable, or add a cloneable
`SharedCompletion` and return it from `Admission`. Prefer updating
`CompletionHandle` while the wrapper package remains pre-stable so there is one
obvious completion type.

All observation methods must borrow `&self`; waiting once must not consume the
handle. Required behavior:

```rust
pub fn try_wait(&self) -> Result<Option<Completion>>;
pub async fn wait_async(&self) -> Result<Completion>;
pub fn wait(&self) -> Result<Completion>;
pub fn wait_timeout(&self, timeout: Duration) -> Result<Completion>;
pub fn wait_timeout_outcome(&self, timeout: Duration) -> CompletionWaitOutcome;
```

Every successful observer receives an equivalent cloned completion. Every
observer of a terminal error receives an equivalent cloned error. A caller wait
deadline remains an observation outcome and must not be cached as the
operation's terminal result. Cancelling an async waiter changes neither the cell
nor other waiters.

Update `rumqttc-c` to store the shared completion directly. Remove its duplicate
terminal-result cache and receiver-consumption state. The C opaque object may
retain a small mutex only where required for C handle destruction or other FFI
ownership rules, not to recreate completion broadcast semantics.

### 2.3 Shutdown coordinator

Represent shutdown as one internal state machine with these phases:

```text
Running
ClosingGracefully
ClosingImmediately
Closed
Failed
```

The public `LifecycleState` may continue to collapse both closing modes into
`Closing`. Internally, one atomic phase may be retained for cheap observation
and immediate event-delivery bypass. The committed shutdown record must be one
coherent value protected by the existing admission ordering boundary and must
contain, at minimum:

- shutdown mode;
- operation ID and completion cell when an observable close was requested;
- the graceful timeout/deadline policy already submitted to rumqttc; and
- whether immediate shutdown escalated an earlier graceful shutdown.

A successful shutdown admission must commit the rumqttc request and the wrapper
record as one ordered transaction from the perspective of driver terminal
processing. A failed rumqttc admission must leave the client `Running`, must not
publish a latent shutdown operation, and must permit retry.

The driver may take the admission boundary briefly when consuming terminal
shutdown state. It must not spin, call `yield_now()` waiting for a registration,
or infer commitment from several independently loaded fields.

Required behavior remains:

- graceful shutdown completes only after rumqttc's existing disconnect barrier;
- immediate shutdown can escalate graceful shutdown;
- escalation completes the immediate request successfully and completes an
  incompatible graceful request with an ambiguous shutdown error;
- idempotent cleanup/finalizer shutdown does not require an operation cell;
- connection failure during a committed graceful shutdown retains the current
  documented terminal behavior;
- diagnostics admitted before a successful graceful barrier receive the final
  cached snapshot;
- unfinished operations on immediate shutdown remain ambiguous; and
- capacity waiters wake when lifecycle becomes terminal.

Remove `shutdown_operation`, `shutdown_registration_ready`, and the
`wait_for_shutdown_operation`/registration-drain handshake after the new
coordinator is in place. Do not retain shadow state for compatibility inside a
private implementation.

### 2.4 Native close ownership

Keep `NativeClient` responsible for driver-thread ownership and bounded join.
Keep `ClientHandle::close_now_idempotent` as the nonblocking cleanup path used by
foreign finalizers.

Provide one host-neutral idempotent close coordinator in wrapper core so C,
Python, and JavaScript wrappers do not independently decide how concurrent
graceful close, immediate escalation, completion observation, and join interact.
It may be an API on `NativeClient` or a separate owned `NativeClientCloser`, but
it must:

- coalesce concurrent graceful closes onto the same completion;
- make repeated successful graceful close return the same successful outcome;
- allow immediate close to escalate a pending graceful close;
- use one caller timeout budget across completion wait and thread join;
- never hold a mutex while waiting for MQTT or joining the thread; and
- preserve the nonblocking finalizer path.

The C wrapper may retain ABI-specific locks and failed-after-panic state, but it
must not retain a second MQTT shutdown state machine after this facility lands.

## 3. Move fallible rustls construction into `rumqttc-core`

### 3.1 Shared constructors

Add fallible constructors on the shared `TlsConfiguration` under the existing
rustls feature gates:

```rust
pub fn try_rustls_with_native_roots(
    client_auth: Option<(Vec<u8>, Vec<u8>)>,
) -> Result<Self, TlsError>;

pub fn try_rustls_with_pem_roots(
    ca: Vec<u8>,
    client_auth: Option<(Vec<u8>, Vec<u8>)>,
) -> Result<Self, TlsError>;
```

The tuple contains a PEM certificate chain and PEM private key. If more explicit
public input structs are introduced during API review, they must preserve the
same ownership and behavior rather than adding configuration scope.

`try_default_rustls()` must delegate to
`try_rustls_with_native_roots(None)`. Keep the existing infallible convenience
constructor only as a documented panicking adapter over the fallible path.

Both new constructors must fully construct `TlsConfiguration::Rustls`; they
must not defer malformed input until the first connection attempt.

### 3.2 Validation and provider behavior

The constructors must use the existing `rumqttc-core` rustls provider-selection
and root-store code. They must:

- report all platform-root loading errors through `TlsError`;
- reject a custom CA input containing no usable certificate;
- reject a client certificate input containing no certificate;
- reject a missing, empty, unsupported, or malformed private key;
- let rustls validate certificate/key compatibility;
- use no client authentication when the tuple is absent;
- preserve the selected workspace crypto-provider policy; and
- return typed sources without reducing them to wrapper-formatted strings.

Wrapper configuration validation must continue rejecting an unpaired client
certificate and private key before calling these constructors.

### 3.3 Wrapper dependency cleanup

Replace `rumqttc-wrapper-core::build_tls` with translation to the shared
constructors. Afterward remove the wrapper's direct dependencies on:

- `tokio-rustls`;
- `rustls-native-certs`; and
- `rustls-pki-types`.

Do not add a new TLS or certificate crate. The v4 and v5 clients already share
`rumqttc-core-next`, and the wrapper must use that implementation through the
re-exported shared TLS configuration API.

Preserve startup behavior: malformed supplied TLS material and platform-root
loading failures must be returned by `NativeClient::start` before the driver
thread is spawned.

## 4. Establish wrapper module boundaries

Split `rumqttc-wrapper-core/src/driver.rs` after or while moving the ownership
described above. Use the following responsibility layout unless implementation
details justify a small naming adjustment:

```text
rumqttc-wrapper-core/src/
├── lib.rs
├── command.rs
├── completion.rs
├── config.rs
├── error.rs
├── event.rs
├── protocol.rs
├── shutdown.rs
├── runtime.rs
├── handle.rs
├── operations.rs
├── acknowledgement.rs
└── adapter/
    ├── mod.rs
    ├── v4.rs
    └── v5.rs
```

Required ownership:

- `runtime.rs`: `NativeClient`, driver-thread start/join/drop ownership,
  `EventConsumer`, terminal-status publication, and ordinary event-buffer
  delivery policy;
- `handle.rs`: `ClientHandle`, clone/drop accounting, command dispatch,
  admission ordering, and sync/async/nonblocking admission modes;
- `operations.rs`: operation IDs, completion cells/registry, diagnostics
  control, and committed shutdown coordination;
- `acknowledgement.rs`: wrapper ACK tokens, generation validation,
  reservation/rollback, and tracked ACK completion;
- `adapter/v4.rs`: v4 client/options construction, the v4 poll loop, event and
  notice translation, diagnostics mapping, and v4 error mapping;
- `adapter/v5.rs`: the corresponding v5 functions and only the
  protocol-specific validation/translation still owned by the wrapper; and
- the existing boundary modules: public owned values and stable wrapper
  semantics only.

Keep shared private runtime structs in the narrowest module that owns their
invariants. Do not recreate one globally mutable `Shared` bag containing every
runtime concern. It is acceptable for a small `Shared` handle state to contain
the protocol client, lifecycle observation, admission ordering primitive, and
references to focused registries/coordinators.

### 4.1 Protocol loop policy

Keep explicit v4 and v5 poll loops. They must continue to preserve one pinned
`EventLoop::poll` future across wrapper control wake-ups so a select branch
cannot cancel a poll after it has dequeued a request or mutated protocol state.
Keep fair, non-biased arbitration for ordinary completion/diagnostics traffic
versus MQTT progress, with the existing intentional priority only for terminal
or immediate-shutdown paths.

Do not introduce `async-trait`, a boxed protocol-driver trait, or a macro that
hides the complete poll/select control flow merely to deduplicate the two
loops. Small pure conversion helpers may be shared where their semantics are
genuinely identical.

### 4.2 Event delivery policy

Preserve:

- one bounded ordinary event channel;
- no silent dropping of incoming publishes;
- finite delivery timeout followed by explicit driver failure;
- a separate capacity-independent terminal status path;
- one owned event consumer; and
- immediate shutdown bypass of an obstructed ordinary event channel.

Module movement must not merge terminal status into the ordinary bounded
channel or turn event delivery into an unbounded queue.

## 5. Error and semantic preservation

Keep the wrapper's existing distinction among:

- not admitted;
- rejected by the broker or local protocol policy;
- admitted with an ambiguous terminal delivery status;
- caller observation deadline elapsed; and
- an operation whose terminal result is itself a timeout.

Moving validation into `rumqttc-v5` must preserve broker reason codes and typed
notice causes. Moving TLS construction must preserve `TlsError` as an error
source. Consolidating completion must not turn a dropped waiter into operation
cancellation.

Do not make formatted error messages the cross-wrapper compatibility contract.
When new internal typed errors require wrapper mapping, classify them through
`ErrorKind`, `DeliveryStatus`, and broker reason data consistently for v4 and v5.

## 6. Dependency policy

No new runtime dependency is expected or approved by this TODO.

- Keep Flume for bounded/unbounded sync and async request/event transport and
  multi-receiver selection.
- Keep Tokio for the dedicated runtime, timers, notification, and async select.
- Keep `futures-util::FuturesUnordered` only if the new operation registry still
  requires driver-polled protocol notice futures. Remove it if completion is
  delivered directly without such a registry.
- Remove `futures-executor` only if the blocking admission path no longer uses
  it; do not replace it without a measured or correctness-driven reason.
- Do not add `async-trait`, an actor framework, a cancellation-token crate, a
  broadcast-channel crate, or a TLS abstraction crate for this work.

Any implementation that concludes a new dependency is necessary must document
the missing correctness primitive, binary/compile-time effect, feature impact,
and why the existing synchronization and channel dependencies cannot provide
it before adding the crate.

## 7. Implementation sequence

Implement in dependency order:

1. Add shared rustls constructors and tests in `rumqttc-core`; migrate the
   wrapper and remove its direct TLS dependencies.
2. Introduce the v5 managed publish-admission state and strict builder policy;
   add deterministic v5 tests before switching the wrapper.
3. Switch the wrapper to v5-owned admission, remove its mirrored capability and
   Topic Alias state, and remove its reconnect-repair call.
4. Add the shared completion cell and operation registry, then migrate ordinary
   publish/subscribe/unsubscribe/acknowledgement/diagnostics completions.
5. Replace the shutdown atomics/registration handshake with the shutdown
   coordinator and host-neutral idempotent close facility.
6. Simplify `rumqttc-c` completion and close state to consume the shared core
   facilities.
7. Split the wrapper driver into the required modules, moving tests alongside
   the responsible implementation where practical.
8. Run the full workspace, feature, wrapper, and C ABI/behavior verification.

Do not perform a file-only driver split first and then preserve the same broad
`Shared` state through cross-module visibility. The module boundaries should
reflect the new ownership.

## 8. Required tests

### 8.1 MQTT 5 admission and reconnect

Add deterministic tests for:

- strict admission before CONNACK for QoS 0 versus capability-dependent
  publishes;
- async wait and wake after CONNACK;
- cancellation while waiting for capabilities;
- Maximum QoS and Retain Available rejection;
- Topic Alias zero, broker maximum, initial mapping, alias-only reuse, and
  rebinding;
- failed/full-channel admission not mutating an alias mapping;
- multiple cloned producers racing alias-only use and rebinding;
- connection failure at the cleanup/admission boundary;
- an alias-only publish followed by a later rebinding in that race window;
- replay recovery and `TopicAliasReplayUnavailable` completion;
- generation reset on every network connection; and
- ordinary `EventLoopValidated` clients retaining their documented offline
  queueing behavior.

Use deterministic gates or test hooks for cleanup races. Do not rely on sleeps
to claim the producer/event-loop boundary is covered.

### 8.2 Completion and shutdown

Test:

- repeated poll and wait after success and after error;
- simultaneous blocking waiters;
- simultaneous async waiters;
- mixed blocking and async waiters;
- one wait deadline elapsing while another later observes success;
- async waiter cancellation without affecting other observers;
- handle clones dropped before and after terminal completion;
- driver failure completing every registered cell exactly once;
- graceful shutdown commitment racing ordinary admission;
- failed graceful admission restoring `Running` without latent state;
- immediate escalation before and after graceful completion registration;
- finalizer-style idempotent close without a completion handle;
- concurrent idempotent close callers sharing one outcome; and
- one timeout budget spanning close completion and thread join.

### 8.3 TLS

Test in `rumqttc-core` and through wrapper configuration:

- native roots without client authentication;
- native roots with a valid client certificate and key;
- custom CA with and without client authentication;
- empty and malformed CA input;
- empty and malformed certificate chains;
- missing, empty, malformed, and incompatible private keys;
- native root-loading failure where it can be injected; and
- failure occurring before wrapper driver-thread creation.

### 8.4 Protocol parity and regressions

Retain and run the existing wrapper lifecycle, shutdown, overload, reconnect,
manual-ack, diagnostics, and protocol-parity tests. Add parity cases whenever a
completion or shutdown behavior is common to both protocols. V5-only capability
and Topic Alias cases remain explicitly v5-only.

Run at minimum:

```text
cargo test -p rumqttc-core-next
cargo test -p rumqttc-v4-next
cargo test -p rumqttc-v5-next
cargo test -p rumqttc-wrapper-core-next
cargo test -p rumqttc-c-next
cargo check --workspace
```

Run the repository's `cargo hack` test and Clippy matrices for the affected
v4/v5 feature combinations before merge. Run the C header, ABI, native consumer,
and packaging checks because the C implementation changes even though its
public ABI should not.

## 9. Documentation and compatibility

Update `CHANGELOG.md` for all public or behavior-visible changes. Update:

- `rumqttc-wrapper-core/README.md` to describe v5-owned strict admission,
  repeatable completions, and consolidated close semantics;
- v5 client documentation for `PublishAdmissionPolicy` and its transient
  outcome;
- shared TLS documentation for the new fallible constructors;
- C wrapper documentation if completion polling/waiting becomes safely
  repeatable or close implementation guarantees are strengthened; and
- examples whose completion method receivers or close flow change.

The Rust packages are pre-stable, but do not hide semantic changes. Call out
changes to pre-CONNACK v5 admission, `CompletionHandle` method receivers,
completion cloneability, and any newly typed client errors.

Do not change the stable C declarations or numeric ABI values for an internal
cleanup. If a C-visible behavior is strengthened, document it without changing
function signatures unless a separately approved ABI revision requires one.

## 10. Completion criteria

This TODO is complete only when all of the following are true:

- the wrapper contains no broker capability atomics or outgoing Topic Alias
  map;
- the wrapper no longer calls a public v5 reconnect-repair hook;
- v5 strict producer admission and reconnect cleanup share one owner inside
  `rumqttc-v5`;
- no shutdown path waits for a separately published completion registration or
  spins with `yield_now()`;
- every admitted wrapper operation is completed exactly once through a shared
  completion cell or is retained by the registry until terminal resolution;
- completion handles are cloneable and repeatably observable by concurrent
  blocking and async callers;
- `rumqttc-c` no longer implements its own completion-result cache or MQTT close
  state machine;
- wrapper TLS construction uses `rumqttc-core` and the wrapper has no direct
  rustls/native-certificate parsing dependencies;
- wrapper runtime, admission, operations, acknowledgement, and protocol
  adapters have explicit module ownership;
- the dedicated driver, explicit v4/v5 loops, event backpressure, terminal
  delivery, and admission/completion semantics remain intact; and
- all required tests, feature matrices, formatting, linting, C ABI checks, and
  documentation updates pass.

