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
`use-rustls-aws-lc`, `use-rustls-ring`,
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
adds a dedicated keychain and an administrator trust entry, while Windows uses
the current user's Root store. Cleanup restores the keychain search list and
removes only the fixture's own root, including after child-process failure.
An interrupted run leaves a cleanup manifest; CI unconditionally runs
`python3 native-wrappers/c/tests/native/platform_trust.py --cleanup`.
If macOS certificate removal stalls, cleanup exports the current administrator
trust settings and imports them with only the fixture's entry removed. It verifies
removal before deleting the manifest, preserving it if cleanup still fails.
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
