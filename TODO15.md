# Remaining Native Wrapper-Core Parity Work

This document tracks only the work still required to close native-client
feature parity in `rumqttc-wrapper-core-next`. Keep the existing public
contracts intact while completing the items below. `TODO16.md` uses the WC
identifiers to track the corresponding C ABI work.

## WC-02: Durable session recovery

Add broker-backed restart tests for both MQTT 3.1.1 and MQTT 5 that prove the
wrapper restores:

- pending SUBSCRIBE and UNSUBSCRIBE operations with their original packet
  identifiers and properties;
- incoming QoS 2 state across the PUBREC/PUBREL/PUBCOMP flow; and
- the complete mixed recovery state in one checkpoint, rather than only
  testing each encoded model in isolation.

Exercise a store future that finishes concurrently with graceful shutdown,
immediate shutdown, and owner abandonment. Assert bounded joining, exactly-once
terminal resolution, safe release of the callback owner, and harmless handling
of a result delivered after cancellation.

## WC-03: Runtime enforcement of packet and inflight limits

Add deterministic broker tests for both protocols that demonstrate the runtime
effect of the exposed controls:

- MQTT 3.1.1 outgoing inflight enforcement;
- MQTT 5 broker Receive Maximum combined with the local outgoing inflight upper
  limit; and
- local incoming and outgoing oversized-packet failures, including the MQTT 5
  distinction between the decoder limit and the Maximum Packet Size advertised
  in CONNECT.

The tests must assert the resulting typed error or completion outcome and prove
that no tracked operation is left unresolved.

## WC-04: Topic-alias reconnect behavior

Add MQTT 5 reconnect tests for automatic and explicit outgoing topic aliases.
They must prove that alias state is scoped to one connection generation,
concrete-topic publishes replay safely, alias-only publishes receive the
documented terminal result when replay is impossible, and negotiated alias
limits are re-applied after every CONNACK.

Verify that connection diagnostics or structured errors retain the available
context when the broker rejects CONNECT properties or negotiated alias use.

## WC-05: Enhanced-authentication lifecycle coverage

Complete deterministic MQTT 5 authentication coverage for:

- a broker changing the authentication method during an exchange;
- overlapping reauthentication attempts;
- reconnect during an initial or reauthentication exchange;
- immediate shutdown while a challenge is pending; and
- explicit callback rejection as distinct from callback panic and timeout.

Extend the SCRAM fixture and captured-tracing checks to prove that usernames,
passwords, nonces, client proofs, server proofs, and authentication data never
appear in `Debug`, display errors, or tracing output.

## WC-06: Redirect and SRV policy coverage

Expand the deterministic redirect fixtures to cover:

- authority-form and absolute `mqtt`, `mqtts`, `ws`, and `wss` references;
- IPv4 and bracketed IPv6 targets;
- WebSocket paths and queries, secure transport transitions, and disabled
  transport features;
- redirects originating from both CONNACK and DISCONNECT;
- malformed and unsupported references;
- SRV priority and weighted selection with multiple records;
- empty answers, lookup failure, loops, and redirect-attempt exhaustion;
- cancellation of an in-flight SRV lookup during shutdown; and
- tracked operations pending when a redirect is accepted or rejected.

For every outcome, assert the source, reason, advertised Server Reference,
selected endpoint, connection generation, and terminal status of affected
operations. Add a test proving that redirect isolation cannot reuse or cross a
configured session-store scope.

## WC-07: Proxy transport composition

Extend the HTTP CONNECT and SOCKS5 fixtures for both protocols to cover:

- broker TCP, TLS, WebSocket, and secure WebSocket connections;
- TLS to an HTTPS proxy independently of broker TLS;
- remote hostname resolution and application-supplied IP targets;
- reconnect through the proxy;
- connection timeout and shutdown during negotiation; and
- authentication rejection and transport failure without credential leakage.

Capture diagnostics, errors, panic output, and tracing output and assert that
proxy credentials and sensitive headers are absent.

## WC-08: Unix-domain socket lifecycle coverage

On every supported Unix CI target, add tests for reconnect, missing paths,
permission failures, connection timeout, graceful shutdown, and immediate
shutdown over Unix-domain sockets. Keep a compile-time or startup-validation
test proving that Unix targets fail before driver startup on unsupported
platforms.

## WC-09: WebSocket transport composition

Add coverage for declarative header edits over WSS and through each supported
proxy composition. Verify the final URI, header ordering, replacement/removal
semantics after reconnect and redirect, and eager failure when WebSocket support
is disabled. Capture errors and tracing output to prove that Authorization,
Cookie, and other sensitive header values remain redacted.

## WC-10: Disconnect validation and close races

Add a direct test that MQTT 5 disconnect options on an MQTT 3.1.1 client fail
before request admission and do not start closing the client. Extend concurrent
close coverage to include graceful-to-immediate escalation and independent
caller timeouts while compatible and conflicting payloads race.

## WC-12: Platform network controls

Add target-specific compile and behavior coverage for local bind addresses,
bind-device selection, and MPTCP. Supported targets must demonstrate that each
value reaches the created socket; unsupported targets must reject the setting
before driver startup. Run tracing and tracing/log-compat independently and
retain captured-output redaction assertions in both configurations.

## WC-13: TLS lifecycle and build-policy coverage

Complete TLS coverage for both protocols and both TLS backends:

- successful platform-root validation where the CI platform permits a
  deterministic fixture;
- malformed PEM identities, PKCS#12 identities, private keys, passwords, and
  ALPN values;
- explicit backend selection when both backends are enabled;
- rejection of a disabled backend before opening a socket;
- independent broker and HTTPS-proxy TLS policies; and
- release and zeroization-sensitive ownership paths after failed start,
  graceful shutdown, immediate shutdown, abandonment, and driver failure.

Captured errors, panic output, and traces must not contain certificate private
keys, identity archives, passwords, authentication data, or sensitive proxy and
WebSocket values.

## Verification and completion

For each completed slice, run at least:

```bash
cargo fmt --manifest-path native-wrappers/Cargo.toml --all --check
cargo check --manifest-path native-wrappers/Cargo.toml --workspace
cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next
```

Feature-sensitive changes must also run the each-feature matrix and the explicit
supported feature combinations used by CI. Broker fixtures must be deterministic
and bounded. Callback and lifecycle tests must cover cancellation, panic
containment, late completion, owner release, and shutdown without hanging.

Remove an item from this file only after its behavior is covered by a stable
test, the relevant parity-matrix row names that test, and any user-visible
change is recorded in `CHANGELOG.md`. This file is complete when it contains no
remaining work.
