# rumqttc C API

`rumqttc-c-next` builds one C library for MQTT 3.1.1 and MQTT 5. Each
configuration selects exactly one protocol, which cannot change during the
resulting client's lifetime. The stable ABI version is 1.0; the Rust crate
version describes the implementation release and is available through
`rumqttc_library_version()`.

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

CMake and pkg-config templates are included for release packaging. ABI-v1 is
natively built and loaded in CI on Linux x86_64, macOS arm64, and Windows
x86_64; no ABI guarantee is made for an untested target.

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
