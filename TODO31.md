# C Wrapper Application-Controlled MQTT 5 Redirects

## Goal

Let applications approve individual advertised redirect targets and configure
their transport, client identity, authentication, and permitted session or
network-credential reuse. Retain finite attempts, loop detection, and observable
redirect results.

## Current foundation and feasibility

`rumqttc-v5/src/redirect.rs` provides a synchronous policy callback,
`RedirectContext`, and `RedirectTargetProfile` with explicit reuse decisions.
The wrapper maps fixed reject/follow settings to the first compatible isolated
profile in `native-wrappers/wrapper-core/src/backend/redirect.rs`.

A typed synchronous decision callback is feasible now. Deferred decisions are
feasible only after the native client exposes an asynchronous policy boundary;
do not synchronously wait on a foreign completion from the driver thread.

## Implementation requirements

- [ ] Add an owned redirect authority to wrapper-core and a retained C
  registration. Keep the existing fixed reject/follow setters as convenience
  policies with unchanged defaults.
- [ ] Present source, reason, attempt number, and all validated advertised
  references in deterministic order. Provide typed reference accessors and
  copied response/profile inputs; never expose native Rust references.
- [ ] Let the caller reject or select one advertised reference and build the
  native target profile. Validate the choice before state changes; preserve
  native rejection of unadvertised or incompatible targets.
- [ ] Expose fresh/reused/replaced client-ID policy, isolated or explicit scoped
  session reuse, supplied target authentication, and separately approved reuse
  of authentication authority and network credentials.
- [ ] Make origin credential/store/proxy/header isolation the default. Every
  reuse decision must be explicit and map to an actual native profile field;
  do not infer trust from a hostname suffix or a redirect reason.
- [ ] Preserve store leases, origin checkpoints, packet-ID ownership, pending
  operation results, and topic-alias generation semantics when reusing state.
  Audit those paths in the native client before advertising migration parity.
- [ ] Select TLS policy per target using existing profiles and
  [TODO27.md](TODO27.md) extensions when available. Keep SRV resolution and
  weighted candidate selection in the native client.
- [ ] Enforce bounded prompt callbacks, owner retention, reentrancy rules, and
  failure containment. Add async policy support only through an owned future
  with deadlines and safe cancellation in the native event loop first.
- [ ] Preserve native loop detection, total connection deadline, candidate
  exhaustion, and attempt bounds; callbacks must not reset those budgets.
- [ ] Retain complete decision/error observations without leaking credentials.

## Verification and completion

- [ ] Exercise reference selection/rejection, target-specific credentials,
  explicitly approved session migration, and default isolation in native C.
- [ ] Cover CONNACK/DISCONNECT redirects, SRV targets, unsupported transports,
  unadvertised choices, loops, attempt exhaustion, and owner teardown.
- [ ] Verify origin data is never sent to a target lacking reuse approval.
- [ ] Update C header/exports, examples, README, `PARITY.md`, and root
  `CHANGELOG.md`; document any remaining async-policy limitation explicitly.

Complete when C can express the native target-profile decisions without moving
redirect validation or MQTT session reconciliation into the application.
