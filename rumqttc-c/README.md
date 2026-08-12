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
cargo build --release -p rumqttc-c-next
```

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
rumqttc-c/tests/abi/check.sh ffi-header
rumqttc-c/tests/abi/check.sh exports
rumqttc-c/tests/abi/compare-release.sh
python3 rumqttc-c/tests/abi/mutation_matrix.py --output target/abi-mutations
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
maintaining `struct_size` and reserved fields:

```c
rumqttc_publish_options_t publish = RUMQTTC_PUBLISH_OPTIONS_INIT;
publish.qos = RUMQTTC_QOS_1;

rumqttc_subscription_t subscription = RUMQTTC_SUBSCRIPTION_INIT;
subscription.filter = (rumqttc_string_view_t){"sensors/+", 9};
```

Corresponding defaults are provided for user properties, MQTT 5 publish
properties, and diagnostics through `RUMQTTC_USER_PROPERTY_INIT`,
`RUMQTTC_V5_PUBLISH_PROPERTIES_INIT`, and `RUMQTTC_DIAGNOSTICS_INIT`. All five
macros compile as aggregate initializers in C11 and C++17.

## Complete C examples

The [`examples`](examples) directory contains warning-clean C11 programs for:

- [single-threaded event polling](examples/event_polling.c);
- [publishing from multiple native threads](examples/multithreaded_publishing.c);
- [polling and timed waiting for tracked completions](examples/tracked_completion.c);
- [manual acknowledgement](examples/manual_acknowledgement.c); and
- [graceful and immediate shutdown](examples/shutdown.c).

Each program accepts `HOST PORT`, owns every returned handle explicitly, and
keeps resource lifetimes local to the operation that acquired them. The
examples are compiled with warnings as errors and run against the deterministic
broker fixture in CI. To reproduce that build against a debug library:

```sh
cargo build -p rumqttc-c-next
cmake -S rumqttc-c/tests/native -B target/rumqttc-c-native
cmake --build target/rumqttc-c-native
ctest --test-dir target/rumqttc-c-native -L example --output-on-failure
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

`rumqttc-c/tests/native` is a dedicated broker-backed test target, separate
from the fast header and ABI checks. It exercises the C surface from C,
including MQTT 3.1.1 and MQTT 5 behavior, overload, reconnect, shutdown,
native-thread concurrency, and repeated teardown. Every network wait and join
has a deadline. Set `RUMQTTC_C_STRESS_ITERATIONS` to increase the stress run;
CI uses a short run while leak-analysis jobs use a longer one.
