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

Client operation functions may be called concurrently. Configuration mutation,
client start from that configuration, and handle destruction must not race any
other access to the same handle. Only one event receive may be active per
client; a concurrent receive returns `RUMQTTC_INVALID_STATE`. Borrowed views
remain valid until their owning event or error is destroyed and must not be
used concurrently with access to that owner. Use the copy helpers for longer
retention.

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

`rumqttc_client_close()` performs a bounded graceful drain and is idempotent. Its timeout covers
coordination with another close caller, operation completion, and driver-thread joining.
`rumqttc_client_close_now()` is idempotent, can escalate graceful shutdown, and
makes no delivery claim for unfinished operations. Client destruction requests
immediate shutdown and waits for at most two seconds for the driver thread.

## Minimal polling example

```c
#include <rumqttc.h>
#include <string.h>

rumqttc_config_t *config = NULL;
rumqttc_client_t *client = NULL;
rumqttc_error_t *error = NULL;
rumqttc_string_view_t host = {"localhost", strlen("localhost")};
rumqttc_string_view_t id = {"native-client", strlen("native-client")};

rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, &error);
rumqttc_config_set_broker(config, host, 1883, &error);
rumqttc_config_set_client_id(config, id, &error);
rumqttc_client_start(config, &client, &error); /* clones config */
rumqttc_config_destroy(config);

for (;;) {
    rumqttc_event_t *event = NULL;
    rumqttc_status_t status = rumqttc_client_event_recv(client, 1000, &event, &error);
    if (status == RUMQTTC_TIMEOUT) {
        rumqttc_error_destroy(error);
        error = NULL;
        continue;
    }
    if (status != RUMQTTC_OK) {
        break;
    }
    /* Inspect event kind and typed accessors here. */
    rumqttc_event_destroy(event);
}

rumqttc_error_destroy(error);
error = NULL;
rumqttc_client_close(client, 5000, &error);
rumqttc_client_destroy(client);
rumqttc_error_destroy(error);
```

For multithreaded producers, share the client handle but keep destruction
synchronized after every producer has stopped. Each producer may call the
nonblocking `rumqttc_client_try_*` functions or create independent tracked
completion handles. In manual-ACK mode, retain the incoming event until
`rumqttc_client_try_acknowledge` or `rumqttc_client_acknowledge_tracked` has
successfully consumed its event-bound token.
