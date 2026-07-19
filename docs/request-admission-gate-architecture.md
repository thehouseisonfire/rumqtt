# Request admission gate architecture

## 1. Executive conclusion

The repository has a demonstrated admission weakness, but it does not need a strict linearizable gate. Today a builder-created client can return success after graceful shutdown has become active in the event loop, even though the request will never enter MQTT protocol state. Untracked work is then silently dropped; tracked work usually resolves as the generic dropped-sender (`Recv`) error. With an unbounded channel this can continue indefinitely while the `EventLoop` value remains alive. With a bounded or zero-capacity channel, a send can instead remain blocked because graceful draining stops receiving normal and control requests.

The proportionate architecture is a **small managed-mailbox admission façade with an event-loop-authoritative lifecycle policy**:

- Keep the existing normal, control, and immediate-disconnect channels and their scheduling independence.
- Replace the managed `RequestSender::WithNotice { ... }` payload with one `Arc<ManagedMailboxTx>` that owns those three senders and a compact atomic admission snapshot. Keep `RequestSender::Plain` behavior unchanged.
- Model only a few lifecycle phases and request-class denial bits, with a small structured rejection reason. Do not introduce a callback-driven or dynamically extensible policy engine.
- Have normal and control submission methods perform one acquire load and a predictable open-path branch before sending. Tracked methods use the same check. `disconnect_now()` keeps its independent path and bypasses the policy check.
- Treat the event loop as authoritative. It publishes `Closing` when it **processes** a graceful disconnect envelope, not when a client merely queues it. It rejects/drains envelopes that must not enter protocol state and supplies explicit tracked rejection results. Terminal completion also retires the normal/control receiving sides so bounded and rendezvous senders wake with ownership-preserving send errors.
- Preserve the existing meaning of successful untracked methods: channel admission only. A request that raced from an earlier “open” observation may still return success but be rejected before protocol admission. Do not claim a global linearization point.
- Do not add an epoch field to every envelope initially. Preserve an epoch in the compact state encoding as an architectural seam, but pay the measured 8-byte per-envelope cost only if a real reversible pause/reconfiguration requirement needs stale-envelope discrimination.

This supports terminal closure and a credible future pause/quiesce-by-class feature without imposing locks, atomic read-modify-write operations, or permit accounting on publish-heavy traffic. It is more reusable than a shutdown boolean, but materially smaller than a general admission framework.

## 2. Current admission and shutdown behavior

### 2.1 Submission topology

The v4 and v5 topologies are the same.

Builder flow:

1. `AsyncClientBuilder::build` / `ClientBuilder::build` call `build_async_client` in `rumqttc-v4/src/client.rs` and `rumqttc-v5/src/client.rs`.
2. `EventLoop::new_for_async_client_with_capacity` creates:
   - `requests`: flow-controlled `PUBLISH` envelopes;
   - `control_requests`: every non-`PUBLISH` normal request, including graceful disconnect;
   - unbounded `immediate_disconnect`: `DisconnectNow` only.
3. `RequestSender::WithNotice` stores all three Flume senders. Cloning `AsyncClient` or `Client` clones this enum and therefore all three managed senders.
4. `send_request_async`, `send_request`, and `try_send_request` classify only `Request::Publish` versus everything else and select the normal or control sender. Tracked publish/subscribe/unsubscribe helpers construct a oneshot notice and route to the corresponding channel. V5 additionally routes tracked `AUTH` on the control channel.
5. `send_immediate_disconnect_*` uses the dedicated unbounded channel for builder clients.

The relevant definitions are `RequestSender`, `AsyncClient`, the send helpers, and `build_async_client` in each `src/client.rs`; `RequestEnvelope`, `RequestChannelCapacity`, the channel fields, and `new_for_async_client_with_capacity` are in each `src/eventloop.rs`. Public request variants are in each `src/lib.rs` (`Request` is v4 at the MQTT 3.1.1 surface and v5 at the MQTT 5 surface).

Plain-sender flow is intentionally different. `AsyncClient::from_senders(Sender<Request>)` and `Client::from_sender(Sender<Request>)` create `RequestSender::Plain`. All untracked requests—including `DisconnectNow`—share the caller's single Flume channel. Tracking is unavailable, there is no event loop, no replay, no priority path, and no library-owned lifecycle policy. The constructor documentation already describes these limitations. A gate added to builder clients must not silently redefine this low-level request-sink contract.

`EventLoop::new(options, cap)` is another compatibility case: it constructs all three channels but retains internal sender clones in `_requests_tx`, `_control_requests_tx`, and `_immediate_disconnect_tx`. In contrast, builder event loops retain no internal senders, so dropping every client clone can disconnect the request sources and allow `ConnectionError::RequestsDone`.

### 2.2 What “successfully submitted” means

A public method becomes observably successful when its Flume operation succeeds:

- unbounded `send_async`/`send` normally enqueues immediately;
- bounded sends succeed after capacity is obtained;
- a zero-capacity send succeeds only through rendezvous with a receiving operation;
- `try_send` succeeds only if immediate channel admission/rendezvous is possible;
- failures return `ClientError::RequestChannelFull` or `RequestChannelDisconnected` with the original public `Request` boxed inside.

This is queue admission, not scheduler admission, MQTT state admission, socket writing, flushing, or broker acknowledgement. The client type documentation says so explicitly. Tracked APIs add a second observable milestone: after successful queue admission they return a notice that later resolves at the protocol-specific completion point.

Flume 0.12's pending async send owns the item in an internal waiting hook. Dropping a still-unmatched future removes that hook; if a receiver already took the item, cancellation cannot retract it. Therefore cancellation around rendezvous/channel acceptance is inherently a two-outcome race unless the library adds a higher-level acknowledgement.

### 2.3 Graceful and immediate shutdown

`AsyncClient::disconnect` and `disconnect_with_timeout` only queue a control request. Graceful shutdown becomes active later, when `EventLoop::handle_request` (v4) or `handle_request_internal` (v5) matches `Request::Disconnect` or `Request::DisconnectWithTimeout` and sets `pending_disconnect`.

That envelope can pass blocked QoS 1/2 publishes because the control channel is independently selected and `OutboundScheduler` classifies ready control work separately from flow-controlled publishes. This implements the important invariant that non-PUBLISH traffic is not blocked merely by publish flow control.

Once `pending_disconnect` is set:

- normal/control request admission into the event-loop scheduler stops (`pending_disconnect.is_none()` guards both channel receives and ready-request handling);
- the loop only reads broker/network progress and the immediate-disconnect channel;
- it waits for `MqttState::outbound_requests_drained()`, which represents already protocol-admitted work, not all channel-admitted work;
- after drain, `send_pending_disconnect` writes and flushes MQTT `DISCONNECT`, calls `drop_unprocessed_requests`, and marks `disconnect_complete`;
- on timeout, terminal cleanup also drops unprocessed work and sets `disconnect_complete` (v5 first normalizes/checkpoints connection state; v4 directly clears network state);
- subsequent `poll()` calls return `ConnectionError::RequestsDone`.

`disconnect_now()` uses the unbounded high-priority channel. `poll`/`select` checks it before normal scheduling, and `poll_disconnect_drain` continues to select it during graceful drain. It can therefore supersede graceful draining. Priority is not interruption: connection setup, a currently executing write/flush, already buffered events, or an application that does not poll can delay observation. Once `disconnect_complete` is already true, `poll()` returns before reading any newly queued immediate request.

### 2.4 Requests accepted after the boundary and later discarded

There is no sender-visible lifecycle state. After `pending_disconnect` is established:

- all clones can still send to all three channels;
- unbounded normal/control sends can keep returning success;
- bounded channels accept until full, after which sends wait or `try_*` reports full;
- zero-capacity normal/control sends cannot complete while the event loop is in the graceful drain because it no longer receives those channels;
- immediate sends continue to enter their unbounded channel, although only one observed before terminal completion is useful.

At graceful completion, `drop_unprocessed_requests` clears `pending`, clears the scheduler's `queued` work, and drains both normal/control receivers. Untracked callers have no completion path, so their earlier success is the only observation. Tracked notice senders are merely dropped here, so notices resolve to the generic `Recv` error. `drain_pending_as_failed` can produce the structured `SessionReset` reason, but graceful cleanup does not use it.

Any request that already entered `MqttState` is different: it participates in outbound drain and tracked completion. QoS 0 tracked publishes resolve after a successful network flush; QoS 1 on `PUBACK`; QoS 2 on `PUBCOMP`; subscribe/unsubscribe on their acknowledgements. V5 also tracks authentication exchanges and explicitly fails active auth when client disconnect starts.

### 2.5 Connection loss, replay, closure, and ownership

On ordinary transport/protocol failure, `handle_network_result` calls `clean()` and persists/checkpoints session state. `clean()`:

- extracts protocol-state requests and notices from `MqttState` for replay;
- drains scheduler `queued` work;
- drains both normal and control channel receivers;
- moves replayable work into `pending` (manual inbound `PubAck`/`PubRec` are intentionally not replayed).

The next connection processes `pending` before/alongside new channel work, with replay-aware readiness. V5 additionally reconstructs topic aliases and can reject an unreplayable retained/aliased publish with a structured tracked error. A connection failure during graceful drain clears `pending_disconnect` through `clean`; current behavior can therefore abort graceful shutdown and permit a later reconnect. This is a key reason not to equate “disconnect requested” with an irreversible sender-side close without an explicit API decision.

Dropping all builder-client clones disconnects the three channel sender sets. The event loop returns `RequestsDone` only after normal/control sources are disconnected and empty, internal queues are empty, no graceful disconnect is pending, and protocol outbound state is drained. The dedicated immediate source is checked first; it is not part of the final predicate. Plain-sender channel closure is wholly caller-owned.

### 2.6 V4 versus v5

There is no material admission-topology difference. V5 has larger/more varied requests, tracked `AUTH`, topic-alias replay validation, session-expiry properties, and more involved checkpoint behavior. V5 graceful/immediate disconnect also terminates active authentication state. These affect the set of structured rejection targets and terminal persistence ordering, but not the gate architecture. Any facility should share semantics and naming across both crates while allowing v5-only request classes/results.

## 3. Guarantee levels

| Level | Caller may rely on | Caller may not rely on |
|---|---|---|
| Best-effort precheck | A call that observes a denied snapshot fails before channel waiting and retains its request. | A call that observed open cannot be prevented from queueing after a transition; success is not protocol admission. |
| Atomic snapshot plus class mask | The same fast check can deny publish, control, or tracked classes independently and attach a small reason. | It still does not order the check atomically with channel acceptance. |
| Generation/epoch validation | An envelope stamped under generation N can be rejected after the event loop moves to N+1; stale queued work is distinguishable. | Queue-send success still is not protocol success; blocked sends and cancellation still need a contract. |
| Event-loop-authoritative admission | Only the event loop decides whether an envelope enters MQTT state; tracked rejections can be explicit. | Untracked queue success cannot later become a synchronous error without a response API. It also does not stop post-terminal unbounded queue growth by itself. |
| Permit-based admission | A successfully acquired permit has a precise relationship to quiescence; transition can wait for all permit holders. | Unless carefully defined, a permit does not imply channel or protocol admission. A permit held across bounded/rendezvous wait can delay shutdown indefinitely. |
| Strict linearizable gate | Every request and transition has one exact total order at a defined commit point. | Existing third-party channel sends cannot be placed inside that order without locks/reservations or a custom queue; it is substantially more machinery than current semantics require. |
| Channel retirement/replacement | Dropping/closing receivers wakes blocked sends and preserves unsent ownership through send errors; old senders cannot target the new channel. | Existing clones do not automatically acquire replacement channels; reversible pause/resume becomes handle invalidation or channel indirection. |
| Mailbox-owned gate | Routing, precheck, channel backend, and error mapping have one reusable internal seam. | A façade alone provides no stronger ordering; its contract depends on its policy and event-loop cooperation. |
| Capability/generation token | A handle tied to generation N can be made permanently stale after migration/takeover. | Long-lived clones no longer transparently survive reconnect/reconfiguration; distributing fresh capabilities becomes an API/lifecycle problem. |
| Sequenced cutoff | An atomic sequence number can label requests and establish a numeric cutoff without a mutex. | `fetch_add` writes a shared cache line on every request; missing/blocked sequence holders create holes, and numeric order still is not channel acceptance order. |

“Atomic admission” is therefore not binary. The useful near-term combination is best-effort sender rejection plus event-loop authority. It deliberately defines protocol admission—not client method return—as the shutdown drain boundary.

## 4. Candidate architectures

### 4.1 Comparative summary

| Candidate | Per-request hot cost | Producer/cache behavior | Bounded/zero-capacity and cancellation | Ownership/results | Reuse and backend neutrality | Complexity/hazards |
|---|---|---|---|---|---|---|
| Shared atomic lifecycle | One load and branch; no envelope growth | Read-shared cache line; transition invalidates producers once | Precheck does not wake an already waiting send | Immediate rejection can return `Request`; raced untracked success remains | Easy masks/reasons; independent of Flume/Kanal | Phase encoding/orderings can drift from event-loop state |
| Atomic plus envelope epoch | One load/branch; +8 bytes measured per envelope | Read-shared unless using per-request `fetch_add` | Event loop can reject stale completed sends; cannot retract a pending send | Strong tracked reason; untracked still only had queue success | Good for pause/reconfigure/reconnect generations | Wraparound policy, replay stamping, internal/trusted envelopes, and validation points add tests |
| Mutex gate | Lock/unlock every call; measured uncontended primitive ~10–12 ns | Serializes producers and bounces an exclusively owned cache line | Releasing before send leaves the race; holding across send/rendezvous risks deadlock | Easy synchronous rejection before send | Lock API is backend-neutral, behavior is not safe across waits | Temptation to extend critical section; poisoning/runtime blocking concerns |
| Permit/reservation | At least shared increment/decrement or semaphore operations per request | Writes shared counters; contention grows with clones | Correct cancellation needs RAII; permits across full/zero channels can block transition; transfer-to-envelope complicates it | Can account for every admitted request and reject precisely | Reservations are often backend-specific | Most state-machine and cancellation complexity short of a custom queue |
| Event-loop-only policy | No sender hot cost; event loop checks each envelope | No producer policy contention | Receiver can keep draining/rejecting to free bounded/rendezvous senders | Excellent tracked results; cannot synchronously reject untracked calls or stop unbounded growth | Backend-neutral after receive | Weak user feedback; terminal event loop must still retire sources |
| Managed mailbox façade | One `Arc` dereference plus whatever chosen precheck costs | Can reduce clone refcount operations from three to one | Can centralize closure/error mapping, but cannot cancel a backend send generically | Best place to preserve original request on early/channel rejection | Strong Flume→Kanal seam | Must not let event loop retain a strong sender owner, or `RequestsDone` breaks |
| Close/replace channels | No open-path policy check if lifecycle is represented solely by closure | No shared policy line | Closure is excellent for terminal wake-up, including rendezvous; replacement makes old clones stale | Send error returns unsent item; already enqueued items still need rejection | Common channel concept, exact APIs differ | Reversible states and current reconnect-after-failed-graceful behavior are awkward |
| Capability token | One compare/load; token stored per handle | Mostly read-only | Stale handles fail before waits; in-flight send race remains | Clear stale-generation error | Useful for takeover/migration | Surprising clone invalidation; needs a fresh-handle issuance mechanism |
| Custom mailbox queue / strict gate | Can be optimized but every request enters custom synchronization | Depends on implementation; likely serialization or complex lock-free code | Can define exact reservation, cancellation, and close semantics | Can provide strongest ownership and results | Hides channel backend entirely | Reimplements mature channel behavior and greatly expands maintenance/test surface |

### 4.2 Candidate-specific interactions

**Shared atomic lifecycle.** A packed `AtomicU64` can encode phase, denied class bits, a compact reason, and spare generation bits. Open-path loads should be `Acquire` only if transition publication protects associated data; if the word contains all decision data, `Relaxed` may be sufficient, but that should be proven rather than guessed. Transition is rare. Immediate disconnect bypasses it. Replay is event-loop-internal. Rich strings or arbitrary policy cannot live in the word; use stable enum codes. A standalone `Arc<AtomicU64>` added to today's enum grows every handle from 24 to 32 bytes.

**Epoch validation.** Stamping `RequestEnvelope` makes event-loop decisions robust across reversible phases and stale queued sends. It increases every managed envelope from 96 to 104 bytes in v4 and 240 to 248 bytes in v5 on the measured x86-64 build. It also requires deciding whether restored replay, reconnect replay, manual acknowledgements, and immediate disconnect are stamped, trusted, or translated. It is justified when the first reversible transition needs a firm stale-work rule, not merely because spare bits exist in the atomic.

**Mutex/strict lock.** A short lock around a precheck is strictly worse than an atomic precheck: it has the same send race and serializes producers. Holding it through `send`, `send_async`, or a zero-capacity rendezvous violates the stated constraint and can deadlock shutdown, especially if the event loop needs the same gate to transition. A lock can be safe only inside a custom mailbox for a short, nonblocking reservation/enqueue operation, which is a different architecture.

**Permits.** Permits are appropriate if a future quiescence contract explicitly promises “the transition waits for every request whose permit was acquired.” That is not current graceful shutdown, which intentionally does not wait for unsent flow-controlled publishes. Permit acquisition/release would add atomic writes to every request; cancellation after the channel takes an item also requires transferring the permit into the envelope. A leaked or long-held permit can prevent shutdown. Do not add this without that stronger contract.

**Event-loop-only policy.** This naturally respects protocol ownership and can reject by request class. During pause/closing it can receive solely to reject, preserving network progress. But without a sender snapshot, unbounded producers can allocate indefinitely and all untracked methods still return success. It is a useful half of the recommendation, not sufficient alone.

**Mailbox façade.** `ManagedMailboxTx` should be a sender-side object only. If the event loop holds the same strong `Arc`, its three senders remain alive and `RequestsDone` no longer follows dropping all clients. Either give the event loop a `Weak<ManagedMailboxTx>` for snapshot publication or split the snapshot into its own `Arc`; the former avoids a second allocation and is safe because failure to upgrade means there are no managed producers left. The event loop keeps its own authoritative phase regardless. The façade must preserve separate senders; it must not merge publish and control queues.

**Closure/replacement.** Terminal receiver retirement is complementary and recommended. It wakes sends that passed an earlier open precheck but are blocked on full/zero-capacity channels. Re-reading the terminal snapshot when mapping `SendError` can turn that error into a structured admission rejection; if the snapshot is not terminal, preserve `RequestChannelDisconnected`. Closing at the moment graceful drain begins is not recommended because current connection failure can abort graceful drain and reconnect. Replacement is unnecessary until a real takeover API intentionally invalidates old clones.

**Capability tokens.** These are attractive for migration/takeover where old handles must never act in the new ownership generation. They are too strong for ordinary reconnect, where current clones remain useful. Keep this as a separate future API, not the default client gate.

**Runtime-neutral / `no_std + alloc`.** The policy word, enums, and envelope rejection logic can live in an allocation-capable core using `core::sync::atomic`. Flume/Kanal/Tokio-specific send and notice adaptation should remain at the client/runtime edge. Mutexes, Tokio semaphores, notifications, and async cancellation machinery would make such a split harder. The current crates themselves are not `no_std`; this is an architectural compatibility advantage, not a present requirement.

## 5. Performance and memory analysis

Measurements were taken on 2026-07-19 with rustc 1.96.1, x86-64 Linux, release optimization for the timing probe. The probes were temporary and removed; production behavior and tracked files were restored before writing this report.

### 5.1 Type sizes

| Type/layout | V4 | V5 | Notes |
|---|---:|---:|---|
| `flume::Sender<Request>` | 8 B | 8 B | One `Arc`-sized sender handle |
| current private `RequestSender` | 24 B | 24 B | Managed variant is three senders; enum uses niche/layout optimization |
| current `AsyncClient` | 24 B | 24 B | One `RequestSender` field |
| current `Client` | 24 B | 24 B | One `AsyncClient` field |
| `Request` | 72 B | 216 B | V5 properties dominate |
| current `RequestEnvelope` | 96 B | 240 B | Request + optional notice + replay flag/padding |
| representative envelope + `u64 epoch` | 104 B | 248 B | +8 B on both crates |
| current sender enum + `Arc<AtomicU64>` in managed variant | 32 B | 32 B | Would make public handles 32 B |
| representative `ManagedMailboxTx` allocation | 32 B | 32 B | Three senders + `AtomicU64` |
| `Plain(Sender) | Managed(Arc<Mailbox>)` enum | 16 B | 16 B | Representative proposed handle payload |

The mailbox layout therefore reduces public handle size in this representative implementation (24→16 B) and moves 32 bytes into one shared allocation. This is not “free”: it adds allocation/lifetime structure and a managed-send indirection, while benefiting clone-heavy usage.

### 5.2 Microprobe results

- Repeated acquire atomic load: about 0.22–0.25 ns in a tight single-thread loop. This is a lower-bound primitive measurement and likely benefits from an L1-resident unchanged cache line; it is not a publish-method latency claim.
- Repeated uncontended `std::sync::Mutex` lock/read/unlock: about 10–12 ns.
- Clone/drop three Flume senders: about 47–51 ns per iteration; clone/drop one mailbox `Arc`: about 10.4–10.6 ns.
- Single-thread alternating `try_send`/`try_recv` on unbounded and bounded(1) channels measured about 20–24 ns per round trip. Adding one acquire load produced no stable difference larger than run-to-run noise.
- One- and four-producer send/receive tests were scheduler-noisy. Unbounded results were roughly 94–185 ns/message, bounded(64) roughly 74–308 ns/message, and zero-capacity roughly 0.77–1.73 μs/message. Paired raw/gated results changed sign between runs. They do not establish a throughput percentage.

The defensible conclusion is limited: an uncontended atomic read is much cheaper than a mutex in isolation; the mailbox materially reduces clone refcount work; and the provisional channel test could not resolve the gate overhead under concurrency. Before production, Criterion-style repeated distributions, CPU pinning, and the maintained end-to-end benchmark harness are required.

### 5.3 Cost placement

- **Every managed normal/control request:** enum/Arc access, class selection already present, plus one atomic load and branch under the recommendation.
- **Every tracked request:** the same gate cost plus the existing oneshot allocation/state; structured queued rejection adds only rejection-path work.
- **Lifecycle transitions:** one atomic store/CAS, receiver drain/rejection, possible channel retirement, and instrumentation; rare.
- **Immediate disconnect:** no admission load; only the mailbox routing indirection and existing channel operation.
- **Replay:** no sender check; event-loop policy/replay validation only.
- **Plain-sender clients:** no new cost or behavior.

## 6. Race and correctness matrix

Legend: **H** = recommended hybrid (mailbox atomic precheck + event-loop authority + terminal retirement), **E** = H plus stamped epochs, **P** = permits, **L** = strict custom/locked linearization, **O** = event-loop-only, **C** = channel replacement/closure as the primary model.

| Scenario | H | E | P | L | O | C |
|---|---|---|---|---|---|---|
| 1. Reads open as graceful becomes closing | May queue; event loop rejects before protocol. Acceptable residual race. | Stale stamp makes rejection explicit. | Ordered by permit cutoff if specified. | Exactly ordered. | Queues, then rejects. | Old channel may still accept until closed. |
| 2. Bounded send waiting at transition | Continues waiting during drain; terminal receiver retirement wakes it with request. If graceful aborts, it may later queue and is subject to authority. | Same, with stale rejection after reopen. | Permit can delay transition or be transferred; hazardous. | Lock across wait is incompatible; custom reservation required. | Event loop must keep receiving/rejecting or terminal-close. | Closure wakes it cleanly. |
| 3. Zero-capacity rendezvous at transition | Same as bounded; no lock. Terminal close is essential. | Stamp resolves post-rendezvous generation. | Outstanding permit can block forever if receiver stops. | Custom rendezvous required. | Must receive/reject or close. | Closure handles unmatched sender. |
| 4. Send future cancelled after partial admission | Existing channel rule: unmatched item drops; already-taken item proceeds. Gate does not promise retraction. | Event loop can reject taken stale item. | RAII plus transfer protocol required. | Custom future can define semantics. | Same existing ambiguity. | Closure does not retract already-taken item. |
| 5. Clones submit while another disconnects | Open observations may race; no producer serialization. | Same plus stale discrimination. | Counter contention; cutoff can wait. | Producers serialize. | All queue; event loop decides. | Timing depends on channel close. |
| 6. Event loop sees disconnect with channel backlog | Only protocol-admitted state drains; backlog is explicitly rejected/dropped. | Can distinguish generations if needed. | Must define whether permits imply drain. | Exact ordering possible at high cost. | Natural authority, but sender feedback weak. | Closing discards/returns only not-yet-enqueued work. |
| 7. Immediate races graceful | Immediate bypasses H and remains prioritized; terminal outcome wins. | Same. | Must be permit-exempt. | Must reserve a bypass lane. | Existing behavior. | Separate immediate channel retained until terminal. |
| 8. Receiver disappears | Send error returns request; map as disconnected unless a published terminal reason applies. | Same. | Permit drops on error. | Custom handling. | Sender sees ordinary disconnect. | This is the mechanism. |
| 9. Reconnect/replay changes generation | H keeps current behavior; no generation promise. Closing abort must restore/open policy deliberately. | Strong stale-envelope rejection; best model if this becomes required. | Requires permit generation rules. | Can order it. | Event loop can apply policy but cannot identify stale queue items. | Replacement invalidates all old handles. |
| 10. Queue-admitted then protocol-rejected | Explicitly allowed; tracked result/instrumentation reports it. | Same with reason tied to epoch. | Permit must not be described as protocol acceptance. | Can avoid only if commit point is protocol state, impractical across async I/O. | Core strength. | Still possible for already queued items. |
| 11. Tracked request needs terminal error | Reject envelope through typed notice error. Recoverable/observable. | Same, richer stale reason. | Same if permit transferred. | Same. | Fully supported. | Queued notices need explicit drain before drop. |
| 12. Untracked rejected after public success | Possible in race; documented and instrumented, not synchronously recoverable. | Still possible; epoch only explains it internally. | Avoidable only if success awaits authoritative ack. | Avoidable with a changed commit/API. | Always possible. | Already-enqueued work has same issue. |
| 13. Pause then resume | Snapshot rejects calls that observe pause; event loop must reject/drain raced queue before acknowledging resume. | Cleaner: old stamps cannot leak across resume. | Strong quiescence if worth cost. | Strongest. | Unbounded growth unless sender also checks. | Replacement makes old handles stale, poor transparent resume. |
| 14. Stale clone after generation change | Observes current shared state and remains a valid clone; H intentionally offers no capability invalidation. | Cached-token variant can reject it; ordinary shared-snapshot variant cannot. | Tokenized permits can reject. | Can reject if generation is part of lock state. | Cannot reject while current phase open. | Old sender remains disconnected after replacement. |

For H, outcomes 1, 4, 10, and 12 are intentionally not hidden. They follow the retained “success means channel admission” API. A stronger outcome requires either stamped epochs for diagnosis, tracked completion, or a new authoritative submission API.

## 7. Reusability assessment

### Shutdown-only flag

A single `AtomicBool shutting_down` is the smallest code change but encodes the wrong abstraction boundary. Current connection failure can cancel graceful drain and reconnect, immediate disconnect is a separate class, and credible pause/quiesce requirements are reversible. A boolean also cannot explain rejection or selectively leave control traffic open.

### Small lifecycle gate

This is justified. The minimum useful model is:

- phases: `Open`, `Quiescing`/`Closing`, and `Terminal` (names should reflect the eventual API contract);
- denied class bits: at least normal publish and normal control; optionally tracked as an orthogonal bit only when there is a concrete use;
- compact rejection reason enum;
- spare generation bits in the snapshot, without initially stamping every envelope;
- irreversible `Terminal`, while nonterminal denied states may reopen under event-loop control;
- sender observation plus event-loop authority;
- the same synchronous, async, and try-send precheck semantics.

“Draining” should remain event-loop protocol state, not a sender promise that every queued request will complete. Immediate disconnect and internal replay are explicit bypass capabilities.

### General policy engine

Not justified. There is no evidence for dynamic predicates, callbacks, per-topic admission, user-injected policy, priority weights, or arbitrary resource accounting. Such a design would put indirect calls, locks, allocation, or large enums on the publish path and make behavior difficult to reason about across both protocol versions.

### Mailbox abstraction

A **small sender-side mailbox façade** is justified; a custom mailbox queue is not. The code already repeats routing and error mapping across async/blocking/try and tracked/untracked methods, and a Flume→Kanal change is contemplated. Centralizing `send(class, envelope)`, `try_send`, terminal error mapping, and immediate bypass creates a stable seam. It must remain a thin owner of the existing three channel senders, not absorb scheduling, replay, MQTT state, or a general policy language.

## 8. Public API and compatibility implications

Useful first-stage behavior can mostly be internal, but structured rejection has concrete public value.

- Keep successful untracked methods defined as channel admission only. Changing them to await event-loop authority would alter latency, cancellation, backpressure, and deadlock behavior and is a breaking semantic change.
- Add a non-exhaustive public `AdmissionRejection` (or narrowly named equivalent) with stable reasons such as `Closing`, `Terminal`, and later `Paused`/`Reconfiguring` only when used.
- Add `ClientError::AdmissionRejected { reason, request: Box<Request> }`. `ClientError` is already non-exhaustive in both crates, so adding a variant is intended to be source-compatible for downstream exhaustive matching, but it is still observable behavior and requires changelog documentation.
- Add corresponding typed tracked-notice error variants carrying the same reason. The notice error enums are non-exhaustive. This concretely distinguishes “the event loop disappeared” from “the event loop deliberately rejected this request.”
- Do not return the internal envelope. Pre-send and channel-send errors must retain and return the public `Request`; internal notice senders are implementation details.
- Untracked requests already accepted into a channel cannot receive asynchronous structured rejection without a new response handle. Do not retrofit one into existing methods.
- If stronger users need protocol-authoritative submission later, add a separate tracked/admitted API rather than changing `publish()` success semantics.
- Plain-sender constructors should not participate automatically. Their receiver and lifecycle are caller-owned. Document that `AdmissionRejected` is a builder-managed-client behavior. A future explicit constructor taking a mailbox/policy would be a separate API.
- Apply the same type/variant names and core semantics in `rumqttc-v4-next` and `rumqttc-v5-next`; v5 additionally maps rejection into tracked auth notices.

## 9. Recommended architecture

### 9.1 Precise contract

1. The event loop is the sole authority for entry into MQTT protocol state.
2. A managed client call first reads a shared admission snapshot for its request class.
3. If denied at that observation, the call does not wait on a channel and returns `AdmissionRejected` with the original request.
4. If allowed, the existing channel operation runs unchanged. Success continues to mean channel admission only.
5. A concurrent lifecycle transition may occur after the observation. Such a request may still be channel-admitted; the event loop will not protocol-admit it under the denied phase. A tracked request receives a structured rejection. An untracked caller may already have returned success; this is an explicitly documented residual race.
6. Graceful closing begins when the event loop processes the disconnect envelope. Only work already in MQTT protocol state is guaranteed to participate in the existing drain. Work merely in `pending`, scheduler queues, or channels is not guaranteed to drain.
7. Immediate disconnect bypasses sender admission policy and retains its separate high-priority channel. At terminal completion all receivers are retired, so even immediate sends subsequently fail by channel disconnection.
8. No locks are held across sends, waits, network I/O, or rendezvous.

It intentionally does **not** provide a global request order, a strict cutoff at `disconnect()` call time, completion of every precheck-open request, retraction after async cancellation, stale-clone invalidation, or a guarantee that untracked queue success will later be reported as rejection.

### 9.2 State and paths

Use an internal shape equivalent to:

```text
RequestSender
├── Plain(Sender<Request>)                         # unchanged
└── Managed(Arc<ManagedMailboxTx>)
    ├── publish_tx: Sender<RequestEnvelope>
    ├── control_tx: Sender<RequestEnvelope>
    ├── immediate_tx: Sender<RequestEnvelope>
    └── admission_snapshot: AtomicU64

EventLoop
├── authoritative AdmissionPolicy
├── Weak<ManagedMailboxTx>                         # publication only; does not keep senders alive
├── publish_rx
├── control_rx
└── immediate_rx
```

The atomic word holds a phase, denied-class mask, reason code, and reserved generation bits. The event loop changes its authoritative policy first and publishes the corresponding snapshot. If `Weak::upgrade` fails, there are no managed senders to inform.

All builder-managed normal/control submission helpers consult the snapshot, including manual acknowledgements and tracked publish/subscribe/unsubscribe/auth. Request class must be explicit at the mailbox boundary rather than repeatedly inferred in ad hoc matches. Graceful disconnect itself is allowed while open and is routed as control. Internal replay does not consult the sender snapshot; it remains event-loop-owned and is evaluated against protocol/replay policy. Immediate disconnect does not consult the snapshot.

### 9.3 Bounded, zero-capacity, and terminal handling

The precheck occurs before `send`, `send_async`, or `try_send`; no gate guard survives into the channel call. A send already waiting when closing begins retains ordinary channel behavior. During a potentially reversible closing/drain phase, the event loop may receive and explicitly reject raced envelopes in bounded batches if needed to prevent avoidable producer blockage, but must not starve network acknowledgements.

At irreversible terminal completion, drain/reject queued envelopes, then drop/retire all three receivers (likely by storing receiver sides in `Option` or an internal mailbox-Rx holder). This wakes blocked bounded and zero-capacity sends. Error mapping rechecks the snapshot: a terminal snapshot maps the recovered request to `AdmissionRejected(Terminal)`; an unrelated missing receiver stays `RequestChannelDisconnected`.

Do not close channels when graceful processing first starts, because current behavior can recover from a connection failure during graceful drain and reconnect. Whether graceful should become irrevocably terminal at observation is a separate public semantic decision.

### 9.4 Rejection and future extension

One `reject_envelope(reason)` path should:

- complete any tracked notice with its typed admission rejection;
- drop untracked work only after incrementing diagnostics/tracing counters by request class/reason;
- preserve replay/session bookkeeping if the envelope had already been represented there;
- never report a deliberately rejected tracked request as generic `Recv`.

This small state model supports terminal shutdown immediately and later supports pause/resume or quiesce-by-class by adding a phase/reason and a tested resume barrier. Add envelope epoch stamps only when such a reversible feature requires stale queued work to be distinguished after reopen. Add permits only if a future API explicitly promises transition completion waits for all acquired submissions.

### 9.5 Estimated overhead and justification

The steady-state managed normal/control cost is one shared atomic load, one branch, and one mailbox indirection. There are no per-request atomic writes and no producer serialization. Representative layout reduces the handle from 24 to 16 bytes and managed clone refcount operations from three to one, while adding one shared 32-byte mailbox allocation. The microprobe did not establish an end-to-end throughput regression; that uncertainty must be resolved by required benchmarks before merge.

The overhead is justified by concrete benefits: immediate ownership-preserving rejection after an observed boundary, bounded unbounded-queue growth, explicit tracked terminal outcomes, centralized channel routing/error semantics, and a backend migration seam. The abstraction is not overgeneralized because it has fixed phases, fixed class bits, fixed reason codes, and no user policies or custom queue implementation.

## 10. Rejected alternatives

- **Shutdown `AtomicBool`:** too narrow for reversible lifecycle and class-specific control; cannot explain rejection.
- **Mutex held through send:** can deadlock or indefinitely delay shutdown on bounded/zero-capacity channels and serializes producers.
- **Mutex only around precheck:** retains the same race as an atomic while costing more.
- **Permit system now:** changes the implied drain contract, adds write contention, and creates cancellation/leak hazards without a demonstrated requirement.
- **Strict linearizable gate:** disproportionate; third-party channel acceptance cannot share its exact commit point without custom reservations/queueing.
- **Event-loop-only rejection:** does not prevent unbounded post-terminal growth or give synchronous ownership-preserving feedback.
- **Channel closure as the entire lifecycle model:** good terminal mechanism, poor reversible/reconnect model, and invalidates old clones on replacement.
- **Epoch on every envelope immediately:** technically sound but costs 8 bytes on every queued request and adds replay rules before a reversible requirement exists.
- **Capability-bound client handles by default:** conflicts with clone persistence across ordinary reconnect and requires fresh-handle distribution.
- **General policy callbacks/trait objects:** no repository evidence justifies per-request dynamic dispatch or arbitrary user policy.
- **Single merged mailbox/channel:** violates the established independence of non-PUBLISH and immediate-disconnect traffic from publish backpressure.

## 11. Incremental implementation path

No production implementation is part of this investigation. If approved later:

1. Add tests that lock down the current three-channel routing, builder/plain split, terminal drops, reconnect-after-failed-graceful behavior, and zero-capacity behavior before refactoring.
2. Introduce internal request-class and structured rejection enums shared semantically by v4/v5; add diagnostics for deliberately rejected envelopes.
3. Refactor managed senders into the thin `ManagedMailboxTx` façade without a gate. Prove `RequestsDone` still occurs when all clients drop and that immediate/control routing is unchanged.
4. Add the compact snapshot and sender prechecks. Keep plain constructors and immediate bypass unchanged.
5. Add the event-loop authoritative policy transition at graceful observation and a single explicit envelope-rejection path. Replace graceful generic notice drops with structured tracked failures.
6. Add terminal receiver retirement and ownership-preserving error mapping for bounded/rendezvous waiters. Verify connection-failure recovery before deciding whether nonterminal channels should ever close.
7. Run the full test/feature matrix and performance gates. Update `CHANGELOG.md` and client/notice documentation for public rejection variants.
8. Only after a real reversible pause/quiesce feature is specified, decide whether its acknowledgement protocol can drain all raced envelopes without stamps. If not, add an epoch to envelopes then.

## 12. Tests and benchmarks required

### Correctness tests (both crates unless v5-only)

- Concurrent clones: one starts graceful shutdown while others submit publish, control, and tracked requests.
- Submission before, during, and after event-loop observation of the barrier, with deterministic hooks rather than timing sleeps.
- Bounded capacities 1 and N; unbounded; zero-capacity with async and blocking senders.
- A send already waiting at transition; terminal receiver retirement returns the original request.
- Cancellation before first send poll, while waiting, after rendezvous, and after queue acceptance.
- Tracked QoS 0/1/2, subscribe, unsubscribe, and v5 auth receive explicit rejection rather than `Recv`.
- Untracked raced success is never protocol-sent; diagnostics count it.
- Immediate disconnect remains independently admissible and wins during graceful drain.
- Immediate send after terminal fails rather than accumulating in an unobserved queue.
- Receiver disappearance unrelated to lifecycle remains `RequestChannelDisconnected`.
- Graceful timeout, successful graceful, immediate override, and network failure during graceful drain.
- Reconnect replay preserves current request/notices and does not accidentally apply sender-only policy to internal replay.
- V5 topic aliases, retained replay rejection, session-expiry override, auth exchange, and checkpoint ordering.
- Dropping every managed clone still allows `RequestsDone`; `EventLoop::new` compatibility-retained senders preserve its current behavior.
- Plain-sender constructors retain one-channel, no-tracking, no-gate semantics.
- If pause/resume is later added: a transition acknowledgement proves all raced old-phase envelopes were rejected before reopen.
- Loom or a small model test for atomic publication/phase transitions if associated non-atomic data is introduced.

### Performance gates

- Criterion benchmarks for request construction + mailbox `try_send` into a drained unbounded channel, raw current layout versus proposed layout.
- Async bounded benchmarks at capacities 0, 1, default, and a large value, separating channel waiting from submission CPU time.
- 1, 2, 4, 8, and 16 cloned producers, pinned where possible; publish-only and mixed publish/control loads.
- Lifecycle transition during load to measure cache-line invalidation and rejection latency.
- Clone/drop benchmark for client handles and allocation count during build.
- Queue memory footprint at fixed envelope counts, especially v5's 240-byte current envelopes.
- Maintained end-to-end `benchmarks` throughput/latency scenarios against a broker, with confidence intervals and multiple runs.
- Equivalent probes for the candidate Kanal version before choosing backend-specific cancellation/closure techniques.
- Define an explicit regression budget before implementation; the temporary probe is not sufficient to set one.

## 13. Open questions

1. Is graceful disconnect intended to become irrevocably terminal when the event loop observes it, or should current reconnect-after-failure behavior remain authoritative? This determines whether channels may close at `Closing` or only at `Terminal`.
2. Should a pre-boundary send still waiting for bounded/rendezvous capacity be considered “admitted before the boundary,” or is channel acceptance the only admission milestone? Current public semantics imply the latter.
3. Is explicit rejection of tracked queued work enough, or is there a concrete need for untracked callers to receive an authoritative post-queue result? The latter requires a new API, not merely a gate.
4. Which request classes are genuinely needed initially: publish/control only, or tracked/manual-ack/auth distinctions too? Avoid bits without a policy use.
5. Should repeated graceful disconnect calls during `Closing` return `Closing`, be idempotently accepted, or be treated as ordinary rejected control requests?
6. What should happen to normal/control work raced into channels if graceful drain aborts and reconnects: reject it, replay it as today, or distinguish it by epoch?
7. Should terminal rejection reasons distinguish graceful completion, graceful timeout, immediate disconnect, event-loop drop, and administrative quiescence, or is a smaller stable enum preferable?
8. Can Flume and the selected Kanal version both support receiver retirement with equivalent ownership and wake-up behavior for sync, async, and zero-capacity sends? This must be prototyped, not assumed.
9. Does the mailbox `Weak` publication design remain clean under future diagnostics/control handles, or is a separate shared gate allocation worth the simpler ownership graph?
10. Is stale-clone invalidation actually required for migration/takeover? If so, it should likely be a separate generation-bound capability API rather than ordinary reconnect behavior.
11. What measurable publish-throughput/latency regression is acceptable in production, and on which target architectures (x86-64, ARM64, embedded Linux)?
12. Should deliberate untracked rejection emit a public event, tracing only, metrics only, or no new signal? A public event can itself affect event-loop ordering and backpressure.
