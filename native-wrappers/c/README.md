# rumqttc C API

`rumqttc-c-next` builds one C library for MQTT 3.1.1 and MQTT 5. Each
configuration selects exactly one protocol, which cannot change during the
resulting client's lifetime. The wrapper is currently `0.1.0-alpha` and its
native ABI line is 0.1. The package version is available through
`rumqttc_library_version()`; `rumqttc_abi_version()` returns the separately
packed native ABI line (`RUMQTTC_ABI_VERSION`). Neither value claims a mature
ABI 1.0 promise.

The public, checked-in header is [`include/rumqttc.h`](include/rumqttc.h). Build
the shared and static libraries with:

```sh
cargo build --release --manifest-path native-wrappers/Cargo.toml -p rumqttc-c-next
```

`rumqttc_library_capabilities()` reports the features compiled into the loaded
library. Test bits with the `RUMQTTC_CAP_*` constants; ignore unknown bits for
forward compatibility. C Cargo features forward to wrapper-core (`use-rustls`,
`use-rustls-aws-lc`, `use-rustls-ring`, `tls12`,
`use-native-tls`, `websocket`, `http-proxy`, `socks-proxy`, `proxy`,
`system-srv-resolver`, `auth-scram`, `tracing`, and `tracing-log-compat`).
`proxy` enables both HTTP and SOCKS5 proxy support; `tracing-log-compat`
includes `tracing`. The default build enables AWS-LC Rustls and WebSocket.
The library does not install a process-global tracing subscriber or logger.
An embedding Rust host can install its own subscriber before creating clients.
A C-only host that wants Rust trace output can link a small Rust integration
shim that installs the host's chosen subscriber or logger at process startup;
the C ABI does not currently configure or own that process-global sink.
For Ring, build with `--no-default-features --features use-rustls-ring,websocket`.
Ring and AWS-LC cannot be enabled together. The Rust-only wrapper core also
supports `use-rustls-no-provider` for hosts that install a process default
Rustls provider; the C library has no provider installation API. A capability
bit describes the artifact, not a broker
negotiation. `RUMQTTC_CAP_SESSION_STORE_CALLBACKS` reports the available
durable-session callback API.
`RUMQTTC_CAP_AUTH_CALLBACKS` reports the raw MQTT 5 asynchronous
authenticator callback API.

## Custom transports

`RUMQTTC_CAP_TRANSPORT_CALLBACKS` reports the custom stream API, available
independently of optional TLS/proxy/WebSocket features. Initialize a
`rumqttc_transport_vtable_t` with `RUMQTTC_TRANSPORT_VTABLE_INIT`, provide
`connect`, `cancel`, and `destroy`, register it with
`rumqttc_transport_registration_new()`, and attach it with
`rumqttc_config_set_transport_connector()`. The setter retains the owner;
destroy the registration handle when finished configuring. Clear the connector
to restore native socket creation for future clients.

The connect callback receives the protocol, configured client ID, actual dial
target (the proxy endpoint when configured), client-local socket-attempt
generation, registration-wide unique operation ID, requested socket settings,
and remaining native deadline in nanoseconds. This is the remaining budget for
the whole connection attempt, including native negotiation, not a fresh timeout.
Copy request strings needed after the callback. A host owns socket creation and
must apply **all** requested settings or return
`RUMQTTC_TRANSPORT_FAILURE_NETWORK_OPTIONS`. On success report
`RUMQTTC_TRANSPORT_NETWORK_APPLIED`, or `..._NOT_APPLICABLE` only when settings
are default. Buffer presence flags distinguish absent values from explicit zero.

Create a stream with `rumqttc_transport_stream_new()` using that connect
completion and `RUMQTTC_TRANSPORT_STREAM_VTABLE_INIT`. This binds it to the
registration and attempt; it can succeed in one connect response only. Complete
with `RUMQTTC_TRANSPORT_RESPONSE_INIT`, the stream handle, and network handling.
The result retains the stream; destroy its handle when no longer needed.
Failure to register/create a stream leaves its userdata owned by the caller.
Reconnect requires a new stream, including after timeout or cancellation.

`RUMQTTC_TRANSPORT_BASE` supplies bytes before native proxy, TLS, and WebSocket
layers. `RUMQTTC_TRANSPORT_ESTABLISHED` supplies MQTT-ready bytes, including any
host-managed tunnel/security/framing, and requires TCP with no native proxy or
layered redirect profile. The stream mode must match the registration. Custom
connectors cannot use Unix broker targets.

The stream's `perform` callback receives READ, WRITE, FLUSH or SHUTDOWN.
One read may overlap the serialized writer family; read and write buffers are
at most `RUMQTTC_TRANSPORT_MAX_TRANSFER` (16 KiB). A successful read copies
`response.bytes` before returning; empty success is permanent EOF. A successful
write reports `response.count` from 1 through input length. Short transfers are
supported. FLUSH drains host-buffered output; SHUTDOWN closes writing after the
wrapper drains writes and flushes. Return a typed failure for errors. Pending
work retains its completion instead of reporting empty success. A wrapper write
may accept bytes into its bounded buffer before host completion; later writes,
reads or flushes advance pending output and report errors.

The callback completion is borrowed during the callback. For deferred work call
`rumqttc_callback_completion_retain()` and release every retained handle with
`rumqttc_callback_completion_destroy()`. Completion can occur on any thread via
`rumqttc_callback_transport_complete()`. WRITE input remains valid until that
operation's retained tokens are released; all other request views require a
copy. Duplicate, cancelled, and stale results return `RUMQTTC_INVALID_STATE`
before examining response buffers. Malformed transfers are accepted as a
terminal `INVALID_RESULT` failure without reading an oversized read buffer.
Dropping the last host token without completing wakes the observer with
`ABANDONED`.

`cancel(user_data, operation_id)` runs after observation is invalidated, without
wrapper locks. It must promptly stop/wake host work; the connection is discarded.
Cancellation never frees host-retained buffers or owners. Connect/perform/cancel
callbacks must return promptly and be thread safe. A timeout cannot preempt a
blocking callback. Avoid waiting for progress on that callback's driver.
`max_retained_operations` (1–65536, default 256) bounds all live operations across
clients sharing a registration, including cancelled operations retained by the
host. Multiple token clones count as one operation. Exhaustion produces the
terminal `RESOURCE_LIMIT` failure; hosts must release cancelled tokens.

`CONNECT`, `IO`, `TIMEOUT` and `ABANDONED` failures allow reconnection. All other
transport failures stop the driver with `RUMQTTC_EVENT_DRIVER_TERMINATED`, fail
pending operations and first-connection observers with the typed error, and reject
further admission. `rumqttc_event_disconnected()` also extracts this terminal error.
Terminal failures stop MQTT 5 SRV fallback before another candidate is attempted.
Retryable candidate failures can advance to the next endpoint. Failed redirects
are terminal; their errors expose both `rumqttc_error_redirect_failure()` and
the underlying `rumqttc_error_transport_failure()` when present.

Stream userdata and registration userdata each receive exactly one `destroy`
after their last independent owner releases. Destruction can run on any thread
and must not block or throw. A retained completion keeps its stream/registration
alive after client destruction. Unload the library only after all clients,
configs, streams, registrations, and completion tokens have been released.
`rumqttc_error_transport_failure()` returns fixed classifications without host
error strings or credentials.

The [custom transport example](examples/custom_transport.c) uses a portable
worker/socket provider in [transport_socket.c](examples/transport_socket.c).
Run it against a broker as `rumqttc-example-custom_transport HOST PORT`, or
set `RUMQTTC_TEST_BYTE_TUNNEL_PORT` to route through the test byte tunnel. DNS
resolution runs in a worker; cancellation signals its result for discard but
cannot interrupt a platform resolver. A production provider can use its own
asynchronous reactor/resolver instead. Join host workers after destroying
clients/configs, then release remaining stream handles. The native C memory
fixture exercises short deferred I/O, tracked QoS1, reconnect, cancellation,
late-result rejection, timeout/failed construction and final owner release.
The tunnel matrix covers both
protocols with enabled proxy/TLS/WS/WSS combinations and broker trust rejection.

## Durable sessions

Durable sessions use a `rumqttc_store_vtable_t` registered with
`rumqttc_store_registration_new()`, then attached with
`rumqttc_config_set_session_store()`. The setter copies scope and retains the
registration; destroying its handle does not detach existing configurations or
clients. Load distinguishes found, not found, and failure. Save and clear must
atomically replace or remove a whole checkpoint, including after cancellation.
The request identifies protocol, scope, client ID, and checkpoint format
version 1; its views are valid only during the callback. Copy values required
by deferred work. The callback may complete immediately or retain its
completion and finish later on any thread. Release every retained completion.
Duplicate or cancelled completion returns `RUMQTTC_INVALID_STATE`. Calls are
serialized per client and may overlap across clients. One active client per
store identity, protocol, scope, and client ID is enforced in-process;
cross-process exclusion belongs to the store. The configured timeout bounds
each call, and checkpoint limits must be from 8 bytes through 256 MiB. On a
found load, a length above that client's configured limit is rejected before
the callback buffer is read or copied. A successful completion call means its
result was accepted; an oversized checkpoint still fails the client with a
typed persistence error. On a destroy timeout, retry or abandon the client;
callback ownership remains until
driver cleanup and retained completions finish. The vtable's `destroy`
receives unchanged `user_data` once after all owners release it. Unload the
shared library only after all clients, registrations, and retained completions
are released. `destroy` runs on the thread that releases the last owner; it
must not block or throw through the C ABI. Failed registration leaves
`user_data` with the caller. A callback may admit nonblocking client work, but
must not wait for progress on its own driver.

The existing `rumqttc_config_set_transport_tls()` and
`rumqttc_config_set_transport_wss()` always select Rustls. For native TLS,
initialize `rumqttc_tls_options_t` with `RUMQTTC_TLS_OPTIONS_INIT`, set
`backend = RUMQTTC_TLS_BACKEND_NATIVE`, and call the matching
`_with_options()` setter. The setter rejects a backend absent from the loaded
library before connecting. For mutual TLS, attach a
`rumqttc_tls_pkcs12_identity_t` initialized with
`RUMQTTC_TLS_PKCS12_IDENTITY_INIT`; Rustls uses the separate PEM identity
record. `RUMQTTC_TLS_ROOTS_PLATFORM` uses platform trust, while
`RUMQTTC_TLS_ROOTS_PEM` replaces platform roots with the supplied CA PEM.
ALPN identifiers are ordered byte views. All views are copied during the
setter. Wrapper-owned private keys, PKCS#12 data, and passwords are wiped on
release; caller, operating-system, and TLS-library copies have independent
lifetimes. Configure the CMake/pkg-config package for a native-TLS build with
`-DRUMQTTC_NATIVE_TLS=ON`; on Linux this adds OpenSSL to static-consumer
dependencies. The package option must match the Cargo features used to build
the library.

### Owned TLS profiles

Create an immutable `rumqttc_tls_profile_t` using
`rumqttc_tls_profile_new()` and `RUMQTTC_TLS_PROFILE_OPTIONS_INIT`. Its `tls`
pointer supplies the existing backend, roots, identity and ALPN options; `NULL`
selects Rustls with platform roots. Creation copies all inputs and validates
credentials and policy without networking. Platform trust is consulted again
when a client starts. The profile has no mutation API and can be reused across
configurations; profile setters take independent owned copies, so destroy the
profile immediately after applying it if desired. Existing clients retain their
original policy when their source configuration changes.

Apply profiles with `rumqttc_config_set_transport_tls_with_profile()`,
`rumqttc_config_set_transport_wss_with_profile()`,
`rumqttc_config_set_proxy_with_tls_profile()`, or
`rumqttc_config_set_v5_redirect_policy_with_tls_profile()`. The proxy setter
requires HTTPS and `options.tls == NULL`. The redirect setter enables finite,
isolated MQTT 5 redirects with TLS or WSS; the existing redirect setter disables
following. Broker, proxy and redirect profiles are independent, including roots,
pins and client identities. Established custom transports still supply their
own TLS and cannot combine with native TLS profiles.

`RUMQTTC_TLS_ROOTS_PLATFORM_AND_PEM` adds nonempty supplied CA PEM to platform
trust. `RUMQTTC_TLS_ROOTS_PEM` continues to replace it. Malformed supplied roots
and platform trust loading failures remain errors.

`version_policy` selects `DEFAULT`, `12_ONLY`, `13_ONLY`, or `12_OR_13` using
the `RUMQTTC_TLS_VERSION_*` constants. Explicit policies are allowed sets,
never instructions to fall back outside the set. `DEFAULT` preserves backend
defaults. Rustls TLS 1.2-only policy requires the opt-in Cargo feature `tls12`;
without it the combined policy uses TLS 1.3. Native TLS on Apple platforms
supports TLS 1.2-only and the combined set, but rejects TLS 1.3-only. Windows
uses explicit protocol selection; OpenSSL-based builds expose restrictions only
when native-tls has enforceable minimum/maximum bounds. Other native builds
reject explicit policies. Host/provider protocol availability can still cause
construction or handshake failure.

Query `rumqttc_tls_backend_capabilities()` with
`RUMQTTC_TLS_BACKEND_CAPABILITIES_INIT` before selecting policy. The version,
root and pin masks contain bit `1u << selector`; disabled backends return zero
masks. These masks report enforceable policy independently of the library's
provider/build features and the server's negotiated capabilities.

Pins use `RUMQTTC_TLS_PIN_INIT` with a fixed 32-byte SHA-256 digest of either the
complete leaf certificate DER (`LEAF_CERTIFICATE`) or its complete DER
SubjectPublicKeyInfo (`LEAF_SPKI`). Configure up to `RUMQTTC_TLS_MAX_PINS` (32);
zero pins disables pinning. Any one configured pin may match, allowing backup
pins and key rotation. Certificate pins change on renewal; SPKI pins survive
renewal using the same key. Rustls first performs normal chain, validity and
hostname validation, then checks the pins; handshake signatures remain verified.
Pins cannot authorize an otherwise untrusted or expired certificate. Native TLS
rejects pins before networking. Pinned profiles disable TLS resumption so each
reconnect checks the presented certificate. Profiles without pins, callbacks or explicit restrictions preserve resumption
defaults. Raw pins, certificates and identity material are omitted from wrapper
diagnostics.

Use `rumqttc_tls_profile_new_with_extensions()` with
`RUMQTTC_TLS_PROFILE_EXTENSIONS_INIT` for advanced policies. The existing
profile options and capability records retain their layouts. The extension
selects SNI `DEFAULT`, `ENABLED` or `DISABLED` (both backends), resumption
`DEFAULT` or `DISABLED` (Rustls), and an ordered Rustls cipher allowlist of up
to 64 IANA `uint16_t` suite IDs. Zero suites preserves provider defaults;
duplicate/unavailable suites and version sets with no usable suite fail before
networking. Selection clones the provider privately. Disabling SNI preserves
hostname verification and does not override the TLS authority. Native TLS
rejects cipher selection, resumption restriction and callback registrations.

`rumqttc_tls_advanced_capabilities()` reports policy masks and feature flags in
a separate size-versioned record. `rumqttc_tls_supported_cipher_suites()` and
`rumqttc_tls_supported_signature_schemes()` report the selected Rustls provider's
algorithms. Call with `NULL`, capacity zero to obtain the required count; then
supply an array. Insufficient capacity writes no elements and reports the count.
Build support is separate from server negotiation or host key availability.

A verifier registration supplies synchronous additional policy after standard
chain/validity/hostname validation and pins. The request borrows the server
name, layer, leaf-first DER chain, OCSP bytes, verification time and remaining
connection budget. It cannot approve an invalid certificate or pin, and TLS
still verifies the peer's handshake signature. Use
`rumqttc_tls_verifier_registration_new()` and attach `extensions.verifier`.

An external identity registration copies an immutable catalog of certificate
chains, opaque key IDs and preferred IANA signature schemes. Use
`rumqttc_tls_identity_registration_new()` and attach
`extensions.external_identity`; static and external identities are mutually
exclusive. Selection receives issuer hints and compatible schemes; return a
catalog index or `RUMQTTC_TLS_IDENTITY_DECLINE` with `CALLBACK_OK`. Signing
receives the exact **unhashed** TLS message and the selected scheme/key ID.
Write the signature into the wrapper-owned buffer and set its length. RSA-PSS
uses MGF1 with the same digest and digest-length salt; ECDSA uses DER `(r,s)`;
Ed25519 signs the full message. The catalog supports provider-available
RSA-PSS/PKCS#1 SHA-256/384/512, ECDSA P-256/384/521, and Ed25519. The wrapper
checks every returned signature against the selected leaf certificate before
using it. It receives no private key or foreign cryptographic object pointer.

Limits are 32 identities, 32 certificates per chain/request, 1 MiB aggregate
catalog or request metadata, 256 bytes per nonempty key ID, ten schemes per
identity and 4096 signature bytes. Validation invokes no callbacks. Callbacks
return `CALLBACK_OK`, `REJECTED`, `FAILED`, `TIMEOUT` or `TRANSIENT`; other values
become `INVALID_RESPONSE`. `rumqttc_error_tls_callback_failure()` reports typed
verification/selection/signing stage, reason and broker/proxy/redirect layer.
Callback failures are terminal except `TIMEOUT`/`TRANSIENT`, including when a
server allows anonymous TLS. Retry/fallback remains bounded by the connection
policy; an exhausted isolated redirect is still terminal. Terminal failures stop redirect/SRV candidate fallback.
Diagnostics omit callback data, certificate/key IDs and signature bytes.

Successful registration transfers ownership of `data`; failed construction
leaves it with the caller. Registrations, profiles, configurations and in-flight
handshakes retain the owner; `destroy` runs exactly once after the final
reference is released. Callbacks and destructors must be thread-safe and cannot
unwind; calls can overlap across clients sharing a registration. All request
views and signature output buffers are borrowed only for the callback.
Callbacks execute synchronously on the driver thread, return promptly, and may
use nonblocking MQTT admission. Blocking waits for the same driver's progress
are unsupported. There is no deferred completion API or forced preemption:
cancellation and elapsed budgets are processed once a callback returns. The
absolute original connection deadline is checked before and after callbacks
and before exposing the stream. Pins, verifiers and external identities force
resumption off so every reconnect performs authentication again.

[The native EVP example](examples/external_identity.c) takes
`HOST PORT CA_PEM CLIENT_CHAIN_PEM HOST_KEY_PEM [wss]` and supports RSA-PSS
SHA-256 or P-256 ECDSA. Its host opens the key and retains it through the
registration; replace the signing adapter with an HSM integration as needed.
OpenSSL Crypto is optional for examples/consumers and required by the dedicated
signer CI job. Rustls-only production builds have no OpenSSL link dependency.

The warning-clean [TLS profile example](examples/tls_profile.c) takes
`HOST PORT CA_PEM SPKI_SHA256_HEX`, selects TLS 1.3, and destroys its profile
before starting the client. For example:

```sh
rumqttc-example-tls_profile mqtt.example.com 8883 ca.pem "$SPKI_SHA256_HEX"
```

Custom verification callbacks, cipher selection and external signing are not
part of this profile API.

```c
rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
tls.backend = RUMQTTC_TLS_BACKEND_NATIVE;
if ((rumqttc_library_capabilities() & RUMQTTC_CAP_NATIVE_TLS) == 0)
    return 1;
rumqttc_error_t *error = NULL;
rumqttc_status_t status =
    rumqttc_config_set_transport_tls_with_options(config, &tls, &error);
if (status != RUMQTTC_OK) {
    rumqttc_error_destroy(error);
    return 1;
}
```

`rumqttc_error_context()` returns protocol, connection phase and generation,
and delivery status as separate machine-readable outputs; absent values are
zero with a separate presence flag for generation.

For a copied Last Will, initialize `rumqttc_last_will_t` with
`RUMQTTC_LAST_WILL_INIT`, fill topic, payload, QoS, and retain, then call
`rumqttc_config_set_last_will()`. MQTT 5 properties use the separate
`RUMQTTC_V5_WILL_PROPERTIES_INIT` record and the MQTT 5 selector. The setter
copies all views, including ordered user properties. Clear or replace the will
before client start. MQTT 5 CONNECT properties use
`RUMQTTC_V5_CONNECT_PROPERTIES_INIT`; presence flags distinguish absent values
from present empty values. The advertised MQTT 5 Maximum Packet Size is
independent of the local decoder limit, while automatic outgoing topic aliases
use a separate policy setter. Replacing either configuration copies the new
values immediately, so caller buffers can be released before start or restart.

Request batching uses one request per poll when
`rumqttc_config_set_max_request_batch()` is zero. A nonzero value sets the
maximum requests processed together. `rumqttc_config_set_read_batch_size()`
uses adaptive batching at zero, based on the outgoing inflight window; one
limits reads to a single packet per poll. Resetting the MQTT 3.1.1 outgoing
packet limit restores the native default. Clearing the MQTT 5 outgoing
inflight upper limit leaves the broker's Receive Maximum authoritative;
clearing its advertised Maximum Packet Size omits that CONNECT property and
does not replace the separate local decoder limit.

Unix socket paths are native bytes on Unix and fail before start on other
platforms. Declarative WebSocket header edits are copied in order for each
handshake, including reconnects; add preserves duplicates, replace overwrites,
and remove deletes a name. The core rejects protected handshake headers and
redacts all header values in debug output. TCP network setters accept portable
numeric buffer sizes, booleans, and a numeric local socket address. Bind-device
and MPTCP settings fail on platforms that do not support them. Clearing the
local bind address restores automatic local address/port selection; clearing
TCP buffer overrides restores operating-system defaults. TCP_NODELAY can be
disabled independently. The operating system may adjust buffer sizes or tune
default buffers while the connection is active.

For per-attempt token refresh or signing, initialize `rumqttc_websocket_vtable_t`
with `RUMQTTC_WEBSOCKET_VTABLE_INIT`, create a registration, and attach it with
`rumqttc_config_set_websocket_handshake()`. The loaded library must advertise
`RUMQTTC_CAP_WEBSOCKET_CALLBACKS`; disabled builds reject registration eagerly.
The registration owns user data only after successful creation. Configurations,
clients, and retained completion tokens retain it independently. `destroy` runs
exactly once after their final release; it may run on any participating thread.
Clearing a configuration affects future clients, not an already started client.

`prepare` runs after static edits once per prepared WS/WSS attempt, including
reconnects. Its request, header array, and completion handle are borrowed for
the callback only. Copy any request fields needed later and retain the completion
with `rumqttc_callback_completion_retain()` before returning. Return promptly;
defer credential retrieval to a host worker instead of waiting for MQTT work.
Registrations shared by clients must support concurrent callbacks.

Create an owned `rumqttc_websocket_response`, set its path/query or authority,
apply header edits, and finish with `rumqttc_callback_websocket_complete()`.
Setters copy their inputs and completion copies the response; destroy the
builder after completion.
Header values are byte views. Add preserves duplicate values and their per-name
order, replace removes previous values, and remove distinguishes absence from
an intentional empty value. Snapshot names are sorted, without a global wire
ordering guarantee. `rumqttc_websocket_response_set_authority()` accepts
`host[:port]` or `[IPv6][:port]` without user information and updates the URI
authority and `Host` together. Repeated successful calls replace the override;
invalid edits leave it intact. Generic `Host` edits remain prohibited. GET,
HTTP/1.1, scheme, framing, upgrade, and `Sec-WebSocket-*` fields cannot change.
Sign the final authority, path/query, and selected headers as required by your
service. Authority overrides select HTTP routing only: broker resolution, TCP
and proxy destinations, TLS SNI, and certificate verification still use the
configured endpoint. For example, a connection to `gateway.example` can send
`Host: mqtt.customer.example`, while TLS authenticates `gateway.example`.

The absolute connection deadline bounds deferred work. Complete or reject only
once; duplicate and cancelled replies return `RUMQTTC_INVALID_STATE`. Release
every retained token, including late tokens after timeout or close. Releasing
the final unanswered token abandons the decision. A late token retains user data
until it is destroyed, even after the client exits. Callback rejection,
abandonment, and timeout follow the existing reconnect policy. Invalid final
requests, resource limits, and callback panics terminate the driver. Query
`rumqttc_error_websocket_failure()` for the fixed typed detail. Wrapper diagnostics
exclude handshake credentials. Upstream WebSocket dependency TRACE logging can
include outgoing requests; configure those log targets accordingly. Isolated
redirects clear the origin registration and static edits.

Limits are 128 final header values, an 8 KiB path/query, 64 KiB aggregate request
data, 256 response edits, and 64 KiB copied response data. The portable
[`websocket_tokens.c`](examples/websocket_tokens.c) example demonstrates deferred
completion and token refresh on reconnect. The callback is available to C and
wrapper-core; JavaScript and Python bindings do not expose it yet.

Proxy connections use `rumqttc_proxy_options_t` with
`RUMQTTC_PROXY_OPTIONS_INIT`. Select HTTP, HTTPS, or SOCKS5; the loaded library
must have the corresponding proxy capability bit. `RUMQTTC_PROXY_DNS_REMOTE`
resolves broker hostnames at the proxy; no local DNS policy is supported. Set
`credentials_present` before supplying username/password byte views. The
current core requires those bytes to be UTF-8. HTTPS proxies require their own
`rumqttc_tls_options_t`, separate from broker TLS. The setter copies all inputs
and `rumqttc_config_clear_proxy()` removes the proxy configuration. Unsupported
proxy kinds and policies fail before connecting to the broker. Wrapper-owned
credential copies are wiped on release; caller and proxy-library copies have
independent lifetimes.
Proxy credential byte views must contain UTF-8 for the supported core clients.

For MQTT 5 graceful or immediate close with a reason and properties, use
`RUMQTTC_V5_DISCONNECT_PROPERTIES_INIT` inside
`RUMQTTC_DISCONNECT_OPTIONS_INIT`, select MQTT 5, and call the matching
`_with_options_timeout_ms` function. A later close caller must supply matching
options because the first admitted payload wins. The original close functions
remain available for version-neutral close.

`rumqttc_client_try_reauthenticate()` and
`rumqttc_client_reauthenticate_tracked()` admit MQTT 5 reauthentication when
the configured core mechanism supports it. An overlapping request receives a
separate operation ID and completes with `RUMQTTC_AUTH_FAILURE_OVERLAPPING`,
`RUMQTTC_AUTHENTICATION_ERROR`, and `RUMQTTC_DELIVERY_NOT_ADMITTED`. It sends no
AUTH and leaves the active exchange running. Transport loss resolves that
exchange with `ConnectionClosed`; its Failed lifecycle event precedes
Disconnected and the next connection's authentication events.
A build with `RUMQTTC_CAP_SCRAM`
can configure SCRAM-SHA-256 with `rumqttc_config_set_v5_scram()` and clear it
before start. The password is a byte view and must contain UTF-8 for the
underlying SCRAM mechanism; wrapper-owned copies are wiped on release. New
event accessors expose CONNACK
reason, outgoing packet identifier, authentication lifecycle, broker
DISCONNECT fields, and redirect reason, source, reference, and selected target.
Returned string views belong to the event; copy them before destroying it.
Presence flags distinguish absent values from present empty strings. Typed
store, authentication, and redirect failure codes and statuses are available
on errors.
For a raw MQTT 5 mechanism, register `rumqttc_auth_vtable_t` with
`rumqttc_auth_registration_new()` and attach it with
`rumqttc_config_set_v5_authenticator()`. The setter copies the method and
retains the registration. One authority owns the exchange; SCRAM and a raw
callback cannot be configured together. `respond` runs on the client driver
thread with an owned exchange generation, stage, reason code, and borrowed
views of optional broker properties and ordered User Properties. These views
expire when `respond` returns. Complete before returning or retain the opaque
completion and finish on another thread with `rumqttc_callback_auth_complete()`.
The response record can complete, send owned properties, or reject. Exactly one
completion wins; a duplicate, cancelled, or late call returns
`RUMQTTC_INVALID_STATE`. Calls are serialized per client and may overlap across
clients. A callback may admit nonblocking client work but must not wait for
progress from its own driver. The exchange timeout includes deferred work;
close and abandonment cancel pending completions. The optional `failed`
notification has no completion and must return promptly. `destroy(user_data)`
runs once after all clients, registration handles, calls, and retained
completions release the owner. Failed registration leaves `user_data` with the
caller. Unload a shared library only after releasing every retained completion
and owner. Authentication data is never included in wrapper errors or traces.
`rumqttc_event_authentication_details()` and
`RUMQTTC_EVENT_PROPERTIES_AUTHENTICATION` expose broker AUTH reason and
properties as event-owned views; copy values needed after event destruction.
[`examples/authenticator.c`](examples/authenticator.c) shows registration,
shutdown, and exactly-once context cleanup with a local broker fixture.

`rumqttc_config_set_v5_redirect_policy()` selects a fixed reject or bounded
follow policy and an explicit transport for isolated redirect targets. A custom
DNS SRV callback uses `rumqttc_resolver_vtable_t` and
`rumqttc_resolver_registration_new()`, then
`rumqttc_config_set_v5_srv_resolver()`. The request owner name is borrowed for
the callback call; the completion can be retained and completed later with
owned priority, weight, port, and target records. Empty success and query
failure have separate results. Cancellation, ownership, and shared-library
unload follow the store callback rules above. With no custom resolver, the
system resolver is selected only when `RUMQTTC_CAP_SYSTEM_SRV` is present.
`rumqttc_event_redirect_diagnostics()` exposes follow/reject decision, attempt
count and limit, visited endpoints, loop flag, and SRV candidate position. A
fixed reject policy reports `RUMQTTC_REDIRECT_FAILURE_DISABLED`; a detected
loop reports `RUMQTTC_REDIRECT_FAILURE_LOOP` and a reject decision.

Define `RUMQTTC_STATIC` before including the header when linking the static
library on Windows. Static consumers must also link the platform libraries
required by Rust, networking, and the bundled rustls/AWS-LC TLS provider:

| Platform | Additional static-link inputs |
| --- | --- |
| Linux | `pthread`, `dl`, `m` |
| macOS | `pthread`, `m`, `Security`, `CoreFoundation`, `SystemConfiguration` |
| Windows | `ws2_32`, `bcrypt`, `crypt32`, `ncrypt`, `secur32`, `userenv`, `advapi32`, `kernel32`, `ntdll` |

CMake and pkg-config templates are included for release packaging. Native
consumers are built and loaded in CI on Linux x86_64, macOS arm64, and Windows
x86_64; no ABI guarantee is made for an untested target.

While the package version is a SemVer prerelease, CMake consumers must discover
it without a numeric version request and may inspect `rumqttc_VERSION`
afterwards. CMake's package-version request grammar cannot name a SemVer
prerelease, so the generated version file deliberately rejects requests for
the future stable `0.1.0` release.

Release archives use an ABI-line-specific loader identity:

| Platform | Shared-library identity |
| --- | --- |
| Linux x86_64 | `librumqttc.so.0.1` |
| macOS arm64 | `@rpath/librumqttc.0.1.dylib` |
| Windows x86_64 | `rumqttc-0_1.dll` |

## Native validation

From the repository root, run the native suite and the seven installed-package
profiles with a required Mosquitto broker:

```sh
cargo build --manifest-path native-wrappers/Cargo.toml -p rumqttc-c-next
cmake -S native-wrappers/c/tests/native -B native-wrappers/target/rumqttc-c-native
cmake --build native-wrappers/target/rumqttc-c-native --config Release
RUMQTTC_REQUIRE_MOSQUITTO=1 ctest --test-dir native-wrappers/target/rumqttc-c-native -C Release --output-on-failure
RUMQTTC_REQUIRE_MOSQUITTO=1 python3 native-wrappers/c/tests/package_feature_matrix.py --native
```

Set `MOSQUITTO_BIN` if the executable is outside PATH. CI installs Mosquitto on
all three platforms and fails if a supported Will case cannot run. Local runs
without a broker report a skip unless `RUMQTTC_REQUIRE_MOSQUITTO=1` is set.
Package profiles retain command logs, platform/feature metadata, and CTest
results under `native-wrappers/target/c-feature-matrix/`.

The TLS matrix checks platform-root acceptance and untrusted-root rejection
for each enabled backend and both MQTT versions. Linux uses isolated
`SSL_CERT_FILE`/`SSL_CERT_DIR` inputs. macOS and Windows execution requires
`RUMQTTC_DISPOSABLE_TRUST_RUNNER=1` on a disposable runner: macOS temporarily
adds its root certificate to `System.keychain` and an administrator trust entry,
while Windows uses the current user's Root store. Cleanup removes only the
fixture's own certificate, including after child-process failure; it does not
change the macOS keychain search list.
An interrupted run leaves a cleanup manifest; CI unconditionally runs
`python3 native-wrappers/c/tests/native/platform_trust.py --cleanup`.
If macOS trust removal stalls, cleanup imports the current administrator trust
settings with only the fixture's entry removed, preserving unrelated entries.
Removing the final administrator entry can require interactive authorization,
including through an empty import. On disposable runners, cleanup instead
replaces that entry with an unconditional deny, verifies it, and deletes the
certificate. A `.cleanup` audit records the deny metadata retained until the
runner is discarded. Failed verification preserves the cleanup manifest for
retry. CI uploads these records with the native validation evidence.
The latest verified platform results and pending execution are recorded in
[`../wrapper-core/PARITY.md`](../wrapper-core/PARITY.md).

## Compatibility policy

The first published `0.1.0` archive establishes the 0.1 baseline. Every later
`0.1.z` release must contain the complete ABI of the latest earlier 0.1
release. Compatible declared function additions are permitted, so an
application using a new function must run with at least the release that added
it. A new pre-stable minor line may deliberately break ABI and receives a new
loader identity. After 1.0, incompatible changes require a new package major
and native ABI line.

CI derives declarations, canonical function types, typedefs, constants, public
record layouts, exports, and loader identity from the checked header and final
native artifact. Linux, macOS, and Windows each produce a target-specific
contract. Linux also runs the third-party comparator evaluation corpus; it is
not treated as cross-platform evidence. Runtime ownership, timeout, panic,
loading, and package-relocation behavior remain covered by their dedicated
consumer and behavior tests rather than being called structural ABI checks.

Before the first published wrapper release, historical comparison reports
`no published baseline` and only current header/export consistency is enforced.
Afterwards, contributors can reproduce the authenticated comparison without
repository credentials:

```sh
native-wrappers/c/tests/abi/check.sh ffi-header
native-wrappers/c/tests/abi/check.sh exports
native-wrappers/c/tests/abi/compare-release.sh
python3 native-wrappers/c/tests/abi/mutation_matrix.py --output native-wrappers/target/abi-mutations
```

Historical artifacts are downloaded from the public GitHub release, checked
against the paired SHA-256 file, and verified with `gh attestation verify`.
See the repository's
[compatibility policy](https://github.com/thehouseisonfire/rumqtt/blob/main/docs/c-abi-compatibility.md)
for the normative release rules and the
[tool evaluation](https://github.com/thehouseisonfire/rumqtt/blob/main/docs/c-abi-tool-evaluation.md)
for the selection evidence.

Installed CMake packages expose explicit shared and static targets:

```cmake
find_package(rumqttc CONFIG REQUIRED)
target_link_libraries(my_shared_client PRIVATE rumqttc::rumqttc_shared)
target_link_libraries(my_static_client PRIVATE rumqttc::rumqttc_static)
```

The compatibility target `rumqttc::rumqttc` continues to select the static library.

## Ownership and threading

Every config, client, completion, event, and error returned by the library has
a matching destroy function. Destroy functions accept `NULL`. Memory returned
by this library must never be passed to `free()`.

`rumqttc_client_destroy_timeout_ms()` is the one fallible destructor. It
requests immediate shutdown when necessary and consumes the client only after
the driver thread joins. On timeout or failure the caller still owns a valid
handle and may retry. `rumqttc_client_abandon()` is a last-resort consuming
escape hatch: it requests immediate shutdown but relinquishes join ownership,
so a driver thread can remain temporarily alive. Do not unload the shared
library after abandonment while that thread may still be running.

Client operation functions may be called concurrently. Configuration mutation,
client start from that configuration, and handle destruction must not race any
other access to the same handle. Only one event receive may be active per
client; a concurrent receive returns `RUMQTTC_INVALID_STATE`. Borrowed views
remain valid until their owning event or error is destroyed and must not be
used concurrently with access to that owner. Use the copy helpers for longer
retention.

Multi-output accessors allow each unneeded output to be `NULL`, require at
least one output, and initialize every supplied output before validation.
Single-output accessors require their output pointer. Completion observation
functions accept `const rumqttc_completion_t *`; their internal result cache is
synchronized and does not change the caller-visible handle identity.

Client destruction preserves any previously admitted DISCONNECT options while
requesting immediate cleanup and joining the driver. A timeout leaves the handle
valid for retry, including when a host callback is still executing.

Admission means a request entered the bounded local MQTT queue; it does not mean
the broker received it. Tracked completion distinguishes QoS 0 local flush,
QoS 1 acknowledgement, and QoS 2 completion. Destroying or timing out a
completion drops only the waiter and never cancels admitted work. A timeout can
therefore be marked ambiguous even when its returned status is
`RUMQTTC_TIMEOUT`.
Completion polling and waiting are repeatable and safe from concurrent callers:
after termination, every observer receives the same success or error. A wait
deadline does not become the completion's terminal result, so a later observer
can still receive the operation outcome.

Use `rumqttc_error_code()` for fine-grained machine-readable handling. Local
API misuse reports `INVALID_STATE`, premature completion access reports
`WOULD_BLOCK`, and `SHUTDOWN` is reserved for failures caused by the client
lifecycle. Synchronization failures report `INTERNAL`.

Applications must continuously drain events. If the bounded event queue remains
full past its configured delivery timeout, the driver terminates visibly rather
than silently dropping incoming publishes. Manual acknowledgement consumes an
event-bound token; reuse and cross-client acknowledgement are rejected.

`rumqttc_client_try_acknowledge_with_options` and
`rumqttc_client_acknowledge_with_options_tracked` add MQTT 5 ACK content using
`rumqttc_acknowledgement_options_t` and `rumqttc_v5_acknowledgement_options_t`.
Initialize them with `RUMQTTC_ACKNOWLEDGEMENT_OPTIONS_INIT` and
`RUMQTTC_V5_ACKNOWLEDGEMENT_OPTIONS_INIT`, select
`RUMQTTC_PROTOCOL_OPTIONS_V5`, and set `v5_options`. NULL options or a
version-neutral record send default success on either protocol. Explicit V5
options require MQTT 5 even when all their values are defaults.

The legal client-originated PUBACK/PUBREC reasons are `0x00` (Success), `0x80`
(Unspecified error), `0x83` (Implementation specific error), `0x87` (Not
authorized), `0x90` (Topic Name invalid), `0x91` (Packet Identifier in use),
`0x97` (Quota exceeded), and `0x99` (Payload format invalid). `0x10` (No matching
subscribers) is server-only and is rejected. Reason String presence is explicit,
so absent and present-empty values differ; User Properties preserve order and
duplicate names. All supplied strings and properties are copied during the call.
Malformed or oversized options and backpressure leave the event's token and
original default ACK available for retry with different options or the existing
no-options API. The full encoded packet must fit the broker's Maximum Packet
Size for that token's connection generation; properties are never silently
removed. Reconnect invalidates old tokens.

Both APIs admit without waiting for capacity. Tracked
`RUMQTTC_COMPLETION_ACKNOWLEDGED` means the selected ACK flushed locally,
including a negative ACK; it does not prove broker receipt or application
processing. Destroying the completion observer does not cancel an admitted ACK.
The publication remains readable for the event's lifetime after acknowledgement
and client destruction; destruction must follow the existing handle lifetime
rules.

A negative ACK terminates that delivery rather than requesting retry. For
shared subscriptions the broker must discard the message rather than assign it
to another subscriber. In particular, sending `QuotaExceeded` during temporary
overload does not request redelivery. Reason String and User Properties are
broker-facing diagnostics, not an application response to the original
publisher.

`rumqttc_client_close_timeout_ms()` performs a bounded graceful drain and is
idempotent. Its timeout covers coordination with another close caller,
operation completion, and driver-thread joining.
`rumqttc_client_close_now_timeout_ms()` is idempotent, can escalate graceful
shutdown, uses its caller-supplied deadline, and makes no delivery claim for
unfinished operations.

Time units are part of every relevant symbol: keep-alive and connection setup
use `_seconds`; event delivery, receive, completion wait, close, and destruction
use `_ms`.

Initialize extensible records with the header macros instead of manually
maintaining `struct_size`, selectors, and reserved fields:

```c
rumqttc_publish_options_t publish = RUMQTTC_PUBLISH_OPTIONS_INIT;
publish.qos = RUMQTTC_QOS_1;

rumqttc_subscription_t subscription = RUMQTTC_SUBSCRIPTION_INIT;
subscription.filter = (rumqttc_string_view_t){"sensors/+", 9};
```

For MQTT 5 subscription extensions, select MQTT 5 explicitly at both scopes:

```c
rumqttc_v5_subscription_options_t filter_v5 =
    RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT;
filter_v5.no_local = 1;
subscription.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
subscription.v5_options = &filter_v5;

rumqttc_v5_subscribe_properties_t properties =
    RUMQTTC_V5_SUBSCRIBE_PROPERTIES_INIT;
properties.subscription_identifier_present = 1;
properties.subscription_identifier = 7;
rumqttc_subscribe_options_t options = RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
options.v5_properties = &properties;

rumqttc_client_subscribe_tracked(client, &subscription, 1, &options,
                                 &completion, &error);
```

Passing `NULL` for the subscribe or unsubscribe options pointer selects the
version-neutral behavior. Unknown selectors, inconsistent selector/pointer
pairs, and MQTT 5 options submitted to an MQTT 3.1.1 client are rejected
without admission. Initializer macros are provided for every size-versioned
record and compile as aggregate initializers in C11 and C++17.

## Complete C examples

The [`examples`](examples) directory contains warning-clean C11 programs for:

- [single-threaded event polling](examples/event_polling.c);
- [publishing from multiple native threads](examples/multithreaded_publishing.c);
- [polling and timed waiting for tracked completions](examples/tracked_completion.c);
- [manual acknowledgement](examples/manual_acknowledgement.c); and
- [graceful and immediate shutdown](examples/shutdown.c).
- [resource-bounded MQTT 5 setup](examples/resource_limits.c).

Each program accepts `HOST PORT`, owns every returned handle explicitly, and
keeps resource lifetimes local to the operation that acquired them. The
examples are compiled with warnings as errors and run against the deterministic
broker fixture in CI. To reproduce that build against a debug library:

```sh
cargo build --manifest-path native-wrappers/Cargo.toml -p rumqttc-c-next
cmake -S native-wrappers/c/tests/native -B native-wrappers/target/rumqttc-c-native
cmake --build native-wrappers/target/rumqttc-c-native
ctest --test-dir native-wrappers/target/rumqttc-c-native -L example --output-on-failure
```

The WebSocket token example exits with code 77 when the loaded library lacks
`RUMQTTC_CAP_WEBSOCKET_CALLBACKS`; CTest reports it as skipped. Invalid invocation
arguments remain errors.

Keep these distinctions in mind when adapting the examples:

- Successful admission only means that an operation entered the bounded local
  request queue; it is not MQTT completion.
- A completion timeout does not prove non-delivery. The operation may complete
  after the waiter times out.
- Destroying an incomplete completion releases the waiter but does not cancel
  an admitted MQTT operation.
- String and byte views returned from an event or error are borrowed. They
  become invalid as soon as that owning event or error is destroyed.

For multithreaded producers, share the client handle but keep destruction
synchronized after every producer and the single event consumer have stopped.
Each producer may use nonblocking `rumqttc_client_try_*` operations or its own
tracked completion. In manual-ACK mode, retain the incoming event until
`rumqttc_client_try_acknowledge` or `rumqttc_client_acknowledge_tracked` has
successfully consumed its event-bound token.

## Native verification

`native-wrappers/c/tests/native` is a dedicated broker-backed test target, separate
from the fast header and ABI checks. It exercises the C surface from C,
including MQTT 3.1.1 and MQTT 5 behavior, overload, reconnect, shutdown,
native-thread concurrency, and repeated teardown. Every network wait and join
has a deadline. Set `RUMQTTC_C_STRESS_ITERATIONS` to increase the stress run;
CI uses a short run while leak-analysis jobs use a longer one. The callback
stress fixtures cover registration replacement, retained completions, cancellation
races, per-client serialization, shared store-key exclusion, and safe dynamic
library unload. The persistence fixtures restore subscriptions and incoming QoS 2
state, inject callback failures, and interrupt an atomic checkpoint replacement.
Wire fixtures cover rich event properties, runtime limits, topic-alias reconnects,
redirects and SRV fallback, close races, and Unix/socket/WebSocket configuration.

TLS and proxy fixtures generate temporary certificate authorities and test broker
and proxy trust independently, mutual TLS, ALPN, failed construction, and output
redaction. OpenSSL and Python are required by the broker runner. The process Will
fixture uses a local `mosquitto` executable and reports a CTest skip if it is absent.
Platform trust is tested in an isolated Linux child process without changing the
host trust store.

Run all seven installed feature profiles, including static/shared CMake and
pkg-config consumers and the corresponding native transport fixtures, with:

```bash
python3 native-wrappers/c/tests/package_feature_matrix.py --native
```

Use `--profile minimal`, `--profile rustls`, `--profile native`, `--profile mixed`,
or one of their `-proxy` variants to select a profile. The CI matrix runs these
consumers and the native suite on Linux, macOS, and Windows; sanitizer jobs include
the callback cancellation and ownership races.

## Session Present compatibility

The MQTT 3.1.1 default rejects a successful clean-session CONNACK with invalid
Session Present=true. The opt-in `AcceptAsClean` policy resolves only this case
as fresh, resets old local protocol state and replay work, and clears the
current scope/client-ID checkpoint before reporting success. Failed or cancelled
clearing remains pending for retry before checkpoint loading. The broker remains
non-conforming and raw connected-event/connection-result flags remain true.

Existing diagnostics expose an optional `connack` observation containing raw
Session Present, effective session resume, and a compatibility reason. Use the
effective session for resubscription decisions. No MQTT 5 clean-start recovery
is introduced; the existing broker-only resume policy retains its restrictions.

Use `rumqttc_config_set_v4_session_present_mismatch_policy` with
`RUMQTTC_SESSION_PRESENT_MISMATCH_ERROR` (0, default) or
`RUMQTTC_SESSION_PRESENT_MISMATCH_ACCEPT_AS_CLEAN` (1). Wrong-protocol handles
and unknown values are rejected without updating the configuration.
`rumqttc_completion_connack_session_diagnostics` reads the existing diagnostics
completion using separate presence, raw flag, effective resume, and diagnostic
outputs. Diagnostic codes are 0=none, 1=v4 accepted as clean, and 2=v5 broker-only
resume. Optional outputs are zeroed on error; at least one output is required.
Existing public event and diagnostics records and the ABI line are unchanged.
