# Complete DNS SRV Redirect Integration Coverage

## Scope

The MQTT 5 DNS SRV redirect implementation is complete. The remaining work is
to add the integration coverage required to prove that resolved SRV candidates
preserve transport, security, redirect, and session boundaries. Do not change
MQTT v4 behavior, broaden the public API, or rework the resolver and RFC 2782
implementation unless a new test exposes a defect.

Tests must use injected `SrvResolver` callbacks, local Tokio listeners, and
deterministic synchronization. They must not use public DNS, response-order
assumptions, wall-clock randomness, or sleep-based races.

## Remaining work

### 1. Proxy routing uses the resolved candidate

Add MQTT 5 SRV redirect integration tests for the maintained HTTP CONNECT and
SOCKS proxy paths.

- Configure an SRV owner whose selected record has a hostname and non-default
  port distinct from the advertised owner and origin broker.
- Verify that the proxy receives the selected SRV target hostname and record
  port, not the SRV owner, origin endpoint, a transport default, or an eagerly
  resolved IP address.
- Verify that proxy-establishment failure advances to the next SRV candidate
  and that exhaustion produces `RedirectFailure::SrvTargetsExhausted`.
- Keep proxy credentials isolated according to the approved
  `RedirectTargetProfile`; do not infer authorization from the DNS answer.

Cover `http-proxy` and `socks-proxy` independently so each feature remains
testable with `--no-default-features`.

### 2. TLS authenticates the resolved target

Add local TLS integration tests for SRV redirects using every maintained TLS
backend supported by the existing harness (`rustls` and `native-tls`).

- Issue a test certificate for the selected SRV target and verify that the
  redirected connection succeeds when the client dials that target on the
  SRV-supplied port.
- Verify that a certificate valid only for the underscored SRV owner is
  rejected. The selected target must be used for SNI and DNS-ID verification.
- Verify that TLS setup or certificate-verification failure advances to the
  next SRV candidate, while an MQTT-level failure after transport establishment
  does not.
- Assert that diagnostics and structured exhaustion errors do not expose
  credentials.

Use certificates and listeners created by the test harness; do not depend on
external certificate authorities or network services.

### 3. Redirect and session invariants span candidate failover

Add focused event-loop tests for the remaining multi-candidate state
transitions.

- A temporary `UseAnotherServer` SRV redirect that establishes a candidate and
  later disconnects restores the exact origin broker and origin session.
- Authentication data, enhanced-auth state, Client Identifier, session-store
  scope, WebSocket modifiers, and network credentials remain isolated across
  every attempted candidate unless the approved profile explicitly opts in.
- Candidate failover applies redirect isolation once: it must not rerun policy,
  emit another `Event::Redirect`, increment the redirect-hop count, reload the
  origin session, or complete tracked notices more than once.
- Duplicate SRV records and direct/SRV aliases are skipped through normalized
  target-and-port loop identities and cannot bypass loop detection.

Reuse the existing scripted connector and session-store test helpers where
possible. Prefer assertions on observable events, connection attempts, store
operations, and structured errors over assertions on private implementation
details.

## Completion checks

Run the focused tests with each relevant feature combination, then run the
repository acceptance checks:

```bash
cargo fmt --all -- --check
cargo test -p rumqttc-v5-next
cargo check --workspace
cargo check -p rumqttc-v5-next --no-default-features
cargo check -p rumqttc-v5-next --no-default-features \
  --features system-srv-resolver
cargo hack --each-feature --exclude-all-features test \
  -p rumqttc-v4-next -p rumqttc-v5-next
cargo hack clippy --each-feature --exclude-all-features --no-dev-deps \
  -p rumqttc-v4-next -p rumqttc-v5-next
```

Verify the optional dependency boundary remains intact:

```bash
if cargo tree -p rumqttc-v5-next --no-default-features --edges normal \
    | rg -q hickory-resolver; then
    echo "hickory-resolver must be absent without system-srv-resolver" >&2
    exit 1
fi
cargo tree -p rumqttc-v5-next --no-default-features \
  --features system-srv-resolver --edges normal | rg -q hickory-resolver
```

This TODO is complete when the proxy, TLS identity, temporary-redirect,
candidate-isolation, and alias-loop tests above pass across their applicable
feature configurations and the full acceptance commands succeed.
