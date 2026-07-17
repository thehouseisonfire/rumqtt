# TODO

---

## Reusable Evaluation Prompts

### Evaluate a Single Recommendation

What is your assessment of this recommendation?

Determine whether it is technically feasible and whether it would provide legitimate, meaningful value.

Do not assume the recommendation is correct. Identify its benefits, drawbacks, implementation complexity, compatibility risks, and likely maintenance cost.

---

## Candidate Improvements

### 1. Add Production Deployment Recipes

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

### 2. Ship a Tested File-Backed Session Store

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
