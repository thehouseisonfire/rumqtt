# C Wrapper Structured Logging Integration

Status: implemented and validated on Linux.

## Goal

Let C applications consume audited native lifecycle/protocol tracing through an
optional typed logging sink with bounded queued delivery, explicit ownership,
and observable logging teardown, without weakening MQTT progress.

## Starting foundation and feasibility

The C crate already forwarded `tracing` and `tracing-log-compat`, but the library
did not install a tracing subscriber or logger and offered no C registration for
one. Rust hosts could configure those facilities themselves. Both protocols already
emitted structured lifecycle records. Client events, completions, and diagnostic
snapshots remain separate interfaces with separate ordering guarantees.

`tracing-log-compat` enables `tracing/log`: it allows tracing instrumentation to
emit legacy `log` records when no tracing subscriber is active. It does not
automatically turn existing native or dependency `log!` calls into tracing
events for a C sink.

This is feasible with library-scoped tracing dispatch and owned sink adapters.
Do not install or replace a process-global subscriber/logger as a side effect
of client startup or registration, and do not claim lossless packet tracing.

## Initial scope

Support an explicit inventory of audited native lifecycle/protocol events and
selected wrapper instrumentation for both protocols. Include event names and
typed fields even when an event has no message. Specify how selected wrapper
spans become records without exporting a general span tree or every poll's
span entry/exit.

General legacy-log bridging, arbitrary dependency instrumentation, full span
export, and raw packet/history inspection are outside this task. Document
unsupported sources. In particular, dependency WebSocket TRACE output can
contain handshake requests and must not be automatically forwarded. Broader
coverage requires a separate audit and follow-up; complete process logging is
not a completion requirement.

## Implementation requirements

- [x] Add an owned, retained C sink registration with immutable level/target
  filters and size-versioned records. Include event name, level, target, an
  opaque wrapper-generated client identifier, typed scalar fields, and bounded
  optional message/text fields. Do not use MQTT client IDs as automatic log
  identifiers. Define stable fields, extensibility, and explicit truncation
  indicators; backend-specific text is not a stable schema.
- [x] Associate dispatch with driver execution and relevant wrapper operations
  for both protocols. Apply the correct dispatcher on every future poll in
  dedicated and shared execution, including task migration. Audit startup,
  synchronous calls, auxiliary/blocking tasks, cancellation, and late cleanup;
  explicitly identify uncovered paths. Do not leave thread-local guards across
  await points or assume dispatch propagates automatically to spawned work.
- [x] Define host coexistence separately from global-state preservation. State
  whether records within a C sink's scope also reach the ambient host subscriber;
  scoped dispatch alone is not fan-out. Leave unrelated clients/host work alone.
  With no C sink, preserve existing host-controlled tracing/log behavior and
  install no output sink. Never hijack a host logger to extend coverage.
- [x] Filter supported names/targets and levels before formatting/copying where
  possible. Audit field producers and use bounded formatting/storage rather
  than formatting unbounded data and truncating afterward. Keep credentials,
  TLS key material, authentication data, persistence contents, proxy passwords,
  sensitive WebSocket headers, topics/payloads, and broker response properties
  out of automatic records. Redaction must not depend on parsing formatted text.
- [x] Use a bounded dispatcher queue with nonblocking producer admission.
  Bound record count, per-record fields/bytes, aggregate queued bytes, and any
  retained instrumentation state. Run C sink callbacks and owner destructors
  away from MQTT execution workers and admission paths. Bound dispatcher
  resources and avoid adding a logging thread per client. Specify delivery
  ordering and how a blocked sink affects other logging registrations.
- [x] Define overload rejection and shutdown discard policies and expose drop
  counts independently of log delivery. Logging loss is not MQTT event loss;
  leave the application event queue and terminal outcome channel unchanged.
  A saturated queue or slow sink must not make producers wait for logging.
- [x] Avoid holding lifecycle/admission/queue locks during sink calls or owner
  destruction. Specify callback threads, permitted nonblocking reentrancy,
  recursive logging suppression, and sink failure behavior. Suppression must
  cover logging-triggered wrapper calls as well as direct recursive records.
  Sink failures affect logging state/accounting, not broker/protocol outcomes.
- [x] Define transactional ownership transfer, callback-borrowed record views,
  explicit copying, and sink replacement/removal. Already accepted records
  retain their original sink; replacement must not redirect them silently.
  Destroy each owner exactly once after all configuration/client references,
  queued records, and active calls release it. Late cleanup must never use a
  released sink.
- [x] Separate MQTT shutdown from logging flush/quiescence. Define the admission
  boundary covered by flush, finite observation deadlines, and retryable
  teardown observation. A deadline cannot interrupt a blocked C callback or
  destructor: timeout retains required owners and does not report quiescence.
  Provide a way to stop logging admission and observe pending delivery/active
  calls ending. Document releasing remaining owners and completing logging
  teardown before unloading the library or callback code.
- [x] Keep protocol content inspection in explicit event/completion accessors,
  including [TODO29.md](TODO29.md). This sink observes instrumentation; it is
  not a replacement for operation results or a raw packet API.
- [x] Report C sink support through capabilities, distinguish it from native
  instrumentation availability, and define unsupported-feature registration
  behavior. Disabled support and clients without explicit logging placement add no
  logging workers or queues.

## Verification and completion

- [x] Capture representative connection, reconnect, protocol, and shutdown
  records from native C for both protocols with deterministic filters.
- [x] Verify simultaneous clients/sinks, dedicated/shared execution, task
  migration, auxiliary dispatch coverage, host subscriber coexistence, and
  unchanged ambient behavior without a sink. Verify disabled-feature APIs.
- [x] Verify audited coverage, event names without messages, typed fields,
  truncation, pre-format filtering, and omission of private protocol content
  and unsupported dependency logs.
- [x] Demonstrate bounded queue/storage/resources and directly observable drop
  counts under sustained overload without starving keepalive, QoS, reconnect,
  or MQTT shutdown. Use a stalled callback to prove producer independence;
  release it afterward to verify retained ownership and eventual cleanup.
- [x] Verify reentrant calls, recursive logging suppression, sink failures,
  replacement/removal during delivery, queued-record ownership, exactly-once
  destruction, and flush boundaries. Verify teardown timeout/retry, blocked
  destructors, and absence of late callbacks after logging quiescence.
- [x] Update C header/exports, README, logging example, `PARITY.md`, and root
  `CHANGELOG.md` with supported records, filtering, host coexistence, loss,
  callback threads, ownership, and separate MQTT/logging teardown guarantees.

Complete when C can integrate the documented audited tracing into its own
logging system with bounded delivery and observable safe teardown, without
taking over global process instrumentation or weakening MQTT behavior.

Delivery uses one worker and one teardown observer per explicit logging context.
The observer performs the delivery thread's actual join, including sink-created
TLS destruction; timed callers observe completion without joining that worker
directly. Zero-time polling and finite deadlines also cover observation locks.
Flush observation uses published completion progress and notifications without
acquiring or reacquiring the delivery queue mutex. Host TLS owners can finalize
clients after logging routing TLS destruction; missing routing state never
prevents cleanup. Regressions cover host TLS exit and contended flush observation.
Pending and expired C observations publish owned errors through the common ABI
boundary; Boolean try-join still reports pending successfully. Compatibility
capture preserves the broker's raw session flag separately from resumed state.

## Implementation and validation evidence

The [C logging contract](native-wrappers/c/README.md#structured-logging) documents
the supported record inventory, filters, bounds, ownership, callback behavior,
host coexistence, and independent logging teardown. The runnable
[logging example](native-wrappers/c/examples/logging.c) demonstrates the API.
Source-local routing preserves tracing's global callsite registry and legacy-log
fallback; per-poll placement also covers cancellation and explicitly carried TLS
blocking work. Unsupported dependency/legacy sources remain outside capture.
The native `wrapper-tracing` integration feature is enabled by wrapper tracing,
independently of ordinary client `tracing`/`tracing-log-compat`. Standalone Rust
tracing retains direct event macros with no source-routing checks or thread-local
state. Cargo unifies routing for builds that also enable wrapper tracing.

Validation on Linux:

- Default native-wrapper workspace tests and both v4/v5 client suites with
  `tracing-log-compat` passed.
- Focused logging tests cover typed/private/bounded records, stalled delivery
  with MQTT progress, replacement, failure, reentrancy, ownership, migration,
  cancellation, host coexistence, unchanged fallback, and retryable teardown.
- Strict Clippy passed all 35 core/C feature configurations with
  `-D warnings -W clippy::pedantic -W clippy::nursery`.
- Feature tests passed all 35 configurations. The remaining matrix ran serially
  after an existing two-second fairness test timed out during competing builds;
  its isolated rerun and serial feature run passed without changing its deadline.
- The native C suite completed 77 cases with one optional ordered-shutdown
  example skipped; the final enabled logging consumer and example both passed.
- `native-wrappers/c/tests/abi/check.sh all`, documentation generation, Rust
  formatting, and all 221 optional error-output coverage checks passed.

Windows/macOS execution is left to the existing CI platform matrix, which now
also requires the enabled native logging consumer to run successfully.

The isolation follow-up passed all 38 v4/v5 client feature-test and standard
feature-lint configurations. CI now rejects `wrapper-tracing` in standalone
default, tracing, and log-compat production dependency graphs; all three checks
passed locally. The enabled wrapper workspace tests and strict lints, default
workspace build check, and rebuilt native C logging consumer/example also passed.
An additional strict client lint attempt encountered the pre-existing
`must_use_candidate` warning on `SessionRecoveryGate::new`; that API is unchanged.

The teardown/admission review fixes passed all ten logging unit tests, including
callback/owner-created TLS destruction, zero-time polling, finite deadlines,
retryable quiescence, and contended observation locks. Lifecycle capture passed
200 Rust repetitions and 50 native C repetitions. Retries require sufficient
overload accounting for missing records and still require complete coverage
within 16 fresh scenarios. Both default and tracing-enabled wrapper workspace
suites, strict workspace lints, the C logging consumer/example, error-output
coverage, and checked/generated header consistency passed on Linux.
