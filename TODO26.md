# C Wrapper Dynamic WebSocket Handshakes

## Goal

Let a C application asynchronously prepare or reject each WebSocket upgrade
request, including reconnect attempts. Support refreshed authorization tokens,
request signing, and dynamic request targets through the native request
modifier APIs.

## Current foundation and feasibility

The native `set_request_modifier` and `set_fallible_request_modifier` methods
accept asynchronous modification of `http::Request<()>`. Wrapper-core currently
converts fixed `WebSocketHeader` edits into a ready callback in
`native-wrappers/wrapper-core/src/websocket.rs`.

This addition is feasible using the existing asynchronous foreign-callback
ownership pattern. It does not require exposing Rust HTTP objects to C.
Live changes to the modifier registration are a separate concern in
[TODO35.md](TODO35.md).

## Implementation requirements

- [ ] Add an owned asynchronous handshake authority in wrapper-core and map it
  to the native fallible request modifier for both protocols.
- [ ] Add a retained C registration and request completion. Present HTTP
  method, URI, version, and ordered headers as borrowed views for the call;
  provide owned response builders for deferred changes.
- [ ] Preserve duplicate headers, absence, and intentional empty values.
  Copy response inputs at completion and keep credentials out of debug/error
  output. Apply explicit request-size and header-count bounds.
- [ ] Specify composition with static edits: select and document one order,
  and invoke the dynamic modifier exactly once for each prepared attempt.
- [ ] Permit legal method, URI, and header changes supported by the native
  stack. Validate the final request against WebSocket upgrade requirements.
  Expose the configured dial target and TLS authority separately from the
  request URI. Request edits must not implicitly change transport routing;
  define intentional authority overrides so callers sign the actual request
  while understanding which endpoint and TLS name the connection uses.
- [ ] Run callbacks without wrapper locks and require prompt return. Deferred
  token retrieval is allowed; waiting for MQTT completion from the same driver
  is not. Bound the whole handshake by the connection deadline.
- [ ] Reject duplicate or cancelled responses and release retained owners on
  timeout, reconnect, close, failed start, and abandonment.
- [ ] Define callback rejection as a typed handshake failure. Retry follows
  the connection policy, never a hidden retry inside the callback adapter.
- [ ] Preserve redirect isolation. Origin modifiers must not be reused for a
  redirected authority unless an explicit approved redirect profile permits
  it; coordinate with [TODO31.md](TODO31.md).
- [ ] Make disabled WebSocket support fail eagerly and expose availability
  through the library capability query.

## Verification and completion

- [ ] Use a C fixture whose token changes between reconnects and verify the
  broker-observed headers and signed request target.
- [ ] Cover duplicate headers, malformed requests, rejection, deadline expiry,
  cancellation, retained request data, and credential redaction.
- [ ] Exercise WS and WSS for both protocols and origin/redirect separation.
- [ ] Update `PARITY.md`, C header/exports, README, a reconnect-token example,
  and root `CHANGELOG.md` while preserving existing static-edit behavior.

Complete when dynamic handshake decisions work without rebuilding the client
and without borrowing host memory beyond its declared lifetime.
