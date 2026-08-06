# Shared Native-Wrapper Support

## Goal

Add the smallest useful Rust support layer for the planned Python, JavaScript,
and C wrappers. The layer should remove repeated MQTT lifecycle and v4/v5
translation code, while leaving language-specific APIs, scheduling, memory
management, and packaging in their respective wrapper crates.

This is internal implementation infrastructure, not a promise that Rust types
or Rust's ABI will be exposed to foreign callers.

## Scope and non-goals

Create a workspace crate tentatively named `rumqtt-wrapper-core`. All native
wrappers should consume it unless implementation experience shows that a
particular wrapper cannot do so without compromising its host runtime.

The shared crate should provide only:

- a protocol-neutral, owned configuration model for commonly supported options;
- owned command, completion, event, and error values;
- one driver that continuously polls either the v4 or v5 event loop;
- bounded command and event delivery with explicit overload behavior; and
- deterministic shutdown and thread/task ownership.

Do not put the following in the shared crate:

- Python, Node-API, Deno, Bun, or C ABI types;
- JSON as an internal transport;
- a generic reflection or plugin framework;
- wrapper package discovery or dynamic-library loading;
- host callbacks, GIL handling, JavaScript handles, or C allocation APIs;
- a second MQTT state machine, codec, reconnect implementation, or session
  store format; or
- every field from both `MqttOptions` types merely to make the model complete.

Add an option only when at least two wrappers need the same semantics and the
translation is not trivial. Otherwise, keep it in the wrapper and construct
the underlying client option directly.

## Proposed crate layout

```text
rumqtt-wrapper-core/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── config.rs
│   ├── command.rs
│   ├── completion.rs
│   ├── driver.rs
│   ├── error.rs
│   ├── event.rs
│   ├── protocol.rs
│   └── shutdown.rs
└── tests/
    ├── lifecycle.rs
    ├── overload.rs
    ├── protocol_parity.rs
    └── shutdown.rs
```

Depend on the published package names and alias them because both library
targets are named `rumqttc`:

```toml
rumqttc_v4 = { package = "rumqttc-v4-next", path = "../rumqttc-v4" }
rumqttc_v5 = { package = "rumqttc-v5-next", path = "../rumqttc-v5" }
```

Keep this crate private (`publish = false`) until at least the C and JavaScript
wrappers have validated the boundary.

## Protocol support contract

`rumqtt-wrapper-core` and each native host-language wrapper should support MQTT
3.1.1 and MQTT 5 through one crate or distributed package. This is
dual-protocol package support, not a connection that speaks both versions.

Every client instance must:

- explicitly select exactly one protocol at construction;
- retain that selection for its complete lifetime;
- construct only the matching v4 or v5 client and event loop;
- reject options and commands that are invalid for the selected protocol; and
- require a new client instance to use another protocol.

Do not implement silent protocol fallback or automatic version negotiation.
CONNECT packet formats and session semantics differ, and retrying with another
version could change externally visible clean-session, clean-start, expiry, and
delivery behavior.

Normalize commands, completions, events, and errors only where semantics
genuinely overlap. Keep protocol-specific configuration and MQTT 5 properties
in tagged structures. Never silently discard an MQTT 5-only field when the
client selected MQTT 3.1.1, and never invent MQTT 5 reason information for an
MQTT 3.1.1 acknowledgement that does not carry it.

## 1. Define the owned boundary model

### 1.1 Protocol and configuration

Define an explicit protocol selector:

```rust,ignore
pub enum ProtocolVersion {
    V311,
    V5,
}
```

Define `ClientConfig` with the options required for a first useful release:

- protocol version;
- broker host and port;
- client identifier;
- TCP, TLS, WebSocket, or WSS transport;
- keep-alive and connection timeout;
- username and byte-valued password;
- clean-session/session-mode settings represented without lossy cross-version
  conversion;
- bounded request-channel capacity;
- inbound event-buffer capacity;
- automatic or manual acknowledgement mode;
- incoming packet-size limit; and
- optional TLS CA, client certificate, and private-key bytes.

Use owned `String`, `Bytes`, `Vec`, and duration/count values. Validate at the
shared boundary and then call the fallible v4/v5 option and client builders.
Return a structured configuration error; never call compatibility setters or
builders documented to panic.

Keep protocol-specific options in contained structs:

```rust,ignore
pub struct ClientConfig {
    pub common: CommonConfig,
    pub protocol: ProtocolConfig,
}

pub enum ProtocolConfig {
    V311(V311Config),
    V5(V5Config),
}
```

Do not invent a common meaning for MQTT 3.1.1 clean session and MQTT 5 clean
start/session expiry when their observable behavior differs.

### 1.2 Commands and operation identity

Represent commands as owned values. The initial set is:

- publish;
- subscribe to one or more filters;
- unsubscribe from one or more filters;
- acknowledge an incoming publish in manual-ack mode;
- request graceful disconnect with an optional timeout;
- request immediate disconnect; and
- request a diagnostics snapshot.

Assign a monotonically increasing, nonzero `OperationId(u64)` in the shared
driver. Do not use MQTT packet identifiers as foreign operation handles: packet
identifiers can be assigned later, reused, or hidden by reconnect/session logic.

Each admitted command must produce exactly one admission result. Commands that
request protocol completion must additionally produce exactly one terminal
completion result. Dropping a host-language future must drop only that waiter;
it must not imply cancellation or non-delivery of an MQTT packet.

### 1.3 Completion milestones

Expose MQTT-aware completion rather than a generic `success` boolean:

```rust,ignore
pub enum PublishCompletion {
    Qos0Flushed,
    Qos1Acknowledged,
    Qos2Completed,
}
```

Use the clients' tracked publish, subscribe, and unsubscribe notices. Preserve
broker rejection reason codes where the protocol exposes them. A timeout or
transport failure must be represented as ambiguous when the library cannot
prove whether bytes reached the broker.

Keep admission and completion distinct in names and types. In particular, a
successful send into the request channel must never be reported as delivery.

### 1.4 Events

Normalize only events that wrappers can use without understanding Rust packet
types. The initial `WrapperEvent` should include:

- connection established, including protocol version and session-present state;
- connection attempt or established connection lost, including a stable error
  category and diagnostic message;
- incoming publish, including topic bytes/string, payload bytes, QoS, retain,
  duplicate flag, and an optional opaque manual-ack token;
- outgoing activity summary when enabled by configuration;
- graceful shutdown completed; and
- event-buffer overflow/driver termination.

MQTT 5 publish properties should use an owned `V5PublishProperties` structure,
not JSON and not the codec's public Rust structure. Start with response topic,
correlation data, content type, payload format indicator, topic alias,
subscription identifiers, message expiry, and user properties. Add other
packet/property projections only alongside a demonstrated wrapper API use.

Manual acknowledgements must use an opaque `AckToken` created from the received
publish. Validate that a token belongs to the same live client and has not
already been consumed. Do not expose an API that accepts an arbitrary packet
identifier and acknowledgement kind.

## 2. Implement the shared driver

### 2.1 Ownership

The driver owns exactly one `EventLoop`, its Tokio runtime/task, completion
waiters, and the event producer. `ClientHandle` is cloneable and contains only
thread-safe command/control senders and driver status.

Support two embedding modes only if both are actually needed:

1. `Driver::run()` as an async future for wrappers that already own a suitable
   Tokio executor; and
2. `NativeClient::start()` as a dedicated named thread with a current-thread
   Tokio runtime for wrappers that cannot safely lend their executor.

Implement the dedicated-thread mode first because it gives C, Python, Node.js,
Deno, and Bun the same progress guarantee without blocking a host event-loop
thread. Build the underlying asynchronous client pair and continuously call
`EventLoop::poll`; do not repeatedly call the synchronous `Connection` from
host callbacks.

### 2.2 Reconnection and error classification

After every nonterminal `ConnectionError`, publish a disconnected/error event
and continue polling so the existing client reconnection behavior remains
active. Stop only for:

- `RequestsDone` after shutdown or all handles are gone;
- explicit wrapper shutdown;
- an unrecoverable wrapper invariant failure; or
- event-buffer overload as defined below.

Define a small stable `ErrorKind` owned by the wrapper core, such as
`Configuration`, `Admission`, `Backpressure`, `Network`, `Tls`, `Protocol`,
`Authentication`, `Persistence`, `Timeout`, `Shutdown`, and `Internal`.
Retain the Rust error chain for logging, but do not make downstream APIs match
on formatted Rust error strings.

### 2.3 Backpressure and event overload

Both request admission and host event delivery must be bounded by default.
Expose nonblocking admission and asynchronous/blocking admission separately so
each language wrapper can choose its idiomatic behavior.

Do not silently drop incoming publishes. Until the clients provide an API that
decouples application event consumption from protocol-critical progress, use
this conservative overload contract:

1. enqueue events into a configured bounded event channel;
2. reserve an independent one-shot terminal-status path that cannot be filled
   by ordinary events;
3. if the event channel remains full beyond a configurable wrapper delivery
   timeout, terminate the connection driver and record `EventBufferOverflow`;
4. wake all command and completion waiters with a terminal driver error; and
5. require creation of a new client before reconnecting.

This deliberately favors bounded memory and visible failure over hidden event
loss. Document that an application must continuously consume events. Revisit
the policy if the core clients later implement the notification/protocol
decoupling described in `TODO4.md`.

### 2.4 Shutdown

Implement idempotent states `Running`, `Closing`, `Closed`, and `Failed`.

- Graceful close stops new admissions, submits the client's graceful
  disconnect barrier, continues driving until completion or the supplied
  timeout, then joins the driver.
- Immediate close requests immediate disconnect, wakes pending host waits, and
  joins the driver without claiming queued operations completed.
- Dropping the last handle initiates immediate bounded shutdown; it must not
  leave a detached thread running indefinitely.
- Host finalizers may trigger shutdown but must never block a Python garbage
  collector or JavaScript event-loop thread waiting for an unbounded join.

Provide a separate bounded `join(timeout)` operation for wrapper cleanup code.

## 3. Keep host integration outside the shared crate

The Python wrapper should translate driver results into Python exceptions,
`asyncio.Future` objects, and an asynchronous event iterator. It owns GIL
acquisition and Python interpreter-finalization behavior.

The JavaScript wrapper should translate them into Node-API promises,
`Uint8Array`/`Buffer` objects, and an async iterator. It owns Node-API cleanup
hooks and thread-safe function usage.

The C wrapper should translate them into opaque handles, status codes, explicit
out-parameters, and caller-freed event/completion objects. It owns panic
containment and ABI versioning.

Do not add conditional `pyo3`, `napi`, or C header-generation features to
`rumqtt-wrapper-core`.

## 4. Verification

Add shared tests using deterministic mock broker or injected transport support:

- identical v4/v5 admission, publish, subscribe, unsubscribe, and shutdown
  lifecycle where protocol semantics overlap;
- QoS 0 flush, QoS 1 PUBACK, and QoS 2 PUBCOMP completion mapping;
- MQTT 5 negative acknowledgement mapping;
- reconnect continues after a recoverable polling error;
- dropping a completion waiter does not cancel admitted MQTT work;
- request-channel saturation returns explicit backpressure;
- event-buffer saturation terminates visibly without unbounded memory growth;
- manual-ack tokens reject reuse and cross-client use;
- graceful shutdown drains eligible tracked work;
- immediate shutdown does not report ambiguous work as completed; and
- repeated start/close cycles leave no driver threads behind.

Run at minimum:

```text
cargo fmt --all
cargo test -p rumqtt-wrapper-core
cargo test -p rumqttc-v4-next
cargo test -p rumqttc-v5-next
```

## Completion criteria

This TODO is complete when the shared crate is used by at least two wrappers,
contains no host-runtime dependencies, has explicit bounded-memory and shutdown
contracts, and demonstrably removes duplicated protocol/lifecycle logic. If a
proposed abstraction has only one consumer or merely renames a rumqttc type,
delete it and keep that code in the wrapper.
