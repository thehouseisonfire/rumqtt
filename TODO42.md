# C Wrapper Readiness Notifications for Host Event Loops

Status: planned.

## Goal

Let a C application integrate events and operation observations into its own
reactor without periodic polling or a blocking observer thread per client.

## Foundation and scope

Core event consumers, completion cells, and connection observers already have
asynchronous wakeups. C exposes polling and timed waits but no readiness bridge.
Keep MQTT on dedicated or shared managed execution from [TODO33.md](TODO33.md).
This task signals observable work; it does not embed Tokio's I/O reactor or add
caller-driven MQTT pumping.

## Implementation requirements

- [ ] Add an explicitly owned, shareable notification context and bounded source
  registrations. Cover client events and terminal status, MQTT completions,
  pending admissions from [TODO41.md](TODO41.md), and connection observers from
  [TODO46.md](TODO46.md). Define behavior for registration after a result is ready.
- [ ] Use durable readiness state or monotonically changing revisions with
  register/check/arm ordering that cannot lose wakeups. Specify drain/rearm
  semantics and make notifications hints to recheck an authoritative source;
  duplicate/coalesced notifications must be harmless.
- [ ] Choose a portable base contract and explicit platform adapters: a pollable
  descriptor on supported POSIX hosts and a documented waitable Windows object,
  or equivalent supported mechanisms. Report adapter capabilities separately.
  Do not pretend a Windows HANDLE is a POSIX socket descriptor.
- [ ] Keep producer signaling bounded and nonblocking. A saturated OS signal
  primitive must preserve readiness rather than fail MQTT or block its workers.
  Bound registrations and ready-source storage; define an observable overflow
  fallback that still lets callers discover every ready source.
- [ ] Prefer OS wake primitives over arbitrary inline host callbacks. If a host
  callback adapter is added, dispatch away from MQTT workers, define reentrancy
  and teardown, and avoid one delivery thread per client or operation.
- [ ] Support explicit deregistration and retained source lifetimes. Releasing
  a source or client must not close a descriptor still in a host wait set without
  a documented ownership transition. Define terminal wakeups, concurrent waits,
  destruction, and the boundary proving no future callback or signal use.
- [ ] Notifications do not consume events, cancel operations, relax the single
  event-consumer rule, or bypass existing event-backpressure termination. Retained
  results must remain readable independently of client lifetime where supported.
- [ ] Add versioned C APIs and capability discovery without changing existing
  record layouts. Expose borrowed versus duplicated OS resources explicitly.

## Verification and completion

- [ ] Deterministically race readiness publication with registration, checking,
  draining, rearming, deregistration, and shutdown. Cover already-completed sources,
  full signal buffers, repeated wakeups, and terminal-only readiness.
- [ ] Exercise many clients/completions through real POSIX and Windows consumers.
  Demonstrate no lost work, bounded resources, and peer MQTT progress under a
  stalled host reactor. Record actual platform execution separately from wiring.
- [ ] Verify descriptor/handle ownership, unload safety, late signal rejection,
  and callback quiescence if applicable. Run C/C++ header and ABI checks.
- [ ] Document a complete host event-loop example, source/rearm semantics, and
  starvation consequences in the C README and `CHANGELOG.md`; update `PARITY.md`.

Complete when the host can sleep until observations may have changed, drain
them safely, and rearm without missed readiness or owning MQTT scheduling.
