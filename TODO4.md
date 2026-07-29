# MQTT 5 Broker Redirects

## Goal

Handle `Use Another Server` and `Server Moved` responses together with Server
Reference through an explicit, safe application policy.

## Required Work

- Correlate redirect reason codes and Server Reference from both CONNACK and
  DISCONNECT.
- Surface a structured redirect outcome containing the reason, reference, and
  source packet.
- Provide an opt-in policy hook; never change endpoints silently.
- Validate references and supported schemes before attempting a redirect.
- Bound redirect attempts, detect loops, and preserve the original failure when
  a redirect cannot be followed.
- Define TLS server identity, authentication data, Client Identifier, and
  session-checkpoint scope for the redirected endpoint.
- Resolve pending tracked operations consistently when the endpoint or session
  identity changes.

## Tests

- Cover both redirect reason codes from CONNACK and DISCONNECT, with missing,
  malformed, unsupported, and valid Server Reference values.
- Cover disabled policy, accepted redirect, rejected redirect, loops, attempt
  limits, TLS identity selection, and session-state isolation.
- Verify no credentials or checkpoint state cross endpoint boundaries unless
  the configured policy explicitly permits it.

## Completion Criteria

- Redirect responses are never reduced to an unstructured connection failure.
- Automatic redirection occurs only under an explicit bounded policy.
- Targeted tests and `cargo test -p rumqttc-v5-next` pass.
- The public policy and security implications are documented and added to
  `CHANGELOG.md`.
