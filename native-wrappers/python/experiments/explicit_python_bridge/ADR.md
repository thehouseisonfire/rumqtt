# ADR: Reject the Explicit Python Completion Bridge

## Decision

Reject the contender as the production Python wakeup mechanism. Keep `pyo3-async-runtimes` and the
Tokio adapter, and remove the private prototype and its wrapper-core readiness machinery. Retain this
ADR, the comparison report, raw measurements, benchmark harness, and generally applicable
cancellation and lifecycle tests as evidence and regression infrastructure.

The contender demonstrates material thread, RSS, wheel-size, throughput, and shutdown improvements,
but it replaces a maintained executor boundary with a larger host-specific scheduler and lifetime
system. The measured benefit does not make that lifecycle model smaller or sufficiently audited.
TODO19 explicitly rejects a home-grown executor disguised as a bridge and substantial shared-core
machinery whose only demonstrated consumer is dependency removal.

## Decision gate

| Gate | Result | Evidence |
|---|---|---|
| Full advertised CPython/platform behavioral and wheel matrix | Fail | local CPython 3.14 Linux passed; the complete release matrix was not demonstrated |
| Fixed dispatcher threads, never one worker per operation | Pass | one module dispatcher; one existing driver per client |
| No unbounded queue, polling, detached thread, or driver callback into Python | Pass in prototype | bounded channel, coalesced epoch, joinable dispatcher, internal loop callback only |
| Exact public API and cancellation semantics | Pass locally | unchanged unit/integration suite and repeated cancelled-event test pass for v4/v5 |
| Explicit bounded loop-closure cleanup | Fail design review | local process tests pass, but an exhausted teardown budget can return before the dispatcher join completes |
| No extension-side Tokio adapter in contender | Pass | explicit Cargo build excludes `pyo3-async-runtimes` and direct Python-crate Tokio dependency |
| Material reproducible benefit | Pass | 527 fewer threads at one client, lower RSS, smaller wheel, several throughput wins |
| Regressions documented and acceptable | Fail | 2.4x callback-backlog memory, worse event latency, and much larger lifecycle state space |

## Rejection criteria

The prototype did not use per-operation threads, polling, an unbounded queue, daemon threads,
process-global native state, direct driver-to-Python calls, or handwritten unsafe code. It does,
however, introduced roughly 1,200 lines of dispatcher and shared readiness machinery to replace a
407-line adapter and dependency integration. The dispatcher performs executor-like registration,
wakeup, cancellation, deadline, ready-queue, callback, and teardown duties. If the shared teardown
budget expires, the Python cleanup call cannot both remain bounded and guarantee that the dispatcher
join handle is consumed; resolving that needs still more lifetime machinery. This is substantial new
machinery with no current non-Python consumer and therefore meets TODO19's architectural rejection
criterion.

## Follow-up

The production adapter now uses a measured cap of two Tokio blocking workers; the report records the
cap sweep and its selection rule. Wrapper-owned native start/join operations use one reserved slot,
leaving the other worker available for Python result delivery, and shutdown deadlines bound both
slot admission and execution. Immediate shutdown is dispatched before slot admission, so timing out
while waiting to join cannot leave the driver or connection live. Because Python cannot resume a
client after committing it to graceful close, any failure to confirm graceful completion escalates to
the same nonblocking immediate signal. A drop guard arms that escalation before the first blocking-slot
await and disarms only after confirmed graceful completion, making cancellation obey the same
lifecycle invariant. The explicit backend, its readiness API, and its CI lane were removed so rejected
scheduler code cannot rot in the supported tree. Reintroducing it would require a new ADR and complete
release-platform lifecycle evidence.
