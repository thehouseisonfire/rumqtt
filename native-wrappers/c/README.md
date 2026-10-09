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

## Runtime configuration updates

`RUMQTTC_CAP_RUNTIME_CONFIGURATION` reports the additive update API. Create a
`rumqttc_runtime_update_t`, replace supported fields using typed setters, or use
`rumqttc_runtime_update_field_action()` for unchanged/clear. Submit with
`rumqttc_client_update_configuration_tracked()`. Builders and TLS profile inputs
are copied independently at admission and can then be destroyed or reused.

`RUMQTTC_COMPLETION_CONFIGURATION_STAGED` proves driver staging. Extract an owned
receipt with `rumqttc_completion_configuration_receipt()` to inspect separate
tuning/connection activation states. `rumqttc_client_configuration_snapshot()`
captures owned redacted desired/effective values, revisions, routes, attempt
outcomes and monotonic freshness ages. Initialize accessor records with `_INIT`
macros. Network accessor string views borrow from the snapshot until destruction.
Receipts/snapshots remain readable after client destruction. Connection errors
retain their selected origin revision through `rumqttc_error_configuration_revision()`.

Supported fields are request/read batching and pending-replay throttle (safe poll
boundary), broker credentials, broker TLS profiles, complete network settings and
connection timeout (next origin attempt). No update forces a reconnect. An idle
connection with keepalive disabled can defer activation indefinitely. Protocol,
broker, transport kind, session identity, live queues and AUTH authorities remain
fixed. Read the [applicability, revision, redirect, failure and erasure
contract](../wrapper-core/runtime-configuration.md) and the runnable
[rotation example](examples/configuration_rotation.c) before using this API.

## Shared execution

Dedicated execution remains the default: each started client has its own driver
thread and Tokio runtime. `RUMQTTC_CAP_SHARED_EXECUTION` is present in every build.
The additive API lets many clients share one explicitly owned runtime:

```c
rumqttc_execution_options_t options = RUMQTTC_EXECUTION_OPTIONS_INIT;
rumqttc_execution_context_t *context = NULL;
options.client_capacity = 1000;
/* Check each status; error handling is omitted from this fragment. */
rumqttc_execution_context_new(&options, &context, NULL);
rumqttc_config_set_execution_context(config, context, NULL);
rumqttc_client_start(config, &client, NULL);
/* Gracefully close clients first when MQTT DISCONNECT is required. */
rumqttc_execution_context_request_shutdown(context, NULL);
rumqttc_execution_context_join_timeout_ms(context, 10000, NULL);
/* Release client/configuration/callback owners before unloading. */
rumqttc_execution_context_release(context);
```

The [runnable example](examples/shared_execution.c) runs both MQTT versions on
one context. `NULL` construction options selects defaults; use the initializer
for the size-versioned options record and leave `reserved` zero. All counts must
be nonzero. Defaults allow 1,024 clients, two scheduler workers and 32 blocking
workers. One extra management thread owns runtime creation/destruction. DNS and
deferred TLS can use the blocking pool; limiting its workers does not bound its
queue or total process threads. Context capacity includes starts under
construction and clients awaiting auxiliary cleanup. Capacity exhaustion returns
`BACKPRESSURE` without admitting a client; failed starts return their reservation.

Setting a context retains it and affects future starts, which atomically snapshot
configuration and placement. Clearing selection restores dedicated startup.
Configuration attachment may retain a closing context; starting against it
returns `INVALID_STATE`. `retain` returns a separately released handle.
Configurations, clients and closers retain ownership: releasing an application
handle does not stop clients. Final-owner release requests cleanup without
waiting and does not prove execution stopped.

Shutdown transitions `OPEN` to `CLOSING`, rejects new starts and coalesces
immediate client cleanup. Existing graceful/ordered close APIs remain available;
finish those before requesting context shutdown when their guarantees are
needed. Shutdown has no new graceful-drain policy or escalation deadline.
`try_join` returns `WOULD_BLOCK` while pending. Timed join requires shutdown
first; `TIMEOUT` ends only that observer's wait, and teardown remains retryable.
Successful join sets `QUIESCENT` after drivers, tracked auxiliary work, runtime
blocking work, workers and the management thread have stopped. Management failure
returns an error on repeated join attempts and never reports quiescence.
Client close/join stops only that client's execution and tracked auxiliary work.

Context quiescence does not release host-retained callback tokens, streams,
registrations, configurations or host jobs. Destroy all independent owners and
join host jobs before unloading the library. Tokens retained after cancellation
remain releasable; successful context join cannot authorize unloading by itself.

Nonzero blocking event/completion/close/destroy/join waits and context creation
return `INVALID_STATE` in host callbacks or on context workers, including waits
for another client. Zero-time observations, nonblocking admission, shutdown
request and state/try-join observation remain available. Synchronous callbacks
and destructors must return promptly; timeouts cannot preempt host code. Shared
tasks may migrate between workers, so callbacks have no thread affinity. Bounded
cooperative work preserves peer progress but supplies no hard latency bound.
Resource savings and observed latency tradeoffs are recorded in
[execution measurements](../wrapper-core/benches/execution.md).

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
are unsupported. Synchronous hooks cannot be forcibly preempted; cancellation and elapsed
budgets are processed once a callback returns. The
absolute original connection deadline is checked before and after callbacks
and before exposing the stream. Pins, verifiers and external identities force
resumption off so every reconnect performs authentication again.

### Deferred TLS hooks

Use `rumqttc_tls_verifier_registration_new_async()` or
`rumqttc_tls_identity_registration_new_async()` with their async vtables. They
return the same opaque registrations and attach through the existing profile
extension fields. Deferred hooks are supported by Rustls and reported by
`RUMQTTC_TLS_ADVANCED_DEFERRED_VERIFIER` and
`RUMQTTC_TLS_ADVANCED_DEFERRED_IDENTITY`; native TLS rejects them at profile
construction. Existing synchronous registrations continue to work.

Each callback receives an operation ID, borrowed request and borrowed completion.
It may answer immediately with `rumqttc_callback_tls_verify_complete()`,
`rumqttc_callback_tls_select_complete()` or
`rumqttc_callback_tls_sign_complete()`. To answer later, retain the completion
with `rumqttc_callback_completion_retain()`, copy the required request inputs,
return promptly, and complete on any host thread. Destroy every retained token.
Signing completion copies the signature. `SIZE_MAX` deliberately declines
selection. Responses accept OK, REJECTED, FAILED, TIMEOUT or TRANSIENT; malformed
responses return INVALID_ARGUMENT and leave an active operation available for
correction. A successful completion accepts one answer; it does not mean the
handshake has authenticated successfully. The selected entry and signature are
still checked before use.

Rustls handshakes using deferred hooks run on one worker per active handshake;
the driver invokes TLS callbacks and waits asynchronously for answers. Its
original connection deadline includes callback work. Close, timeout and failed
attempts cancel pending futures and wake workers, including network waits.
`cancel(data, operation_id)` runs exactly once for unresolved cancelled work,
outside locks; it must promptly signal cancellation rather than join host work.
Late, cancelled, expired or duplicate completions return INVALID_STATE before
reading response views. Dropping the final unanswered host token reports
ABANDONED, a terminal typed callback failure.

Requests are serialized per handshake; shared registrations may run concurrently
across clients. `max_retained_operations` defaults to 64 and must be 1..65536.
It bounds live operations including retained completed/cancelled work; cloning
a token does not consume another slot. IDs never wrap. Retained tokens keep
registration data alive after client destruction until their final release.
Future construction/polling and C callback entry/cancellation must return or
yield promptly; deferral cannot preempt host code that blocks on entry. Continue
to avoid waits for the same driver's MQTT progress. Standard trust, hostname,
pins, signature checks, resumption restrictions and failure retry policy apply
to deferred hooks too.

[The native EVP example](examples/external_identity.c) takes
`HOST PORT CA_PEM CLIENT_CHAIN_PEM HOST_KEY_PEM [wss]` and supports RSA-PSS
SHA-256 or P-256 ECDSA. Its host opens the key and retains it through the
registration; replace the signing adapter with an HSM integration as needed.
The `rumqttc-example-external_identity_deferred` target uses the same CLI and
source, with immediate selection and signing on host threads. It copies the
message, retains/completes/releases its token and joins host workers after
client/configuration cleanup. Replace its short-lived workers with a reactor
or key service for production use.
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


Application decisions use `rumqttc_redirect_vtable_t` and
`rumqttc_redirect_registration_new()`, installed with
`rumqttc_config_set_v5_redirect_authority()`. The most recent successful fixed or
authority setter wins; failed setters leave configuration unchanged. Construction
transfers host ownership only on success. Registrations/configurations/clients
share the owner, and its destroy callback runs exactly once after the last owner
is released. See [`examples/redirect_authority.c`](examples/redirect_authority.c)
for exact advertised-target approval and an isolated replacement identity.

The synchronous callback receives a borrowed request and response builder.
`rumqttc_redirect_request_info()` and `_reference()` expose source, reason,
attempt, current client ID/scope, and validated reference kind/scheme/host/port/
WebSocket resource/SRV owner. Initialize output records with their `_INIT`
macros. String views last as long as the request; `request_retain()` creates an
owned snapshot that can survive client destruction. It does not extend the
decision lifetime. The response is valid only during the callback and cannot be
retained or completed later. `response_follow()` selects a zero-based index
from that exact request and a TCP/TLS/WS/WSS transport. TLS/WSS require an existing
owned TLS profile, retained with its hooks through use. Every failed builder
operation invalidates the decision even if its status is ignored; input fields
are copied transactionally. Returning nonzero from the callback fails the
decision. Leaving the response untouched rejects it.

Target defaults are a fresh ID and an isolated clean session, with origin
credentials, store, proxy and header modifiers cleared. `response_set_client_id()`
chooses fresh/reused/replaced identity; identity reuse alone preserves no session.
`response_set_credentials()` supplies target username/password independently,
including present-empty values. `response_set_reuse()` independently approves
existing enhanced-auth authority/method/data and native network credentials.
Network approval retains proxy configuration and all WebSocket modifiers as one
unit. Authentication approval retains the current authority, reports the actual
target/broker-assigned client ID, and never inherits origin username/password.
Replacement enhanced-auth authorities and selective proxy/header reuse are not
supported. Target TLS policy always comes from the selected profile.

`response_set_session(RUMQTTC_REDIRECT_SESSION_REUSE, scope)` explicitly enables
scoped session reuse. Unchanged client ID and scope can preserve live state;
a changed key resets live state and may load that key's checkpoint. Broker
Session Present and strict local-state checks still apply. Stores require
nonempty IDs/scopes, clean start disabled and nonzero expiry. A target key's
exclusive lease is acquired before applying the profile; a conflict returns
`RUMQTTC_REDIRECT_FAILURE_STORE_IN_USE` and `RUMQTTC_STORE_FAILURE_IN_USE`.
Origin leases remain available for temporary restoration; permanent establishment
and rollback release obsolete leases, and teardown releases all remaining leases.
This does not copy checkpoints between brokers. Isolated hops discard store reuse.

Callbacks run without wrapper lifecycle/admission locks, may overlap across
clients, and must return promptly. Nonblocking admission is allowed; waiting for
completion or destroying/joining the active client is prohibited. Foreign
callbacks/destructors must never unwind across C. The finite decision budget
covers callback and profile preparation; late results are rejected and shutdown
is checked on return. It cannot interrupt a blocking callback or bound driver
stalls/shutdown latency. Existing attempt bounds, loop detection, native SRV
fallback, lookup timeout and individual connection deadlines remain; no total
redirect-chain deadline is promised. Limits are 256 references, 64 KiB copied
request text and 256 KiB response fields plus MQTT wire-length limits.

Policy failures add callback, panic, timeout, invalid-response, resource-limit
and store-in-use selectors without renumbering existing failure codes.
`rumqttc_event_redirect_selected_reference()` returns the selected advertised
reference separately from the eventual endpoint/candidate diagnostics. Diagnostic
errors and Debug output omit CONNECT credentials.

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

## Ordered publish shutdown (optional)

Build with `cargo build --manifest-path native-wrappers/Cargo.toml -p rumqttc-c-next --features ordered-shutdown`.
Standard packages leave this feature disabled.
Check `RUMQTTC_CAP_ORDERED_SHUTDOWN` in `rumqttc_library_capabilities()` before
calling the optional APIs. Their declarations and exports exist in all builds;
disabled calls return `RUMQTTC_CONFIG_ERROR`, initialize outputs, and do
not admit work or close the client. Cargo feature unification can enable native
runtime overhead through another dependency without enabling callable C support.

Use the finite deadline form demonstrated in
[ordered_shutdown.c](examples/ordered_shutdown.c):

```c
rumqttc_completion_t *fence = NULL;
rumqttc_status_t status = rumqttc_client_disconnect_after_queued_timeout_ms_tracked(
    client, 5000, &fence, &error);
/* After successful admission, observe with the existing completion functions. */
/* Success has kind RUMQTTC_COMPLETION_ORDERED_SHUTDOWN. */
```

The eight `disconnect_after_queued` admission functions use the native successful
admission order across producers. `try_` functions return an operation ID;
`*_tracked` functions return an owned completion. Both families are nonblocking,
have optional `_timeout_ms` and `_with_options` variants, and copy MQTT 5 views.
A full queue returns `RUMQTTC_BACKPRESSURE` without installing a fence. Later
publishers receive `RUMQTTC_INVALID_STATE`. An untimed fence may wait indefinitely.

Success proves preceding publishes reached their QoS milestone and DISCONNECT
flushed, including required persistence: QoS 0 transport flush, QoS 1 PUBACK,
QoS 2 PUBCOMP. Subscriptions, independent inbound ACKs and authentication are
outside the collective guarantee. Outgoing packet events and driver termination
are not completion authority. Dropping a completion does not cancel admission;
retained completions survive client destruction.

`rumqttc_client_close_after_queued_timeout_ms` and its `_with_options_timeout_ms`
variant admit or attach to the fence, then join within one caller budget.

| Existing policy | Subsequent call | Result |
| --- | --- | --- |
| Open | Raw fence / ordered closer | First successful admission owns policy, payload and deadline |
| Ordered | Raw fence | Invalid state; no duplicate operation |
| Ordered | Matching ordered closer | Same immutable result; independent observer/join budget |
| Ordered | Different payload or ordinary graceful close | Invalid state; original fence retained |
| Ordinary graceful / immediate without a prior fence | Ordered closer | Invalid state |
| Ordered | Immediate close, destruction or abandonment | Abort; unresolved fence reports superseded by immediate close |

The native absolute deadline starts at successful fence admission and persists
across supported persistent-session reconnects. Observer timeout does not cancel
it or reset its deadline. At expiry the operation reports timeout promptly while
the driver retains required terminal storage cleanup and join ownership. Cleanup
is not deadline bounded; retry joining or explicitly abort. Cleanup failure remains
visible as a driver terminal error and cannot turn timeout into success. A join
timeout retains ownership. Synchronous host callbacks must return promptly.

`rumqttc_error_ordered_disconnect_failure()` retains typed timeout, publish,
transport, protocol, persistence, session reset, redirect, replay-unavailable,
receiver termination and supersession reasons. Delivery can remain ambiguous,
even after timeout. Existing broker/store detail accessors remain available;
diagnostic text omits host secrets.
A collective failure can return `RUMQTTC_AMBIGUOUS` while retaining the rejecting
publish's broker reason; it makes no rejection claim for the entire burst.

`rumqttc_completion_ordered_shutdown_diagnostics()` reads an optional cached
observation from a diagnostics completion, using the separately size-versioned
`RUMQTTC_ORDERED_SHUTDOWN_DIAGNOSTICS_INIT` record. Phase, exact fence sequence,
remaining deadline at capture, optional local queue count and snapshot age are
observations. The local count excludes channels and inflight work and cannot
establish a fence or prove delivery. The record is absent before fence admission.
Diagnostics remain admissible while ordered shutdown is closing. Existing record
layouts are unchanged.

See [performance measurements](../wrapper-core/benches/README.md) for the optional
cost, configurations and reproduction commands. JavaScript and Python retain
their existing public close APIs; ordered exposure is a separate follow-up.

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

MQTT 5 preserves strict negotiated-capability admission by default. Select
`RUMQTTC_PUBLISH_ADMISSION_EVENT_LOOP_VALIDATED` with
`rumqttc_config_set_v5_publish_admission_policy()` for offline process-local
queueing and deferred connected checks. All MQTT 5 wrapper clients enforce
1,024 outstanding publishes and 16 MiB charged data by default; configure these
with `rumqttc_config_set_v5_publish_budget()`, restore defaults with
`rumqttc_config_reset_v5_publish_budget()`, and inspect usage with
`rumqttc_client_v5_publish_budget_snapshot()`. Both C publish entry points remain
nonblocking. `rumqttc_error_publish_failure()` distinguishes capability waits,
channel exhaustion, byte/count exhaustion and local rejection.
`RUMQTTC_LOCAL_REJECTED` distinguishes local checks from broker ACK rejection;
previously transmitted replay failures remain ambiguous. Read the
[full accounting, recovery and retry contract](../wrapper-core/publish-admission.md)
before choosing an application retry/durability policy.

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
It observes its tracked shutdown result after joining, and repeated callers
retain the same failure if shutdown failed. Client destruction still performs
cleanup and waits for driver teardown.

If graceful shutdown expires while deferred TLS work is pending, its close
completion retains the timeout result. A TLS future destruction failure still
terminates the driver and pending operations with the typed TLS callback error,
without emitting a successful immediate-shutdown event.

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

## Terminal broker acknowledgement details

Initialize `rumqttc_acknowledgement_details_t` with
`RUMQTTC_ACKNOWLEDGEMENT_DETAILS_INIT` and call
`rumqttc_completion_acknowledgement()`. A pending operation returns
`RUMQTTC_WOULD_BLOCK`; a terminal operation without an ACK succeeds with
`present=0`. The accessor succeeds for broker-rejected operations even after
legacy poll/wait returned an error and that error object was destroyed.
Existing completion kinds, result accessors, classification, and delivery status
are unchanged.

Metadata retains the packet kind, identifier, scalar reason where available,
native property presence, and recovered QoS 2 distinction. The
`rumqttc_completion_acknowledgement_result_count/at` accessors expose exact
ordered filter codes; `..._reason_string` and `..._user_property_count/at`
expose packet-level diagnostic properties. Duplicate User Properties retain
order. Reason String presence distinguishes absent from present-empty.
Packet-kind values use the `RUMQTTC_ACKNOWLEDGEMENT_*` constants.

Borrowed string views reference immutable completion-owned storage. They remain
valid across repeated observations and client destruction, and expire when that
completion handle is destroyed. Copy with `rumqttc_string_copy()` before
completion destruction; caller-owned copies remain usable afterward. Never race
handle destruction with accessors or borrowed-view reads. Property contents are
omitted from Debug, automatic logs, and formatted errors; explicit caller
inspection should follow the application's own diagnostic-data policy.

MQTT 5 exposes terminal PUBACK, rejected PUBREC, PUBCOMP, SUBACK, and UNSUBACK.
Successful intermediate PUBREC and PUBREL remain internal. `recovered=1`
preserves the native successful replay outcome with PUBCOMP reason `0x92`;
ordinary `0x92` remains a rejection. QoS 0 and local failures have no broker ACK.
MQTT 3.1.1 exposes packet identifiers and SUBACK return codes, with no scalar
reason or MQTT 5 properties. UNSUBACK filter results are unavailable, explicitly
reported by `result_count`'s presence output. Non-filter packet kinds reject
filter accessors. These are native decoded terminal details, not raw wire tracing.
Python and JavaScript retain their existing coarse result APIs.

## Reconnect policy

Unconfigured clients retain legacy immediate retries and existing terminal
checks. To opt into classified retry decisions, initialize
`rumqttc_reconnect_options_t` with `RUMQTTC_RECONNECT_OPTIONS_INIT` and install it
with `rumqttc_config_set_reconnect_policy` before startup. Defaults are a
1-second initial delay, 60-second cap, integer multiplier 2, full jitter,
explicit unlimited retries, and a 30-second stability interval.
`rumqttc_config_clear_reconnect_policy` restores legacy behavior for future
clients; neither setter modifies an already running client.

```c
rumqttc_reconnect_options_t retry = RUMQTTC_RECONNECT_OPTIONS_INIT;
retry.budget_kind = RUMQTTC_RECONNECT_BUDGET_FINITE;
retry.retry_limit = 5;
rumqttc_config_set_reconnect_policy(config, &retry, &error);
```

The first connection cycle starts immediately and is free. Each subsequent
cycle consumes a retry when started, including retries before the first
successful connection. Finite zero permits only the initial cycle. The
exponential base grows after each failure and is capped before full jitter
samples uniformly from zero through that base; `JITTER_NONE` uses the base
unchanged. Zero delays and multiplier 1 are valid. Unlimited requires
`retry_limit=0`; malformed selectors, reserved fields, invalid ranges and
unrepresentable timer durations are rejected atomically.

Successful CONNACK alone does not reset retries or backoff. Reset requires an
uninterrupted connection lasting `stability_interval_ms`, including idle time.
Zero stability resets on CONNACK and permits indefinitely repeated short
connections. MQTT 5 handshake redirects, buffered events, and native SRV
fallback remain within the current cycle. A redirect from an established
connection consumes a new cycle and uses backoff. Native redirect authority,
limits and session isolation remain in force. This budget does not bound
individual SRV candidate dials.

Classified decisions inspect typed native variants and causes, not formatted
text or the historical coarse error kind:

| Failure | Classified decision |
| --- | --- |
| Transport I/O, DNS/connect failures, peer closure, missed PING, connection/flush timeout | Retry; invalid input/data, unsupported operations and permission denial stop |
| v4 CONNACK service unavailable | Retry; other refusals stop |
| v5 CONNACK service/server unavailable, server busy, connection rate exceeded | Retry; other refusals stop or follow native redirect processing |
| v5 DISCONNECT normal, server busy/shutdown, keepalive timeout, connection rate exceeded, maximum connection time | Retry; other reasons stop or follow native redirect processing |
| Malformed protocol data, session mismatch, invalid configuration, authentication rejection, persistence failure | Stop |
| TLS verification/configuration and opaque TLS/proxy failures without a typed transient transport cause | Stop |
| SOCKS proxy/target unavailable, general server failure, unreachable host/network, refused connection or expired TTL | Retry; authentication, ruleset, address and protocol failures stop |
| WebSocket HTTP 408, 429, 5xx or transport closure | Retry; malformed frames and other HTTP/protocol failures stop |
| Typed transport/TLS/WebSocket callbacks | Existing typed eligibility; mandatory callback/destructor failures stop |
| Unclassified failures | Stop |

WebSocket classification also inspects errors wrapped as stream I/O. EOF
without a WebSocket closing handshake remains a retryable transport loss.

MQTT 5 broker DISCONNECT ends stability tracking when observed, including when
earlier packets are still queued for delivery. Event backpressure and deferred
native cleanup cannot earn a stability reset after the connection ends; a reset
already earned before DISCONNECT is preserved.

Under classified policy, exposed error retryability matches the decision.
Legacy decisions retain their historical metadata differences; an error with
`retryable=0` alone does not describe whether a legacy driver will reconnect.

`rumqttc_client_reconnect_diagnostics` returns an owned scalar snapshot without
waiting for native polling. It remains available after termination while the
client handle lives. `rumqttc_completion_reconnect_diagnostics` observes an
immutable snapshot from an existing diagnostics completion. Durations refer to
capture time; `snapshot_age_ms` exposes the age of retained completion snapshots.
Lifetime cycle counts include the free initial cycle; retries since reset and
reset counts are separate. Last-error accessors return independent owned error
handles, or NULL when absent, and must not alias `error_out`.

Exhaustion produces nonretryable `RECONNECT_EXHAUSTED`, with cycle/retry counts
through `rumqttc_error_reconnect_exhaustion` and the last sanitized failure
through `rumqttc_error_reconnect_last_error`. Observer timeouts remain separate
and do not stop retries. First-connection observers and unfinished operations
resolve at terminal exhaustion, completed outcomes remain repeatable, and
unfinished admitted delivery can remain ambiguous. Exhaustion does not prove
non-delivery or make resubmission safe.

Backoff keeps diagnostics, completion observation and immediate close
responsive. Ordinary graceful close admitted before the initial driver poll
allows the initial connection to drain queued operations. After a failed cycle,
ordinary graceful close terminates without retrying. Ordered close may recover
under native session rules, subject to the same retry budget; its total deadline
interrupts backoff and drives native terminal persistence cleanup. Per-attempt
handshake timeouts refresh for a new
attempt; shutdown totals and authentication exchange deadlines retain their
scope. No total connection timeout or live policy mutation is introduced.
Mandatory native terminal failures are handled before retry gating. At the
gate, immediate shutdown and graceful shutdown after a failed cycle take
precedence, followed by expired ordered shutdown cleanup, retry exhaustion,
and finally a ready retry timer.
Admission remains bounded, and applications must continue draining events to
avoid existing event-delivery overflow termination. MQTT replay and Session
Present reconciliation remain native-owned.

See the [retry example](examples/reconnect.c). Custom retry decisions and
pause/resume/request-attempt commands remain follow-up work.

## Complete C examples

The [`examples`](examples) directory contains warning-clean C11 programs for:

- [single-threaded event polling](examples/event_polling.c);
- [classified reconnects and finite retry budgets](examples/reconnect.c);
- [publishing from multiple native threads](examples/multithreaded_publishing.c);
- [bounded offline MQTT 5 publishing](examples/offline_publishing.c);
- [polling and timed waiting for tracked completions](examples/tracked_completion.c);
- [manual acknowledgement](examples/manual_acknowledgement.c);
- [graceful and immediate shutdown](examples/shutdown.c);
- [optional ordered publish shutdown](examples/ordered_shutdown.c); and
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
