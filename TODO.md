# Add DNS SRV resolution for MQTT 5 Server References

## Goal

Implement DNS SRV resolution for an application-approved MQTT 5 redirect whose
`Server Reference` is an SRV owner name such as
`_mqtt._tcp.cluster.example.com`.

The implementation must:

- query SRV records asynchronously without blocking the event-loop thread;
- follow RFC 2782 priority, weight, target, and port semantics;
- try usable SRV targets in the correct order when target establishment fails;
- keep the advertised reference, resolved endpoint, and authenticated TLS name
  distinct in the data model;
- preserve the existing opt-in redirect policy, attempt bounds, loop detection,
  credential isolation, session isolation, and temporary/permanent redirect
  behavior; and
- remain deterministic under unit and integration tests without requiring live
  public DNS.

This is an MQTT 5 client change. Do not add SRV behavior to `rumqttc-v4-next`.

### Prerequisite and assumed baseline

This TODO assumes that `docs/mqtt-v5-uri-redirect-exploration.md` work has landed, including:

- feature-neutral authority/URI variants in `RedirectReference`;
- fallible `RedirectTargetProfile` construction;
- a profile-owned, provenance-checked `Broker` for immediately materializable
  targets;
- structured `RedirectTargetError` failures;
- reusable broker/transport compatibility validation; and
- normalized TCP, TLS, WS, and WSS redirect identities.

Implement SRV as an extension of that model. Do not revert the URI model,
reintroduce an independently supplied `Broker`, or duplicate its parsing,
transport-validation, credential, and loop-normalization rules. If the landed
names differ from the exploration document, use the actual APIs while
preserving the boundaries above.

## Current limitation

After the prerequisite work, SRV-shaped authority references remain
feature-neutrally parsed for policy inspection, but profile materialization
reports the structured SRV-unavailable target error. Direct authority and URI
profiles already own a complete validated `Broker`; the model has no deferred
target capable of representing an unresolved SRV owner and its eventual set of
resolved candidates.

The ordinary socket connector only performs A/AAAA resolution for an already
known `host:port`. It cannot discover the port, priority, or weight supplied by
an SRV record.

## Required behavior

### 1. Recognize only usable SRV references

Replace the permissive boolean heuristic with a parsed, feature-neutral
classification. An authority is an SRV reference only when it contains:

```text
_<service>._tcp.<non-empty-domain>
```

Matching is ASCII case-insensitive. Normalize DNS names to lowercase and remove
one terminal root dot for comparisons, while sending a fully qualified query
name to the resolver. Require `_tcp`; `_udp` and other protocol labels are not
compatible with MQTT's ordered byte-stream transport.

Do not hard-code only `_mqtt`: MQTT 5 does not prescribe an SRV service label,
and the broker explicitly supplied the complete owner name. Preserve the
service label for lookup and diagnostics.

An SRV authority with an explicit `:port` is ambiguous because the SRV answer
provides the port. Reject it with a specific `RedirectReferenceError` instead
of silently overriding either value.

Keep ordinary underscore-containing hostnames observable as direct authorities
unless they match the complete SRV form above. Add table-driven parser tests for
short names, empty labels, `_udp`, explicit ports, mixed case, and terminal dots.

### 2. Add a resolver abstraction without leaking Hickory types

Add an async, cloneable SRV resolver abstraction in `rumqttc-v5`. Mirror the
existing socket-connector pattern: use an `Arc`-backed callback or small wrapper
whose callback returns a boxed `Send` future. Public APIs and errors must expose
rumqttc-owned types, not `hickory_resolver` records or errors.

The conceptual API should be equivalent to:

```rust,ignore
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SrvRecord {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

#[derive(Clone)]
pub struct SrvResolver(/* Arc<callback> */);

impl SrvResolver {
    pub fn new<F, Fut>(resolver: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Vec<SrvRecord>, SrvLookupError>>
            + Send
            + 'static;
}
```

Provide `MqttOptions::set_srv_resolver(...)`, a matching builder method, and a
getter/clearer consistent with the existing option APIs. This injection point
is required for deterministic tests and applications with a controlled DNS
environment. `Debug` output should report only whether a custom resolver is
configured.

When no custom resolver is configured, use `hickory-resolver`'s Tokio resolver
with the host system resolver configuration and its cache. Start with
`hickory-resolver = "0.26.1"`, disabling unrelated encrypted-DNS and crypto
features. Confirm that the selected features build on every CI platform and do
not raise the workspace Rust 1.85 MSRV; if 0.26.1 is incompatible, use the
newest maintained Hickory release that satisfies the existing MSRV and record
the reason in the dependency declaration or PR description.

Construct and retain the default resolver lazily behind shared state. Do not
create a new resolver for every redirect, bypass its cache, call a synchronous
resolver API, or use `spawn_blocking` around libc as the primary implementation.
Map resolver initialization, timeout, NXDOMAIN, no-data, and transport failures
into a rumqttc-owned `SrvLookupError` while retaining a useful source error.

### 3. Normalize and validate the answer

Convert the complete SRV answer into rumqttc-owned records before selection.
Apply the following rules:

- Parse every returned record; do not select only the resolver's first record.
- A sole record whose target is `.` means the service is explicitly unavailable.
  Return `RedirectFailure::SrvServiceUnavailable` and do not fall back to the
  owner name or a transport default port.
- If `.` appears alongside usable records, ignore the dot record and continue
  with the usable set. Cover this defensive behavior with a test.
- Reject or skip a record with port zero, an invalid/empty target, an IP literal
  target, or a target that is not a valid DNS hostname. RFC 2782 defines Target
  as a domain name with address records; do not reinterpret malformed targets.
- Normalize target case and a terminal root dot for equality and loop keys.
  Preserve a suitable DNS spelling for lookup, proxy routing, SNI, and error
  reporting.
- Bound one answer to `MAX_SRV_TARGETS = 32` usable records. Return a structured
  error rather than truncating silently; this keeps one broker-controlled DNS
  response from creating an unbounded connection plan.
- If no usable records remain, return `RedirectFailure::SrvNoUsableTargets`.

Do not implement an owner-name/default-port fallback after an SRV lookup error,
NXDOMAIN, no-data answer, explicit service-unavailable answer, or exhausted SRV
candidate list. The broker selected SRV semantics by advertising an SRV owner.

### 4. Implement RFC 2782 ordering exactly

Extract selection into a pure helper that accepts the records and an injected
random-number generator. Add a direct `rand = "0.10"` dependency only if the
chosen version satisfies Rust 1.85; do not rely on a transitive dependency.

Ordering must work as follows:

1. Group records by ascending numeric priority.
2. Fully exhaust a lower-numbered priority group before considering the next
   priority group.
3. Within one priority group, repeatedly select without replacement using the
   RFC 2782 weighted algorithm.
4. Put zero-weight records before positive-weight records when constructing the
   running sum, and choose an integer uniformly from the inclusive range
   `0..=sum` as RFC 2782 specifies.
5. When every remaining weight is zero, randomize their order rather than
   preserving DNS response order and concentrating traffic on one target.
6. Use checked or widened arithmetic for the weight sum so 32 `u16` weights
   cannot overflow.

Production ordering may use a thread-local RNG because selection finishes
before any await point. Tests must use a seeded RNG or scripted draws and cover:

- ascending priority and complete priority-group exhaustion;
- selection without replacement;
- all-zero weights;
- a mixture of zero and positive weights, including the inclusive zero draw;
- maximum weights without overflow; and
- statistical smoke coverage showing that a higher weight is selected first
  materially more often, without asserting an exact distribution.

Do not sort by descending weight or attempt all same-priority records
concurrently; either shortcut violates the intended priority/weight behavior.

### 5. Make redirect resolution an explicit event-loop state

Keep `RedirectPolicy` synchronous and keep the observable event ordering:

1. parse the broker's references;
2. let policy approve one advertised reference and profile;
3. yield `Event::Redirect`;
4. on the next `EventLoop::poll()`, resolve an approved SRV reference;
5. install the first selected target and attempt the connection.

Do not perform DNS inside the policy callback, block before returning already
buffered protocol/auth events, or hide resolution in a detached task.

Keep this transition cancellation-safe. If the future returned by `poll()` is
dropped during lookup, leave the redirect unresolved so a later poll can retry;
do not consume candidates, reapply isolation, or restore the origin until a
lookup or connection transition has actually completed.

Extend the post-URI `ActiveRedirect` (or introduce a contained redirect
connection-plan type) so it retains:

- the original `RedirectOutcome` and approved `RedirectTargetProfile`;
- the advertised SRV owner;
- unresolved/resolving/resolved state;
- the ordered remaining `ResolvedSrvTarget` candidates;
- the currently installed candidate;
- previous options and optional origin-session checkpoint; and
- whether the redirect has established an MQTT connection.

Do not put a placeholder SRV owner, port zero, or transport default into the
profile-owned `Broker`. Introduce an internal materialization state such as
`Direct(Broker)` versus `SrvPending(SrvOwner)` while retaining the approved
reference and transport in the profile. Only the redirect parser/profile
factory may create that state, so advertised-target provenance remains
structural.

Split the landed profile-application path into two concerns if needed:

- apply the profile's authentication, network-credential, Client Identifier,
  and session isolation policy exactly once per redirect decision; and
- install or replace only the resolved broker endpoint for each SRV candidate.

Trying a second candidate must not clear state twice, reload the origin session,
increment the redirect-chain count again, rerun application policy, or emit a
second `Event::Redirect` for the same broker decision.

The existing `RedirectPolicy::max_attempts` continues to count broker redirect
decisions/hops. SRV candidate attempts are separately bounded by the 32-record
answer limit. Diagnostics should expose the SRV owner, current candidate index,
candidate count, and current target without exposing credentials.

### 6. Define candidate failover precisely

Try the next ordered SRV candidate only when the current candidate cannot
establish a target connection:

- A/AAAA resolution or custom socket-connector failure;
- TCP connect failure or timeout;
- proxy tunnel establishment failure;
- TLS establishment or certificate-verification failure; or
- WebSocket transport establishment failure only if the landed reference model
  explicitly permits an SRV authority to be paired with WS/WSS. Do not infer a
  WebSocket path or query from an SRV owner.

Once the target returns an MQTT packet, it has been reached. Do not silently
move to another SRV target after CONNACK refusal, enhanced-authentication
failure, malformed MQTT, session reconciliation failure, or a new redirect.
Surface those results through the existing MQTT/redirect paths.

Preserve the internal connection phase from `ConnectFailure` long enough for
`establish_connection()` to distinguish `target_setup`/`transport` failures
from `mqtt_handshake` failures. Do not classify by matching error strings.

When candidates are exhausted, restore the original redirect origin exactly as
the current single-target failure path does and return a structured
`RedirectFailure::SrvTargetsExhausted` containing the owner, number attempted,
and last target-establishment error. Pending tracked notices must still receive
the existing redirected/session-reset outcomes exactly once.

The existing per-attempt `connect_timeout` applies to each candidate. DNS lookup
must also be bounded: add a redirect SRV lookup timeout with a documented
default (use the existing connect timeout initially unless a separate public
setting is demonstrably necessary).

### 7. Preserve endpoint and security identities

Represent these values separately:

```text
advertised reference: _mqtt._tcp.cluster.example.com
selected SRV target:  broker-2.example.net
selected port:        8883
dial authority:       broker-2.example.net:8883
loop identity:        <transport>://broker-2.example.net:8883
```

Use the selected SRV target and port when constructing `Broker::tcp`, invoking
the socket connector, and routing through an HTTP/SOCKS proxy. Do not resolve
the target to an IP address early and thereby bypass custom connector behavior,
proxy-side DNS, IPv4/IPv6 staggering, or `NetworkOptions`.

For the repository's current TLS policy, use the selected SRV target hostname
as TLS SNI and DNS-ID verification input; never use the underscored SRV owner or
an A/AAAA address. Document this local policy in the redirect README. SRV-ID,
DANE/TLSA, and DNSSEC-based authentication are separate features and must not be
claimed by this change.

The resolver result is routing information, not authorization. Preserve all
existing `RedirectTargetProfile::isolated` defaults: do not copy CONNECT or
enhanced-auth credentials, proxy credentials, WebSocket modifiers, Client ID,
or session-store scope unless the application explicitly opted in before
resolution.

### 8. Update loop detection

Do not key an SRV redirect by `owner + transport default port`. Build the visited
key from the selected target, selected SRV port, and selected transport using
`normalized_profile_key()`.

Before dialing a candidate:

- reject it if its normalized endpoint is already in `redirect_visited`;
- skip that candidate and continue with the next ordered candidate;
- add the candidate only when it becomes the active connection attempt; and
- retain visited candidates for the lifetime of the redirect chain.

If all candidates are already visited, return the existing loop classification
or a new structured SRV-loop variant, consistently and with tests. Keep the
non-zero redirect attempt limit because DNS answers can change between queries.

### 9. Error taxonomy

Add structured variants rather than collapsing DNS failures into the post-URI
`RedirectTargetError::SrvUnavailable` or a generic `io::Error`. The final names
may follow local style, but callers must be able to distinguish at least:

- resolver initialization/query failure;
- lookup timeout;
- explicit service unavailable (`Target = "."`);
- malformed or oversized answers;
- no usable targets;
- every candidate already visited; and
- all target-establishment attempts exhausted.

Retain the original `RedirectOutcome`, Server Reference, and source packet in
`RedirectError`. Ensure `Display`, `Error::source`, diagnostics classification,
and optional tracing classification cover every new variant. Mark public error
enums consistently with the repository's existing non-exhaustive policy.

## Implementation sequence

- [ ] Add strict SRV-reference classification and parser tests in
  `rumqttc-v5/src/redirect.rs`.
- [ ] Add rumqttc-owned SRV record/resolver/error types and `MqttOptions`
  configuration APIs.
- [ ] Add the Hickory default resolver with only required features and verify
  the Rust 1.85 MSRV.
- [ ] Add answer normalization, validation, and the 32-target bound.
- [ ] Add the pure RFC 2782 weighted-ordering helper and deterministic tests.
- [ ] Refactor `ActiveRedirect` into an unresolved/resolved candidate plan while
  preserving `Event::Redirect` ordering.
- [ ] Separate one-time redirect isolation from per-candidate endpoint
  installation.
- [ ] Preserve `ConnectFailure` phase and implement transport-only candidate
  failover.
- [ ] Change loop detection to use resolved target/port identities.
- [ ] Add errors, diagnostics, tracing classification, and regression tests.
- [ ] Update `CHANGELOG.md`, `rumqttc-v5/README.md`, redirect API rustdoc, and
  any affected examples.
- [ ] Replace the post-URI `SrvUnavailable` materialization path and tests which
  assert SRV rejection only after every replacement test passes.

## Test plan

### Unit tests

Add tests in `redirect.rs` or a focused `srv.rs` for:

- SRV name classification and normalization;
- record conversion and validation;
- dot-target service unavailability;
- explicit-port rejection;
- answer-size bounds;
- priority/weight ordering with deterministic randomness;
- loop-key normalization for case and terminal dots; and
- custom resolver error preservation.

### Event-loop tests

Use an injected scripted resolver and local Tokio listeners. Cover:

- DNS is not queried before policy approves the SRV reference;
- `Event::Redirect` is emitted before lookup starts;
- the SRV port overrides transport defaults;
- a failed first same-priority target advances to the next ordered target;
- every target in the lowest numeric priority group is attempted before a
  higher numeric priority group;
- a successful target in the lowest numeric priority group prevents backup
  attempts;
- transport failure advances, but CONNACK refusal does not;
- temporary redirects restore the origin after the SRV target disconnects;
- successful `ServerMoved` commits the selected resolved endpoint;
- credentials and session state remain isolated across every candidate;
- shutdown queued before resolution prevents DNS and connection attempts;
- lookup timeout/error and exhausted candidates retain the original outcome;
- repeated targets and direct/SRV aliases cannot bypass loop detection; and
- custom socket connectors and HTTP/SOCKS proxy paths receive the selected
  target hostname and SRV port rather than an eagerly resolved IP address.

For TLS-enabled tests, issue certificates for the SRV target and assert that a
certificate valid only for the underscored owner is rejected. Add equivalent
coverage for every maintained TLS backend where the existing harness permits.

Do not rely on public DNS, response iteration order, wall-clock randomness, or
sleep-based races. Script lookup completion with channels when event ordering
matters.

### Commands

Run while iterating:

```bash
cargo test -p rumqttc-v5-next redirect -- --nocapture
cargo test -p rumqttc-v5-next srv -- --nocapture
cargo test -p rumqttc-v5-next --test reliability -- --nocapture
```

Before completion:

```bash
cargo fmt --all -- --check
cargo test -p rumqttc-v5-next
cargo check --workspace
cargo hack --each-feature --exclude-all-features test \
  -p rumqttc-v4-next -p rumqttc-v5-next
cargo hack clippy --each-feature --exclude-all-features --no-dev-deps \
  -p rumqttc-v4-next -p rumqttc-v5-next
```

Also compile the documented MSRV configuration with Rust 1.85 and test at least
Linux, macOS, and Windows resolver construction in CI. If the dependency cannot
obtain host DNS configuration on a supported target, return a structured setup
error and keep custom-resolver injection available; do not panic or silently
fall back to public resolvers.

## Definition of done

This task is complete when an application-approved SRV Server Reference can
resolve and connect through the normal rumqttc transport path; candidate order
and failover follow RFC 2782; TLS, proxy, loop, redirect, and session identities
remain correct; all failures are structured; tests use no public DNS; the full
feature matrix and MSRV pass; and the README no longer says DNS SRV resolution
is unsupported.

## References

- [MQTT 5.0 section 4.11, Server redirection](https://docs.oasis-open.org/mqtt/mqtt/v5.0/mqtt-v5.0.html#_Toc3901254)
- [RFC 2782, A DNS RR for specifying the location of services](https://www.rfc-editor.org/rfc/rfc2782)
- [RFC 9525, Service Identity in TLS](https://www.rfc-editor.org/rfc/rfc9525)
- [Hickory Resolver 0.26.1 API](https://docs.rs/hickory-resolver/0.26.1/hickory_resolver/struct.Resolver.html)
- `docs/mqtt-v5-uri-redirect-exploration.md` (required implementation baseline)
