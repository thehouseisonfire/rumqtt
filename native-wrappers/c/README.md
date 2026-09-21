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
`use-native-tls`, `websocket`, `http-proxy`, `socks-proxy`,
`system-srv-resolver`, `auth-scram`, and `tracing`). The default build enables
rustls and WebSocket. A capability bit describes the artifact, not a broker
negotiation. `RUMQTTC_CAP_SESSION_STORE_CALLBACKS` remains clear until its C
callback API ships.

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
use a separate policy setter.

Unix socket paths are native bytes on Unix and fail before start on other
platforms. Declarative WebSocket header edits are copied in order for each
handshake, including reconnects; add preserves duplicates, replace overwrites,
and remove deletes a name. The core rejects protected handshake headers and
redacts all header values in debug output. TCP network setters accept portable
numeric buffer sizes, booleans, and a numeric local socket address. Bind-device
and MPTCP settings fail on platforms that do not support them.

For MQTT 5 graceful or immediate close with a reason and properties, use
`RUMQTTC_V5_DISCONNECT_PROPERTIES_INIT` inside
`RUMQTTC_DISCONNECT_OPTIONS_INIT`, select MQTT 5, and call the matching
`_with_options_timeout_ms` function. A later close caller must supply matching
options because the first admitted payload wins. The original close functions
remain available for version-neutral close.

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
CI uses a short run while leak-analysis jobs use a longer one.
