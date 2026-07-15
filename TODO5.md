# TODO

## Resume Codex Session

```bash
codex resume 019f1f7a-b9f2-71d2-b6e5-9560ab76063c
```

---

## Reusable Evaluation Prompts

### Evaluate a Single Recommendation

What is your assessment of this recommendation?

Determine whether it is technically feasible and whether it would provide legitimate, meaningful value.

Do not assume the recommendation is correct. Identify its benefits, drawbacks, implementation complexity, compatibility risks, and likely maintenance cost.

### Evaluate a Recommendation and Propose Alternatives

What is your assessment of this recommendation?

Determine whether it is technically feasible and whether it would provide legitimate, meaningful value.

Pay particular attention to whether there is a better alternative or counterproposal that would be more beneficial, idiomatic, maintainable, or technically correct in the context of the current codebase.

Do not implement anything yet.

### Evaluate Multiple Recommendations

Assess each of the following recommendations independently.

For each recommendation:

1. Determine whether it is technically feasible.
2. Evaluate whether it would provide meaningful value to crate consumers.
3. Identify implementation complexity, compatibility risks, and maintenance costs.
4. Challenge the proposed solution rather than assuming it is correct.
5. Suggest a more beneficial, idiomatic, maintainable, or technically correct alternative where appropriate.
6. Recommend whether to adopt, modify, defer, or reject it.

Conclude with a ranked roadmap based on consumer value, implementation cost, risk, and suitability before release.

Do not implement anything yet.

---

## Candidate Improvements

### 1. Improve Error Specificity and Messages

**Problem**

Some errors are overly generic or discard actionable context.

Examples include:

* MQTT v5 `ConnectionError::Timeout`
* `ClientError::Request`
* `ClientError::TryRequest`
* WebSocket `ResponseValidation`, whose display message is currently empty

See:

```text
rumqttc-v5/src/eventloop.rs:183
```

Operators may need to distinguish between:

* Request channel full
* Request receiver dropped
* Connection timeout
* Flush timeout
* Broker protocol violation
* Invalid configuration
* WebSocket handshake validation failure
* Graceful-disconnect timeout

**Proposed change**

Improve error structure and display messages.

Possible changes include:

* Adding more specific variants
* Adding structured fields to existing variants
* Preserving the underlying send failure where possible
* Distinguishing timeout phases
* Including relevant protocol or configuration context

Existing enums should remain non-exhaustive.

**Expected value**

Enables:

* Faster debugging
* Better retry policies
* Better decisions about dropping or replaying requests
* More actionable logs and user-facing errors

**Complexity:** Medium
**Risk:** Medium, because downstream users may still pattern-match on existing variants despite the enums being non-exhaustive.

**Validation**

Add:

* Error-display snapshot tests
* Request-channel-full tests
* Request-channel-disconnected tests
* Connection-timeout tests
* Flush-timeout tests
* WebSocket response-validation tests

**Priority:** High value

---

### 2. Add Production Deployment Recipes

**Problem**

The existing README examples are useful, but they are primarily minimal examples.

The crates support several advanced production features whose correct composition may not be obvious to consumers:

* Custom TLS roots
* Mutual TLS and client certificates
* Secure WebSockets
* Custom WebSocket headers
* HTTP or SOCKS proxies
* Persistent sessions
* Reconnection and resubscription
* Bounded request channels
* Backpressure
* Manual acknowledgements
* Broker-specific behavior

**Proposed change**

Add a structured documentation section under:

```text
docs/recipes/
```

Provide copy-pasteable, tested recipes for common production deployments, along with a feature matrix.

Potential targets include:

* Mosquitto
* EMQX
* HiveMQ
* AWS IoT Core
* Corporate proxy environments

**Expected value**

Reduces:

* Integration time
* Configuration mistakes
* Support requests
* Reliance on incomplete external examples

**Complexity:** Low to medium
**Risk:** Minimal

**Validation**

* Compile every recipe in CI.
* Use Dockerized Mosquitto or EMQX smoke tests where practical.
* Validate relevant feature combinations.
* Check documentation links and commands.

**Priority:** High value

---

### 3. Ship a Tested File-Backed Session Store

**Problem**

The `SessionStore` trait provides the required abstraction, but every production consumer must independently implement crash-consistent persistence.

Correct persistent-session storage requires careful handling of:

* Partial writes
* Atomic replacement
* Corrupted checkpoints
* Client-identifier mismatches
* Schema evolution
* Interrupted shutdowns

**Proposed change**

Prefer a small companion crate, such as:

```text
rumqttc-session-store-file-next
```

An optional feature in the main crate is another possibility, but a companion crate would avoid expanding the main crate's dependency and maintenance surface.

The implementation should use atomic temporary-file and rename semantics where supported.

Database-specific adapters should remain outside the main crate.

**Expected value**

Makes restart-safe persistent sessions directly usable without requiring every consumer to design and validate its own storage implementation.

**Complexity:** Medium
**Risk:** Low if implemented as a separate companion crate.

**Validation**

Add crash-oriented tests covering:

* Partial writes
* Interrupted replacement
* Corrupt checkpoints
* Client-identifier mismatch
* Unsupported checkpoint versions
* Recovery after restart

Provide working MQTT v4 and MQTT v5 examples.

**Priority:** High value

---

### 4. Add Optional `tracing` Integration

**Problem**

The crates currently use `log`, while many modern Tokio applications standardize on `tracing` for structured events and spans.

Important MQTT lifecycle events would benefit from structured fields, including:

* Connection attempts
* Successful connections
* Reconnects
* Disconnect causes
* Packet-identifier pressure
* Session restoration
* Replay state
* Protocol violations
* Backpressure

**Proposed change**

Add feature-gated `tracing` integration.

Possible approaches:

1. Emit `tracing` events directly behind an optional feature.
2. Preserve existing `log` compatibility while adding structured tracing events for key lifecycle operations.
3. Use an adapter strategy only if it preserves useful structured fields.

Avoid duplicating every low-level log statement without a clear observability benefit.

**Expected value**

Improves integration with production telemetry, distributed diagnostics, and structured log aggregation.

**Complexity:** Medium
**Risk:** Low if feature-gated and carefully scoped.

**Validation**

* Test the complete feature matrix.
* Capture events using a test subscriber.
* Assert that key lifecycle events and structured fields are emitted.
* Verify that disabling the feature introduces no tracing dependency.

**Priority:** Nice to have

---

## Preliminary Ranked Roadmap

1. Improve error messages and introduce more actionable `ClientError` and `ConnectionError` variants.
2. Add tested production recipes for TLS, WSS, proxies, sessions, reconnection, and backpressure.
3. Publish a file-backed session-store companion crate.
4. Add optional structured `tracing` integration.

The final ranking should be revised after evaluating API stability, implementation overlap, consumer demand, and release scope.
