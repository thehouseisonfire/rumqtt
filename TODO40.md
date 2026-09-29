# C Wrapper Structured Logging Integration

## Goal

Let C applications consume enabled native tracing through a typed logging sink
with explicit filtering, ownership, redaction, and delivery behavior.

## Current foundation and feasibility

The C crate forwards `tracing` and `tracing-log-compat`, but the library does not
install a tracing subscriber or logger and offers no C registration for one.
Rust hosts can configure those facilities themselves. Client events and
diagnostics remain separate interfaces with separate ordering guarantees.

This is feasible with library-scoped tracing dispatch and owned sink adapters.
Do not install or replace a process-global subscriber/logger as a side effect
of client startup or registration, and do not claim lossless packet tracing.

## Implementation requirements

- [ ] Add owned sink/filter configuration and a retained C registration with
  level, target, bounded message/field views, and stable client identifiers.
  Define which fields are structured and which remain backend-specific text.
- [ ] Associate dispatch with driver execution and relevant wrapper operations
  for both protocols. Ensure task polling on a shared runtime retains the right
  dispatcher without changing other clients' or the application's subscriber.
- [ ] Audit enabled native instrumentation and the log-compat path. Scope any
  bridge safely or document unsupported global log sources; never hijack a
  host logger to claim complete tracing coverage.
- [ ] Filter before formatting/copying where possible. Keep credentials, TLS
  key material, authentication data, persistence contents, proxy passwords,
  and sensitive WebSocket headers out of automatic records.
- [ ] Use a bounded delivery contract. Choose prompt inline callbacks or a
  bounded dispatcher queue explicitly; slow logging must not indefinitely stop
  keepalive, QoS, shutdown, or callback cleanup.
- [ ] If records may be dropped under overload, expose drop counts and document
  the policy. Logging loss is not MQTT event loss and must not silently alter
  the application event queue or terminal outcome channel.
- [ ] Avoid holding lifecycle/admission locks during sink calls. Specify callback
  threads, reentrancy, recursive logging suppression, and failure behavior.
  Sink errors must not masquerade as broker/protocol failures.
- [ ] Define copied/borrowed record lifetimes, sink replacement, flush deadlines,
  and exactly-once owner destruction after queued/in-progress records release
  their references. Do not call a released sink during late cleanup.
- [ ] Keep protocol content inspection in explicit event/completion accessors,
  including [TODO29.md](TODO29.md). This sink observes instrumentation; it is
  not a replacement for operation results or a raw packet API.
- [ ] Report optional logging support through capabilities and preserve silent
  operation when no sink is registered.

## Verification and completion

- [ ] Capture representative connection, reconnect, protocol, and shutdown
  records from native C for both protocols with deterministic filters.
- [ ] Verify simultaneous clients/sinks, host subscriber coexistence, disabled
  features, redaction, reentrant logging, slow sinks, and replacement teardown.
- [ ] Demonstrate bounded behavior and visible dropped-record accounting under
  sustained logging without starving MQTT progress.
- [ ] Update C header/exports, README, logging example, `PARITY.md`, and root
  `CHANGELOG.md` with scope, loss, and callback-thread guarantees.

Complete when C can integrate supported tracing into its own logging system
without taking over global process instrumentation or weakening MQTT behavior.
