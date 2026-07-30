# Explore URI-Based MQTT 5 Broker Redirects

## Goal

Determine whether MQTT 5 Server References should support URI forms and whether
that support should enable genuine WebSocket redirects.

## Design Questions

- Which schemes should be recognized (`mqtt`, `mqtts`, `ws`, and `wss`)?
- How should URI references map to the existing `Broker` and `Transport`
  configuration model?
- Should `RedirectTargetProfile` carry a validated `Broker` so WebSocket paths
  and query strings are preserved?
- How should redirect policy prove that its selected broker was derived from an
  advertised Server Reference?
- Which transport choices must remain explicit, especially TLS configuration
  and client credentials for `mqtts` and `wss`?
- How should authority references and URI references normalize to the same
  identity for redirect-loop detection?
- Which URI components should be allowed for each scheme? In particular,
  determine the treatment of WebSocket paths and query strings and reject
  unsafe or irrelevant components such as user information and fragments.
- How should builds without the `websocket` or TLS features report otherwise
  valid URI references?

## Compatibility and Security

- Preserve support for existing authority references (`host`, `host:port`, and
  bracketed IPv6).
- Do not select TLS configuration or reuse credentials solely from an
  advertised scheme.
- Reuse the client's broker/transport compatibility validation rather than
  introducing redirect-only connection semantics.
- Keep unsupported schemes and unavailable transports as structured redirect
  failures.

## Expected Outcome

Produce a concrete API and parsing design, including examples for TCP, TLS, WS,
and WSS redirects. If the design is accepted, define implementation and test
work for URI validation, broker construction, policy selection, loop
normalization, feature-gated behavior, and preservation of WebSocket
path/query data.
