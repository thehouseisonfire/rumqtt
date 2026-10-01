# C Wrapper Dynamic WebSocket Handshakes

## Implemented scope

Implemented with static edits followed by a bounded asynchronous handshake callback.
The API changes path/query and unprotected headers and provides an explicit
authority setter that updates the URI and Host together. GET, HTTP/1.1, scheme
and upgrade fields remain fixed. HTTP authority overrides do not change broker
resolution, TCP/proxy routing, TLS SNI or certificate verification. Redirect
isolation uses the existing approved redirect profile; no TODO31 or TODO35
dependency was introduced. WebSocket dependencies remain upstream; wrapper
diagnostics redact handshake data. Dependency logging is outside this scope.

## Goal

Let a C application asynchronously prepare or reject each WebSocket upgrade
request, including reconnect attempts. Support refreshed authorization tokens,
request signing, and dynamic request targets through the native request
modifier APIs.

## Current foundation and feasibility

The native `set_request_modifier` and `set_fallible_request_modifier` methods
accept asynchronous modification of `http::Request<()>`. Wrapper-core also
converts fixed `WebSocketHeader` edits into a ready callback in
`native-wrappers/wrapper-core/src/websocket.rs`.

This addition is feasible using the existing asynchronous foreign-callback
ownership pattern. It does not require exposing Rust HTTP objects to C.
Live changes to the modifier registration are a separate concern in
[TODO35.md](TODO35.md).

## Implementation requirements

- [x] Add an owned asynchronous handshake authority in wrapper-core and map it
  to the native fallible request modifier for both protocols.
- [x] Add a retained C registration and request completion. Present HTTP
  method, URI, version, and ordered headers as borrowed views for the call;
  provide owned response builders for deferred changes.
- [x] Preserve duplicate headers, absence, and intentional empty values.
  Copy response inputs at completion and keep credentials out of debug/error
  output. Apply explicit request-size and header-count bounds.
- [x] Specify composition with static edits: select and document one order,
  and invoke the dynamic modifier exactly once for each prepared attempt.
- [x] Permit path/query, explicit authority and unprotected header changes, validate the final request,
  and expose configured broker, dial target, TLS authority and native deadline
  separately from the request URI. Keep method and version fixed; synchronize
  authority overrides with Host while preserving the configured connection identity.
- [x] Run callbacks without wrapper locks and require prompt return. Deferred
  token retrieval is allowed; waiting for MQTT completion from the same driver
  is not. Bound the whole handshake by the connection deadline.
- [x] Reject duplicate or cancelled responses and cancel pending work on timeout
  and close. Release client-owned references on teardown and failed start;
  independently retained tokens keep owners alive until their final release.
- [x] Define callback rejection as a typed handshake failure. Retry follows
  the connection policy, never a hidden retry inside the callback adapter.
- [x] Preserve redirect isolation. Origin modifiers must not be reused for a
  redirected authority unless an explicit approved redirect profile permits
  it; coordinate with [TODO31.md](TODO31.md).
- [x] Make disabled WebSocket support fail eagerly and expose availability
  through the library capability query.

## Verification and completion

- [x] Use a C fixture whose token changes between reconnects and verify the
  broker-observed headers and signed request target.
- [x] Cover duplicate headers, malformed requests, rejection, deadline expiry,
  cancellation, retained request data, and credential redaction.
- [x] Exercise WS and WSS for both protocols and origin/redirect separation.
- [x] Update `PARITY.md`, C header/exports, README, a reconnect-token example,
  and root `CHANGELOG.md` while preserving existing static-edit behavior.

Complete when dynamic handshake decisions work without rebuilding the client
and without borrowing host memory beyond its declared lifetime.
