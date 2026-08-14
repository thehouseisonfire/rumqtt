# C Wrapper Feature Parity

## Goal

Expose every native-client capability planned in `TODO15.md` through
`rumqttc-c-next` as a coherent C11/C++17 API. The C layer must translate the
owned wrapper-core model without weakening its validation, completion,
backpressure, protocol, security, or lifecycle guarantees.

This work begins feature by feature after the corresponding `WC-*` core API is
stable enough to bind. Do not bypass wrapper-core by constructing
`rumqttc-v4-next` or `rumqttc-v5-next` values directly in the C crate. If a
feature cannot be represented safely in wrapper-core, resolve that design in
`TODO15.md` first.

## ABI strategy

The current C package is `0.1.0-alpha`, but all work must follow
`docs/c-abi-compatibility.md` as if a published baseline may exist when a slice
lands.

- Prefer additive functions, constants, opaque handles, and new size-versioned
  option records.
- Never append fields to a public by-value record once a published ABI
  baseline fixes its size. Introduce a new nested record and setter, or advance
  the deliberate pre-stable ABI line with migration notes.
- Every new extensible input record begins with `struct_size`, has zeroed
  reserved fields, provides a `RUMQTTC_*_INIT` macro valid in C11 and C++17,
  accepts the documented older prefix size where possible, and rejects unknown
  nonzero reserved data.
- New enum-like values use fixed-width integers and named constants. Do not put
  a C enum in a public record or function signature where compiler-selected
  width would affect ABI.
- Every declared `rumqttc_*` function must be exported, and no undeclared
  accidental export may appear. Update generated and checked headers together.
- Compatible additions preserve loader identity and the existing ABI version.
  Incompatible changes require the package/ABI/loader transition and migration
  procedure in the compatibility policy.

Each feature slice must update the checked ABI contract, header/export checks,
mutation expectations if applicable, CMake/pkg-config packages, and historical
baseline comparison. “Header compiles” is not sufficient ABI evidence.

## Common C API conventions

### Status and error outputs

All fallible functions return the existing `uint32_t` status convention and
take `rumqttc_error_t **error` where the API can produce diagnostic detail.
On success, set `*error` to `NULL`. On failure, initialize every supplied out
parameter to its documented safe value before validation and return one owned
error or `NULL` only where the existing status contract explicitly permits it.

Extend error-kind/status/flag constants additively for persistence,
authentication, redirect, resolver, proxy, handshake, and callback failures.
Expose stable structured accessors for callback class, protocol, connection
phase/generation, operation ID, and broker reason whenever wrapper-core retains
them. Do not require callers to parse `rumqttc_error_message()` or the source
chain. Preserve delivery status for operation failures.

### Strings, bytes, arrays, and secrets

Input strings use `rumqttc_string_view_t`; arbitrary data uses
`rumqttc_bytes_view_t`. A zero length permits a null pointer, while a nonzero
length requires a valid readable region for the duration of the call. Copy all
configuration and admitted-operation input before returning unless the API is
explicitly a callback borrow.

Repeated values use `(pointer, count)` and preserve order and duplicates.
Check multiplication, count-to-Rust conversions, aggregate MQTT lengths, and
null elements before allocation. Returned event/error views remain borrowed
from their opaque owner and follow the existing copy-helper rules.

Passwords, private keys, authentication data, proxy credentials, cookies, and
authorization headers must be represented as bytes where appropriate, omitted
from debug/error text, and zeroized on release where wrapper-core's owner can
provide that guarantee. Documentation must not overclaim zeroization for
copies made by the OS, TLS library, broker client, or caller.

### Opaque ownership

Add a matching destroy function for every newly allocated config helper,
completion, token, callback context wrapper, or result owner. Destroy functions
accept `NULL`. Do not require callers to use `free()` on Rust allocations.

Configuration setters copy input and retain callback registrations until the
configuration is replaced/destroyed, transferred to a started client, or start
fails. Specify which owner holds each registration after
`rumqttc_client_start`. Destruction must not race mutation/start, consistent
with the existing config contract.

### Callback ABI

Use a vtable-style registration for every host callback family. Each vtable is
a size-versioned value containing:

- a `void *user_data` passed through unchanged;
- one or more function pointers using only fixed ABI types;
- a mandatory or optional `destroy(user_data)` callback invoked exactly once;
- reserved pointers/integers that must initially be zero; and
- explicit capability/version fields when methods are optional.

Never call `destroy` while another callback using the same `user_data` is
active. Callback owners need an internal reference count or equivalent quiesce
step. A callback may run on a wrapper driver/runtime thread and must not block
unless that callback family explicitly permits it. State whether calls can
overlap and whether calling client operations reentrantly is allowed.

For asynchronous callback families, use one uniform C completion model rather
than blocking the driver or inventing one model per feature. A recommended
shape is:

1. the callback receives an owned/borrowed request view plus an opaque
   `rumqttc_callback_completion_t *` valid until completed or cancelled;
2. it either completes synchronously or retains the completion using an
   explicit retain operation;
3. exactly one success/failure completion wins;
4. wrapper shutdown may mark it cancelled, after which late completion returns
   `RUMQTTC_INVALID_STATE` and has no effect; and
5. releasing the final reference never calls application code while holding a
   wrapper lock.

Provide cancellation notification or a query where expensive host work needs
to stop. Define behavior for callback reentry, duplicate completion, forgotten
completion, panic containment on the Rust side, and `destroy` during shutdown.
Native stress tests must exercise all of these cases.

### Lifecycle and threading

Preserve current client concurrency and single-event-consumer rules. New
configuration cannot be mutated after start. New operation completions are
repeatably observable and do not cancel admitted work when destroyed.

Graceful close must wait for or cancel callback work according to the
wrapper-core contract within its existing total timeout. Immediate close must
wake callback waits. `rumqttc_client_destroy_timeout_ms` retains ownership on
timeout; `rumqttc_client_abandon` releases host join ownership but must not
prematurely invoke callback `destroy` while the detached driver can call it.
Document the resulting shared-library unload restriction.

## Feature bindings

The following sections map one-to-one to `TODO15.md`.

### C-WC-01: Last Will and Testament

Add a size-versioned `rumqttc_last_will_t` with topic, payload, QoS, retain,
protocol selector, and optional pointer to
`rumqttc_v5_will_properties_t`. The v5 properties record contains presence
flags for optional scalars/views and an ordered User Property array.

Provide:

```c
rumqttc_config_set_last_will(rumqttc_config_t *,
                             const rumqttc_last_will_t *,
                             rumqttc_error_t **);
rumqttc_config_clear_last_will(rumqttc_config_t *, rumqttc_error_t **);
```

Use explicit presence flags so absent and present-empty values differ. Copy all
data. Reject v5 selectors on v4 at setter time when protocol is already known,
and repeat complete validation at start.

Acceptance requires initializer/header tests, null/count/size/reserved-field
tests, exact broker-observed v4/v5 will tests, graceful-no-will and
ungraceful-will tests, and replacement/clear ownership tests.

### C-WC-02: Durable client-session storage

Define a versioned `rumqttc_session_store_vtable_t` and registration handle.
Callbacks receive owned key/checkpoint views and complete through the common
asynchronous callback-completion API. Required operations are load, save, and
clear; load must distinguish not-found from failure. Include checkpoint format
version and protocol in request metadata rather than asking C code to inspect
opaque bytes.

Provide config functions to set/clear the store, set/clear the copied scope,
select the v5 broker-session-resume policy, and configure a maximum checkpoint
size. Decide whether the file-store workspace is exposed as a separately
linked built-in C adapter; do not make a path-valued “file store” setter the
only persistence API.

The vtable documentation must specify serialization of calls per key,
cross-client overlap, atomic-save expectation, callback timeout/cancellation,
one-active-client-per-key, and exactly-once destruction. Never invoke store
callbacks while holding a config/client/event lock.

Acceptance requires an in-memory native C store test, restart recovery for v4
and v5 QoS flows, corrupt/version/protocol/oversize cases, callback failure and
late completion, destroy/start-failure ownership, close/abandon stress, and an
optional file-store end-to-end consumer if that adapter ships.

### C-WC-03: Packet limits, batching, inflight limits, and throttling

Expose separate, unit-bearing setters rather than one omnibus record:

- maximum request batch and read batch;
- pending throttle in microseconds or nanoseconds with the unit in the symbol;
- v4 incoming/outgoing packet limits and inflight limit;
- v5 incoming limit mode/value, advertised Maximum Packet Size, and outgoing
  inflight upper limit; and
- clear/reset functions for optional/default/unlimited states.

Do not overload zero to mean all of default, adaptive, unlimited, and invalid.
Use constants for limit modes and explicit presence where the protocol uses an
optional nonzero field. Convert widths with checked arithmetic.

Acceptance requires table-driven FFI tests for every boundary and protocol
mismatch, backend/broker behavior tests, default compatibility, and C examples
showing resource-bounded configurations.

### C-WC-04: MQTT 5 CONNECT properties and topic aliases

Add a size-versioned `rumqttc_v5_connect_properties_t` plus
`rumqttc_config_set_v5_connect_properties` and a clear/reset operation. Include
presence flags for Session Expiry, Receive Maximum, Maximum Packet Size, Topic
Alias Maximum, Request Response Information, Request Problem Information,
Authentication Method/Data, and an ordered User Property array.

Expose automatic outgoing alias policy with fixed constants and a dedicated
setter. Keep it distinct from `rumqttc_v5_publish_properties_t.topic_alias`.
Setters copy values and reject invalid booleans, zeros, lengths, reserved
fields, and authentication combinations.

Acceptance requires exact CONNECT packet tests, every record-prefix version,
repeatable set/replace/clear tests, v4 rejection, reconnect alias reset tests,
and examples that explain advertised versus negotiated alias limits.

### C-WC-05: MQTT 5 enhanced authentication and reauthentication

Add a size-versioned authenticator vtable corresponding to wrapper-core's
selected callback or event/token authority. Its records must preserve exchange
kind, method, authentication data, reason code/string, and User Properties.
Authentication data and secrets are borrowed only for the callback duration
unless copied with an explicit helper.

Expose client-initiated reauthentication with both nonblocking admission and a
tracked completion, following existing naming:

```c
rumqttc_client_try_reauthenticate(...);
rumqttc_client_reauthenticate_tracked(...);
```

Add completion and event kind constants plus typed accessors. If event-driven
challenge response is selected, expose an opaque, generation-bound auth token
and a response operation; tokens must reject reuse, cross-client use, stale
generation, and completion after timeout. If configured-callback mode is
selected, do not also emit a consumable token.

An optional SCRAM convenience API may use an opaque credential/config handle,
but raw callback support remains the mechanism-neutral foundation. Never emit
passwords, client proofs, or authentication data in errors or traces.

Acceptance requires C broker tests for initial/multi-step/reauth flows,
tracked completion, broker rejection, overlap, timeout, callback failure,
duplicate/late completion, reconnect, close/abandon, redaction, and SCRAM when
enabled.

### C-WC-06: MQTT 5 redirects and DNS SRV

Expose fixed redirect policies through values and application policy through a
versioned asynchronous callback vtable. Add a resolver vtable returning an
owned array of priority, weight, port, and target records through the common
completion mechanism. Provide a system-resolver selector only when the library
was built with that capability, with an API to query build capabilities.

Add redirect event accessors for reason, source, advertised Server Reference,
decision, selected endpoint, and attempt/loop metadata. Views are owned by the
event. Copy helpers cover values needed after event destruction.

Acceptance requires deterministic native tests for all reference forms,
weighted/empty/failing DNS, redirect rejection/loops, concurrent shutdown,
late callbacks, missing build features, event lifetime, and session-store
scope interaction.

### C-WC-07: Proxy transports

Add a size-versioned `rumqttc_proxy_options_t` with a fixed protocol selector,
host, port, DNS policy, and optional username/password bytes. Provide set and
clear functions. Keep broker endpoint, proxy endpoint, broker TLS, and proxy
security fields unambiguous; add a separate TLS record if HTTPS proxy support
exists rather than reusing broker TLS fields.

Expose compile-time capabilities through `rumqttc_library_capabilities()` or
equivalent fixed bit flags. Configuring a proxy unsupported by the loaded
library returns a structured unsupported-feature error and never connects
directly.

Acceptance requires HTTP/SOCKS fixture tests for both MQTT versions and all
transport compositions, auth and DNS behavior, redaction, reconnect, timeout,
and static/shared package consumers with each supported feature build.

### C-WC-08: Unix sockets and custom socket connectors

Add an explicit Unix broker setter taking a path byte/string view according to
platform path rules; do not reinterpret `rumqttc_config_set_broker` host/port.
Return unsupported-platform before start on non-Unix targets and document path
encoding.

If wrapper-core ships custom streams, define opaque
`rumqttc_async_stream_t`/connector vtables with partial read/write, flush,
shutdown, cancellation, and exactly-once destroy. The stream API must support
concurrent read and write if the Rust transport does. Never call a blocking C
read on the single wrapper runtime thread unless the contract dispatches it to
a dedicated blocking executor.

Ship Unix binding independently; custom connectors remain gated until native
stress tests prove wakeup and destruction safety. Acceptance mirrors WC-08 and
adds C header tests for platform declarations or portable runtime rejection.

### C-WC-09: WebSocket handshake customization

For the common case, expose a copied array of header name/value byte views and
an operation enum for add/replace/remove. Validate protected headers and
sensitive values. For dynamic behavior, add a fallible vtable using the common
async callback model and an owned handshake request/result representation.

Do not expose `http::Request` layout. State which headers are canonicalized,
whether duplicates retain order, and how redirect/reconnect reinvokes the
modifier. Authorization/cookie/header secrets must be redacted.

Acceptance requires C WebSocket fixture tests for declarative and callback
paths, invalid/protected headers, WSS/proxy composition, callback cancellation,
and disabled-feature behavior.

### C-WC-10: MQTT 5 DISCONNECT reason and properties

Add a size-versioned `rumqttc_disconnect_options_t` containing a protocol
selector and optional v5 properties record. Introduce `_with_options` variants
of graceful and immediate close rather than changing existing function
signatures. Preserve timeout units in symbol names and keep old functions as
version-neutral convenience calls.

Document first-admitted-options-wins behavior for coalesced close and return a
structured conflict error for later incompatible options. Copy inputs before
admission.

Acceptance requires exact packet tests, v4 mismatch, invalid reason/property
validation, concurrent close option conflicts, escalation, timeout, repeatable
completion, and ABI checks proving old close signatures are unchanged.

### C-WC-11: Rich events

Add event kinds and accessors for full connection acknowledgement detail,
broker disconnect, authentication, and redirect. Extend outgoing-event access
through new accessors for packet identifier and operation ID where present;
do not change the existing accessor's signature or constants.

Prefer nested opaque event data accessed by functions over large public
by-value output records. Every accessor initializes supplied outputs, permits
documented optional outputs, rejects the wrong event kind, and returns views
owned by `rumqttc_event_t`. Repeated v5 properties use count/at accessors with
stable ordering.

Acceptance requires accessor-kind matrices, absent/present-empty fields,
wrong-kind/null-output behavior, owner lifetime/copy-helper tests, queue
backpressure with large events, and native broker coverage for every mapped
underlying event class.

### C-WC-12: Network options, observability, and capabilities

Expose portable network settings through unit-bearing setters and
platform-specific settings through portable functions that return
unsupported-platform when unavailable. Avoid platform-native structs such as
`sockaddr` in the stable ABI unless their exact cross-platform representation
is independently versioned.

Add a library capability bitset covering protocol versions, TLS backend,
WebSocket, proxy kinds, Unix sockets, system SRV, SCRAM, tracing, session-store
callbacks, custom connectors, and dynamic modifiers. Capability bits describe
the loaded artifact, not whether a particular broker negotiated a feature.

Tracing is configured at library build/integration level unless wrapper-core
provides a safe per-client sink. Do not install or replace a process-global
subscriber implicitly from a config setter. Document how embedding
applications connect Rust tracing/log output and guarantee secret/payload
redaction.

Acceptance requires platform builds and tests, capability/header consistency,
unknown-bit forward compatibility, socket behavior tests, and captured-output
redaction tests.

### C-WC-13: TLS backend and credential policy

Add a size-versioned `rumqttc_tls_options_t` with explicit backend and root
policy selectors. Use distinct nested records for rustls PEM certificate/key
identity and native-tls PKCS#12 identity/password. Represent ALPN as an ordered
array of byte views. Update TLS and WSS setters to accept this record through
new `_with_options` functions; retain existing functions as compatibility
conveniences with their current rustls behavior.

Never infer a backend from input encoding. Capability queries must report the
available TLS backends, and unsupported selections fail before network access.
All credential material is copied during the setter and released with its
configuration/client owner. Password and private identity bytes are secret and
must never be returned through error accessors or traces.

Acceptance requires C fixtures for platform/custom trust, PEM and PKCS#12
mutual TLS, ALPN, WSS, malformed input, hostname rejection, mixed/disabled
backend builds, replacement/clear, failed-start cleanup, redaction, and ABI
proof that existing TLS setter signatures remain unchanged.

## Documentation requirements

Every feature slice must update:

- `native-wrappers/c/include/rumqttc.h`, including ownership, thread safety,
  callback, unit, presence, and borrowed-view comments;
- `native-wrappers/c/README.md` with capability discovery and at least one
  complete use pattern for substantial features;
- `native-wrappers/c/examples/` when the feature changes ordinary client
  setup or operation flow;
- `CHANGELOG.md` with C ABI additions, build-feature changes, and migrations;
- installed CMake and pkg-config documentation/options where artifact
  capabilities vary; and
- `TODO15.md`/`TODO16.md` parity status or the eventual checked-in parity
  matrix.

Examples must be warning-clean C11, compile as C++17 where the header promises
it, check every fallible return, destroy all owners, and avoid public brokers.
Callback examples must demonstrate shutdown and exactly-once context cleanup,
not only the success callback.

## Testing and CI requirements

For each slice, extend all applicable layers:

1. Rust FFI behavior tests for null pointers, invalid UTF-8, count/pointer
   pairs, integer overflow, struct prefixes, selectors, reserved fields,
   output initialization, error ownership, and panic containment.
2. Wrapper-core protocol tests proving the C translation neither loses nor
   invents fields.
3. Native C integration tests against deterministic broker, proxy, DNS,
   WebSocket, authentication, and persistence fixtures.
4. Native stress tests for concurrent client operations, one event receiver,
   callback overlap, callback completion/cancellation, close, destroy timeout,
   abandonment, and shared-library unload rules.
5. Header smoke tests under C11 and C++17 with warnings as errors.
6. Static and shared CMake consumers plus pkg-config consumers on Linux,
   macOS, and Windows where applicable.
7. Header/export/loader/ABI-contract checks and authenticated historical
   comparison for published lines.
8. Feature-build jobs for each independently selectable backend capability and
   selected combinations, including a minimal build that proves unsupported
   configuration fails safely.

Use sanitizers for native callback/ownership tests where supported. Run Miri
against Rust-side vtable ownership if unsafe code is introduced. Add fault
injection for allocation failure where practical, callback never-completes,
duplicate completion, broker disconnect, corrupt checkpoint, DNS/proxy failure,
and shutdown at every callback transition.

At minimum, the completed C workspace must pass:

```bash
cargo fmt --manifest-path native-wrappers/Cargo.toml --all --check
cargo check --manifest-path native-wrappers/Cargo.toml --workspace
cargo test --manifest-path native-wrappers/Cargo.toml --workspace
native-wrappers/c/tests/abi/check.sh ffi-header
native-wrappers/c/tests/abi/check.sh exports
native-wrappers/c/tests/abi/compare-release.sh
```

Run the repository's CMake/CTest native integration and package-consumer suites
on every supported OS as documented in `native-wrappers/c/README.md`.

## Delivery order and definition of done

Bind features in the same order as `TODO15.md`:

1. capability query, ABI helpers, common async callback completion, and
   redaction/error foundations;
2. value-only configuration and operations (C-WC-01, C-WC-03, C-WC-04,
   C-WC-07, Unix C-WC-08, declarative C-WC-09, C-WC-10, value C-WC-12);
3. session storage (C-WC-02);
4. enhanced authentication (C-WC-05);
5. redirect/SRV and dynamic transport hooks (C-WC-06, remaining C-WC-08 and
   C-WC-09);
6. rich events and observability (C-WC-11 and remaining C-WC-12);
7. TLS backend/credential completion (C-WC-13); and
8. final API-coherence, ABI, package, and parity review.

One feature is complete at the C boundary only when:

- its `WC-*` acceptance criteria pass in wrapper-core;
- all supplied values survive C-to-core conversion exactly;
- the C API has documented ownership, concurrency, cancellation, lifecycle,
  units, presence, and error semantics;
- every allocated owner and callback context is released exactly once on
  success, failed start, graceful close, immediate close, destroy timeout,
  driver failure, and abandonment;
- old ABI contracts remain compatible or a deliberate new ABI line is fully
  documented and packaged;
- native tests exercise success, invalid input, failure, timeout, and shutdown;
  and
- headers, examples, README, `CHANGELOG.md`, CMake/pkg-config metadata, exports,
  and ABI manifests agree with the shipped library.

Overall C parity is complete only when every WC-01 through WC-13 item is bound
or carries the same reviewed `intentionally omitted` rationale as the core
parity matrix. A C-only omission must not be hidden by claiming wrapper-core
parity.
