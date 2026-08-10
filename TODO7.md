# Stable C Wrapper

## Goal

Provide a small, stable C ABI over the v4 and v5 clients. The API must work from
C and from other languages that consume C libraries without exposing Rust
layout, ownership, panics, generics, futures, or allocator assumptions.

Favor an explicit pull-based API with opaque handles. Callbacks can be added in
a later ABI version after their threading, reentrancy, and teardown contracts
are proven necessary.

### Protocol packaging contract

Ship one C library and header supporting both MQTT 3.1.1 and MQTT 5. Every
configuration explicitly selects one protocol, and every client created from
that configuration uses only that version for its complete lifetime. To use
another version, create another configuration and client.

Do not auto-negotiate, silently fall back between versions, or expose a handle
that switches protocols after construction. Keep one common ABI for operations
whose semantics overlap, while using tagged values or protocol-specific
setters for behavior that differs. Reject MQTT 5-only configuration or command
data for an MQTT 3.1.1 client rather than ignoring it.

Separate protocol-specific libraries are not the baseline distribution. Add
such artifacts only if measured binary-size, platform, or dependency
constraints justify the additional ABI and test matrix.

## Prerequisite

Use the shared driver and owned boundary values from `TODO5.md`. The C crate is
responsible for ABI stability and C allocation rules, not MQTT lifecycle or
v4/v5 event translation.

## Proposed layout

```text
rumqttc-c/
├── Cargo.toml
├── README.md
├── build.rs
├── include/
│   └── rumqttc.h
├── src/
│   ├── lib.rs
│   ├── client.rs
│   ├── completion.rs
│   ├── config.rs
│   ├── error.rs
│   ├── event.rs
│   ├── ffi.rs
│   └── panic.rs
├── tests/
│   ├── abi/
│   └── c/
└── cmake/
    └── rumqttcConfig.cmake.in
```

Build both `cdylib` and `staticlib`. Generate a candidate header with `cbindgen`
but check in and review the public `include/rumqttc.h`; release artifacts must
not depend on users running header generation. CI must fail when generated ABI
declarations and the checked-in header differ.

## 1. Establish ABI rules

### 1.1 Versioning and symbol visibility

Export only functions declared in `rumqttc.h`, all prefixed `rumqttc_`. Hide
other symbols where supported. Provide:

```c
#define RUMQTTC_ABI_VERSION_MAJOR 1
#define RUMQTTC_ABI_VERSION_MINOR 0

uint32_t rumqttc_abi_version(void);
const char *rumqttc_library_version(void);
```

Encode the ABI version in a documented integer format. Follow semantic ABI
rules: add functions and enum values compatibly within major version 1; never
change an existing function signature, struct layout, enum numeric value, or
ownership contract without a major ABI bump.

Use fixed-width integer types and explicit boolean values. Do not expose Rust
`usize`, `bool`, `char`, `String`, `Vec`, `Duration`, `Result`, or a Rust enum's
layout. All public C structs must begin with a `struct_size` field if callers
allocate them, allowing future tail extension.

### 1.2 Opaque ownership

Forward-declare owned handles:

```c
typedef struct rumqttc_config rumqttc_config_t;
typedef struct rumqttc_client rumqttc_client_t;
typedef struct rumqttc_event rumqttc_event_t;
typedef struct rumqttc_completion rumqttc_completion_t;
typedef struct rumqttc_error rumqttc_error_t;
```

Every returned owned handle must have one matching destroy function. Destroy
functions accept `NULL` and do nothing. Memory returned by the library must be
released by the library; never require a caller to pair Rust allocation with
`free()`.

Use borrowed pointer-and-length views only while the owning opaque object is
alive and not concurrently accessed. Document invalidation precisely. Provide
copy-out functions where callers need longer retention.

### 1.3 Input strings and bytes

Use explicit slices:

```c
typedef struct {
    const uint8_t *data;
    size_t len;
} rumqttc_bytes_view_t;

typedef struct {
    const char *data;
    size_t len;
} rumqttc_string_view_t;
```

Allow `{NULL, 0}` for empty optional data and reject `NULL` with nonzero length.
Copy input before returning from any call that queues work. Validate UTF-8 for
MQTT strings and reject embedded U+0000 where the MQTT specification forbids
it. Payloads, passwords, correlation data, certificates, and keys remain
arbitrary bytes and may contain zero bytes.

Do not use sentinel-terminated payloads or borrow caller memory on the driver
thread.

## 2. Define status and error handling

Define stable numeric status codes, including at least:

```c
typedef enum {
    RUMQTTC_OK = 0,
    RUMQTTC_INVALID_ARGUMENT = 1,
    RUMQTTC_INVALID_STATE = 2,
    RUMQTTC_CONFIG_ERROR = 3,
    RUMQTTC_BACKPRESSURE = 4,
    RUMQTTC_TIMEOUT = 5,
    RUMQTTC_DISCONNECTED = 6,
    RUMQTTC_PROTOCOL_ERROR = 7,
    RUMQTTC_BROKER_REJECTED = 8,
    RUMQTTC_AMBIGUOUS = 9,
    RUMQTTC_INTERNAL_ERROR = 10,
    RUMQTTC_WOULD_BLOCK = 11
} rumqttc_status_t;
```

Functions return `rumqttc_status_t`. Operations with diagnostic detail accept
an optional final `rumqttc_error_t **error_out`, which is set to `NULL` on
success and to a newly owned error on failure. Provide error code/kind/message,
retryable and ambiguous flags, optional operation identifier, source-chain
formatting for logs, and `rumqttc_error_destroy`.

Do not use a process-global or thread-local “last error” as the only diagnostic
channel: it is easy to overwrite and awkward for concurrent callers. Formatted
messages are not stable identifiers.

## 3. Contain unsafe code and panics

Put all `extern "C"` entry points in `ffi.rs`. Each entry point must:

1. validate every pointer, length, enum discriminant, and output parameter;
2. convert inputs into owned safe Rust values in a small unsafe block;
3. invoke safe internal implementation code; and
4. use `catch_unwind(AssertUnwindSafe(...))` at the ABI boundary.

After checking required output-pointer locations for `NULL`, initialize handle
outputs and supplied error outputs to `NULL` before doing fallible work. As with
all C APIs, non-null pointers must still refer to caller-owned writable storage;
the library cannot validate arbitrary addresses. This prevents callers from
observing stale pointers on ordinary failures.

Map a caught panic to `RUMQTTC_INTERNAL_ERROR`, mark the affected client failed
when applicable, and prevent further ordinary operations. Never allow unwinding
across C. Configure release panic behavior consistently with this requirement;
do not use `panic = "abort"` for the C library release artifact unless the
documented ABI contract explicitly changes to process abort.

Add crate-level `#![deny(unsafe_op_in_unsafe_fn)]`. Keep raw pointer conversion
helpers small and covered by unit tests. Run Miri on the safe/raw conversion
tests where supported.

## 4. Configuration API

Use an opaque mutable builder so its representation can evolve:

```c
typedef enum {
    RUMQTTC_PROTOCOL_V311 = 1,
    RUMQTTC_PROTOCOL_V5 = 2
} rumqttc_protocol_t;

rumqttc_status_t rumqttc_config_new(
    rumqttc_protocol_t protocol,
    rumqttc_config_t **out,
    rumqttc_error_t **error_out);

void rumqttc_config_destroy(rumqttc_config_t *config);
```

The protocol discriminants are stable ABI values. Reject unknown values and
store the selected protocol in the opaque configuration. A client created from
that configuration cannot change the selection later.

Add setters for the initial fields in `TODO5.md`. Setters validate and copy
their input. Use protocol-specific names for semantics that are not genuinely
common, for example MQTT 3.1.1 clean session versus MQTT 5 clean start/session
expiry.

Represent TLS material as caller-provided bytes in the base API. Optional
convenience functions may load paths, but they must report filesystem errors
explicitly and must not make path interpretation the only configuration route.

Consume or clone the configuration in `rumqttc_client_start`; choose one rule
and encode it in the function name/documentation. Prefer cloning so the caller
can destroy the config immediately and reuse it to create another client.

## 5. Client and operation API

### 5.1 Lifecycle

Provide:

```c
rumqttc_status_t rumqttc_client_start(
    const rumqttc_config_t *config,
    rumqttc_client_t **out,
    rumqttc_error_t **error_out);

rumqttc_status_t rumqttc_client_close(
    rumqttc_client_t *client,
    uint64_t timeout_ms,
    rumqttc_error_t **error_out);

rumqttc_status_t rumqttc_client_close_now(
    rumqttc_client_t *client,
    rumqttc_error_t **error_out);

void rumqttc_client_destroy(rumqttc_client_t *client);
```

`start` creates the dedicated driver and begins connection progress. `close` is
an idempotent graceful drain with a finite timeout. `close_now` is idempotent
and makes no delivery claim. `destroy` performs bounded immediate cleanup; it
must not leak a driver thread or block forever when the broker is unreachable.

Document thread safety. Prefer a client handle whose operation functions may be
called concurrently, while event receive is restricted to one active consumer.
Enforce rather than merely document the single-consumer rule.

### 5.2 Admission and completion

Provide separate APIs for nonblocking admission and tracked completion. A
representative publish surface is:

```c
rumqttc_status_t rumqttc_client_try_publish(
    rumqttc_client_t *client,
    rumqttc_string_view_t topic,
    rumqttc_bytes_view_t payload,
    const rumqttc_publish_options_t *options,
    uint64_t *operation_id_out,
    rumqttc_error_t **error_out);

rumqttc_status_t rumqttc_client_publish_tracked(
    rumqttc_client_t *client,
    rumqttc_string_view_t topic,
    rumqttc_bytes_view_t payload,
    const rumqttc_publish_options_t *options,
    rumqttc_completion_t **completion_out,
    rumqttc_error_t **error_out);
```

Add corresponding subscribe and unsubscribe operations. Do not block an
arbitrary caller forever waiting for request-channel capacity. If blocking
admission is added, it must accept a timeout.

Completion handles provide `poll`, `wait(timeout_ms)`, operation ID, result
kind, and destroy. Destroying an incomplete completion drops only the waiter;
it does not cancel admitted MQTT work. Result kinds must distinguish QoS 0
local flush, QoS 1 acknowledgement, QoS 2 completion, broker rejection,
failure before completion, and ambiguous timeout/transport outcome.

### 5.3 Event receive

Provide one pull interface with nonblocking and timed variants:

```c
rumqttc_status_t rumqttc_client_event_try_recv(
    rumqttc_client_t *client,
    rumqttc_event_t **event_out,
    rumqttc_error_t **error_out);

rumqttc_status_t rumqttc_client_event_recv(
    rumqttc_client_t *client,
    uint64_t timeout_ms,
    rumqttc_event_t **event_out,
    rumqttc_error_t **error_out);
```

Return a distinct `WOULD_BLOCK` status for an empty nonblocking receive rather
than conflating it with disconnect. Add event-kind and typed accessor
functions; do not expose a tagged union containing unstable Rust-derived
layouts. Accessors return borrowed views valid until event destruction.

Incoming publishes in manual-ack mode expose an opaque acknowledgement handle
or event-bound acknowledgement function backed by `AckToken`. Prevent double
acknowledgement and acknowledgement through the wrong client.

Do not add callbacks in ABI v1. If later required, callbacks must specify the
driver thread on which they run, prohibit or support reentrancy explicitly,
carry a caller context pointer, provide deregistration synchronization, and
guarantee that no callback begins after close/destroy returns.

## 6. Header usability and distribution

Make `rumqttc.h` valid as both C11 and C++ with `extern "C"` guards. Add
`RUMQTTC_API` visibility/import macros for Windows and ELF/Mach-O platforms.
Document static-link feature macros and the transitive system libraries needed
for each TLS provider.

Release:

- headers;
- shared and static libraries;
- debug symbols as separate artifacts where practical;
- checksums and provenance;
- `pkg-config` metadata;
- CMake package configuration; and
- platform-specific installation notes.

Define the initial platform matrix consistently with the Rust clients and CI.
Do not claim ABI support on a target that is only cross-compiled and never
loaded by a C smoke test.

## 7. Verification

Compile public-header tests with GCC, Clang, MSVC, and a C++ compiler. Build
with `-Wall -Wextra -Werror` or the platform equivalent. Add a real C test
program for:

- v4 and v5 connection;
- binary payloads containing zero bytes;
- QoS 0, 1, and 2 tracked completions;
- subscribe/event receive/unsubscribe;
- automatic and manual acknowledgements;
- invalid UTF-8, invalid enum values, null pointers, and inconsistent lengths;
- request backpressure and event overflow;
- reconnect after broker interruption;
- graceful close timeout and immediate close;
- destroying pending completion and event objects;
- concurrent publish from multiple native threads;
- repeated create/destroy without leaked threads or allocations; and
- every exported function with a `NULL` optional error output.

Add ABI checks that compare exported symbols and public type/function
signatures against a checked-in major-version baseline. Run AddressSanitizer,
UndefinedBehaviorSanitizer, and a leak checker on supported CI platforms. Use a
deterministic local broker fixture rather than a public service.

Recommended repository checks include:

```text
cargo fmt --all
cargo test -p rumqttc-wrapper-core
cargo test -p rumqttc-c
cargo clippy -p rumqttc-c --all-targets -- -D warnings
```

## Documentation and completion criteria

Document ownership beside every function, with examples for single-threaded
polling, multithreaded publishing, manual acknowledgement, tracked completion,
and shutdown. Explain queue admission versus MQTT completion and why timeout
does not prove non-delivery. Add the wrapper to `CHANGELOG.md` when it becomes
user-facing.

This TODO is complete when the checked-in C11 header and ABI-v1 library pass
the C/C++ compile, behavioral, sanitizer, symbol, concurrency, and leak tests on
every advertised platform, with no Rust layout or panic able to cross the ABI.
