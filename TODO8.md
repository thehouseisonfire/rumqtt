# Python and asyncio Wrapper

## Goal

Ship one native Python package backed by `rumqttc-v4-next` and
`rumqttc-v5-next`. Provide an idiomatic `asyncio` API with typed options,
awaitable MQTT-aware completions, asynchronous event consumption, bounded
memory behavior, and deterministic native cleanup.

Target CPython first through PyO3. Do not expose Rust futures, Tokio handles,
packet layouts, or the Rust extension module as the supported API. The Python
facade and its documented behavior are the compatibility contract.

Alternative interpreters, limited-API wheels, subinterpreters, and free-threaded
CPython are not supported merely because the extension imports. Advertise each
only after its complete lifecycle and concurrency suite passes.

## Current foundation and role in the wrapper plan

`native-wrappers/wrapper-core` and `native-wrappers/c` now implement the first
production native boundary. This wrapper must consume
`rumqttc-wrapper-core-next` for protocol selection, command admission, tracked
completion, event delivery, error classification, manual acknowledgements, and
client shutdown. Keep Python objects, `asyncio` integration, exceptions,
package metadata, and interpreter lifecycle handling in this wrapper.

This is the second native wrapper required by `TODO5.md`; `TODO5.md` is not an
implementation prerequisite for this document. Record genuine shared-boundary
gaps found here as tests or concrete `TODO11.md` work. Change the core only for
a correctness invariant or behavior now required by both the C and Python
wrappers. Do not duplicate MQTT lifecycle or v4/v5 translation in Python
callbacks.

## Proposed layout

```text
native-wrappers/python/
├── Cargo.toml
├── pyproject.toml
├── README.md
├── python/
│   └── rumqttc/
│       ├── __init__.py
│       ├── _client.py
│       ├── _events.py
│       ├── _types.py
│       └── py.typed
├── src/
│   ├── lib.rs
│   ├── client.rs
│   ├── completion.rs
│   ├── config.rs
│   ├── error.rs
│   ├── event.rs
│   └── runtime.rs
└── tests/
    ├── integration/
    ├── lifecycle/
    ├── typing/
    └── wheels/
```

Add the Rust crate to the independent `native-wrappers/Cargo.toml` workspace,
tentatively as package `rumqttc-python-next`. Do not add it to the repository's
main Cargo workspace.

Use PyO3 and `maturin` unless a compatibility spike demonstrates a concrete
blocker. Keep the compiled module private, for example `rumqttc._native`, and
export the supported surface from Python modules. This permits small ergonomic
and typing adapters without making generated PyO3 details public API.

Choose the minimum CPython version from maintained Python releases when
implementation begins. Initially build version-specific CPython wheels. Adopt
PyO3's GIL-enabled `abi3` limited API only after tests prove that it supports
every API used for futures, loop scheduling, exceptions, buffers, and
finalization; do not use it solely to reduce the wheel count. Treat the
free-threaded `abi3t` line separately and advertise it only after the
free-threaded lifecycle and concurrency matrix passes.

## 1. Define and freeze the initial Python API

Expose importable runtime types and matching type annotations. Prefer immutable
dataclasses and enums for boundary values, while allowing validated mappings
only where they materially improve ergonomics.

The initial public surface should include:

```python
from collections.abc import AsyncIterator, Sequence
from typing import Any

class MqttClient:
    def __init__(self, options: MqttClientOptions) -> None: ...

    async def connect(self) -> ConnectResult: ...

    async def enqueue_publish(
        self,
        topic: str,
        payload: bytes | bytearray | memoryview | str,
        options: PublishOptions | None = None,
    ) -> AdmissionResult: ...

    async def publish(
        self,
        topic: str,
        payload: bytes | bytearray | memoryview | str,
        options: PublishOptions | None = None,
    ) -> PublishCompletion: ...

    async def subscribe(
        self, subscriptions: Sequence[Subscription]
    ) -> SubscribeCompletion: ...

    async def unsubscribe(self, filters: Sequence[str]) -> UnsubscribeCompletion: ...
    def events(self) -> AsyncIterator[MqttEvent]: ...
    async def diagnostics(self) -> ClientDiagnostics: ...
    async def close(self, *, timeout: float | None = None) -> None: ...
    async def close_now(self) -> None: ...

    async def __aenter__(self) -> "MqttClient": ...
    async def __aexit__(self, *exc_info: Any) -> None: ...
```

`connect()` starts the shared driver and waits for the initial CONNACK.
`enqueue_publish()` completes on request-channel admission. `publish()`,
`subscribe()`, and `unsubscribe()` use tracked operations and complete only at
their documented MQTT milestones. Document QoS 0 as local transport flush, not
broker delivery; QoS 1 as PUBACK; and QoS 2 as the completed PUBCOMP exchange.

Do not name an admission-only method `publish`. Cancelling an `asyncio` task or
future drops only the Python waiter: it does not recall work already admitted
to the MQTT driver and does not prove that the broker did not receive it.

The asynchronous context manager calls `connect()` on entry and graceful
`close()` on exit. If graceful close fails or times out while another exception
is already propagating, preserve the original exception and attach or log the
shutdown failure according to a documented rule.

### 1.1 Options and Python values

Project the implemented `rumqttc-wrapper-core-next` configuration rather than
reconstructing options from an older TODO. The first release includes broker
host and port, client identifier, TCP/TLS/WebSocket/WSS transport, keep-alive,
connection timeout, username and byte-valued password, request and event
capacities, event-delivery timeout, acknowledgement mode, incoming-packet size
limit, outgoing-event control, and the distinct v4/v5 session settings.

Expose typed MQTT 5 outgoing PUBLISH properties, SUBSCRIBE packet properties,
per-filter subscription options, and UNSUBSCRIBE properties. Keep these scopes
distinct, as they are in the core. Validate:

- integers without accepting `bool` accidentally, and all Rust numeric ranges;
- finite, nonnegative timeout values before conversion to durations;
- protocol-specific fields instead of silently ignoring them;
- topic and topic-filter validity through the client library;
- TLS client-certificate/private-key pairing and transport-specific inputs; and
- nonzero finite channel capacities and timeouts.

Preserve the core's credential distinction: MQTT 3.1.1 requires a username when
a password is supplied, while MQTT 5 permits username-only, password-only, and
combined credentials. Do not expose an unbounded request or event channel; the
shared core intentionally requires finite nonzero capacities.

Accept `str` payloads as UTF-8 and bytes-like payloads as arbitrary bytes.
Acquire a `Py_buffer` only long enough to copy its contents during call
admission. Do not retain a borrowed `memoryview`, pointer into mutable Python
storage, or any Python object on the driver thread. Return incoming payloads as
immutable `bytes` in the first release; zero-copy views can be considered only
with an explicit lifetime and buffer-ownership contract.

Use `float` seconds for idiomatic public timeout parameters, with `None` meaning
the method's documented finite default or no caller deadline as appropriate.
Reject NaN, infinity, negative values, and values that overflow the internal
duration. State whether a zero timeout means a poll or immediate timeout for
each operation.

### 1.2 Events and manual acknowledgements

Expose documented event classes forming a closed type union:

```python
MqttEvent = (
    Connected
    | Disconnected
    | IncomingPublish
    | Outgoing
    | Closed
    | DriverError
)

@dataclass(frozen=True, slots=True)
class IncomingPublish:
    topic: str
    payload: bytes
    qos: QoS
    retain: bool
    duplicate: bool
    properties: V5PublishProperties | None
    acknowledgement: Acknowledgement | None
```

Give `Disconnected` a `phase: ConnectionPhase` field with `ATTEMPT` and
`ESTABLISHED` values. While the client remains running, both represent
recoverable failures and the core continues reconnecting. Map
`GracefulShutdownCompleted` to `Closed(graceful=True)`. Map terminal status from
a requested immediate close to `Closed(graceful=False)`; ordinary
`close_now()` must not appear as `DriverError`. Reserve `DriverError` for a
terminal failed driver.

Make `events()` a single-consumer asynchronous iterator in the first release.
Raise a stable state error when a second iterator is active. This avoids
undefined fan-out and prevents duplication of manual-ack responsibility.

In manual-ack mode, eligible publishes carry an `Acknowledgement` whose
`ack()` coroutine consumes the shared `AckToken` at most once. QoS 0 messages
must not expose an acknowledgement. Dropping an unacknowledged event does not
implicitly acknowledge it. Reject token reuse, use with another client, and
acknowledgement after terminal shutdown.

Do not add callback handlers or a background `async for` task hidden by the
library. A future callback adapter must define exception handling, slow-handler
backpressure, ordering, manual acknowledgements, and removal synchronization.

### 1.3 Exceptions

Export a stable exception hierarchy rooted at `MqttError`:

```python
class MqttError(Exception):
    code: str
    kind: ErrorKind
    operation_id: int | None
    broker_reason: int | None
    retryable: bool | None
    delivery: DeliveryStatus
    ambiguous: bool

class ConfigurationError(MqttError): ...
class BackpressureError(MqttError): ...
class ProtocolError(MqttError): ...
class BrokerRejectedError(MqttError): ...
class ClientClosedError(MqttError): ...
```

Keep exception text diagnostic and non-stable. Applications match `code`,
`kind`, and documented subclasses, not formatted Rust messages. Preserve MQTT
5 reason codes and relevant completion details as typed attributes. Attach
`operation_id` from the relevant `CompletionHandle`; a general driver error has
no operation identifier. Use built-in
`TypeError` for incorrect Python call shapes and `ValueError` for locally
invalid scalar values only when the distinction is clear; configuration and
runtime failures use the wrapper hierarchy consistently.

The current core exposes `ErrorKind`, `DeliveryStatus`, and broker reason but
does not yet expose a stable fine-grained error code or retryability. Before
freezing this API, add host-neutral machine-readable classification to the core
and use it from both C and Python. Refactor the C wrapper's current local
retryability mapping to consume the shared classification. At minimum,
event-buffer overflow and an internally caught panic must be distinguishable
without matching error text. Adding a C accessor for the fine-grained code is a
compatible pre-1.0 API addition.

Catch panics inside every PyO3 entry/task boundary. The core driver-thread
boundary must likewise convert a caught panic to an `INTERNAL_PANIC` terminal
driver error, fail outstanding futures, and shut down the affected client.
Never unwind through Python, leave the driver without terminal status, or abort
the interpreter as ordinary error handling.

## 2. Integrate with asyncio safely

### 2.1 Driver and event-loop ownership

Start `NativeClient` and its shared dedicated driver when `connect()` is first
awaited. Never run
`EventLoop::poll`, blocking channel receive, thread join, DNS, TLS, or broker
I/O on the Python event-loop thread. Do not install or reuse the application's
Tokio runtime.

Bind a client to the running `asyncio` loop used for its first asynchronous
operation. Reject later use from another loop with a stable error rather than
resolving futures on the wrong loop. Ordinary command methods may be invoked
by tasks on that loop; cross-thread callers must schedule through Python's
documented loop mechanisms themselves.

Make `connect()` idempotent while connected and coalesce concurrent initial
calls onto one pending connection result. Reject calls after closing begins.
Configuration, TLS construction, and driver-start failures raise immediately.
A recoverable connection-attempt failure emits `Disconnected(phase=ATTEMPT)`
while the coalesced connect future remains pending for a later CONNACK. If the
connect waiter is cancelled or has a caller deadline, document that this drops
only that wait and does not stop reconnection or the client; closing the client
is a separate operation.

Do not consume or hide the public `Connected` event merely to resolve
`connect()`. The current core has no independent connection waiter, and draining
the sole `EventConsumer` into an extra host queue changes the overload bound and
can deadlock initial connection behind unconsumed attempt events. Add a
host-neutral, repeatable connection observation in the core, with terminal
failure and shutdown wakeups, and use it for the connect future while leaving
the ordered event stream untouched. This is a correctness requirement, not a
Python convenience.

### 2.2 Future completion and cancellation

Create and complete `asyncio.Future` objects only while attached to the correct
interpreter and, on GIL-enabled builds, while holding the GIL. The Rust driver
sends owned Rust results through a native channel; a small bridge schedules
result delivery with the bound loop's thread-safe callback facility. Never call
arbitrary Python code directly from the MQTT driver thread.

Use the core's `CompletionHandle` as the authoritative, repeatable MQTT
operation state. Maintain only the host-delivery registry needed to associate
an `OperationId`, retained completion handle, and Python future. Remove the
Python future exactly once on result delivery, waiter cancellation, interpreter
shutdown, or terminal driver failure. If the Python waiter is cancelled after
admission, discard only its eventual Python result; retain only the native state
needed for the core to process the MQTT acknowledgement.

Manual acknowledgement has a narrower cancellation rule: if asynchronous
acknowledgement is cancelled while still waiting for request-channel capacity,
the core restores the token for retry. Once admission succeeds, cancellation
drops only the Python waiter and cannot recall the ACK.

Handle races among completion, `Future.cancel()`, loop closure, and client
shutdown without `InvalidStateError` leaking from a scheduling callback.
Scheduling onto a closed loop must transition native state to cleanup; it must
not retry forever or leave the driver blocked.

### 2.3 Event delivery and overload

Bridge the shared bounded event receiver into `__anext__()` without an
unbounded Python-side queue. Keep at most the documented bounded native events
plus one pending iterator future. Do not repeatedly schedule callbacks merely
to discover that no consumer is waiting.

Preserve the core's independent terminal-status path. On overflow, surface the
stable `EVENT_BUFFER_OVERFLOW` exception through the iterator and fail all
pending operations. Do not promise a post-failure diagnostics snapshot: the
core rejects new diagnostics after entering `Failed`. After overflow, require a
new client. An iterator cancelled while waiting must release its pending receive
slot without consuming or silently dropping the next event.

For MQTT 5, preserve capability-aware admission. Before the first CONNACK and
while reconnecting, asynchronous admission of QoS 1/2, retained, or Topic Alias
publishes waits for negotiated capabilities; a nonblocking admission API reports
transient backpressure. Alias-free, non-retained QoS 0 remains admissible.

Preserve event order. Once terminal closure is delivered, later `__anext__()`
calls raise `StopAsyncIteration`; terminal driver failure raises its documented
exception exactly once before iteration ends, unless the selected iterator
contract documents persistent failure instead.

## 3. Manage the GIL, objects, and interpreter shutdown

Keep all Python references out of `rumqttc-wrapper-core` and out of long-lived
driver state wherever possible. Native threads may manipulate only owned Rust
data until explicitly attaching to the correct interpreter for a short Python
delivery callback. On GIL-enabled builds that attachment includes holding the
GIL; do not encode GIL serialization as a general thread-safety invariant.

`close()` is idempotent and performs the graceful barrier using a finite
default timeout. `close_now()` is idempotent and requests immediate shutdown.
Neither blocks the event-loop thread while joining. Completion occurs only
after the bounded native join succeeds or returns a clear cleanup error.

`MqttClient.__del__` requests nonblocking immediate shutdown. It must not await,
join indefinitely, import modules, schedule new application work, or assume
that `asyncio` is still operational. Provide a best-effort warning for a client
garbage-collected while open, but suppress unsafe warning machinery during
interpreter finalization.

Register process/interpreter cleanup only through PyO3 and CPython facilities
whose ordering contract is understood. Before Python finalization destroys the
loop or module state:

- mark delivery state as unavailable;
- prevent new callbacks into Python;
- wake native command and completion waiters;
- request immediate shutdown of live drivers; and
- perform only a bounded join from a safe cleanup context.

Do not rely on daemon threads to make process exit appear successful. Test
normal process exit, `sys.exit()`, loop closure with a live client, garbage
collection cycles, module teardown, and abrupt child-process termination.

Treat each interpreter as owning independent module state. Do not claim
subinterpreter support until PyO3 supports the required module model and clients
can be created, used, destroyed, and the subinterpreter finalized repeatedly
without global Python references or callbacks crossing interpreter boundaries.
PyO3 supports free-threaded CPython, but that does not make this wrapper safe
automatically. Do not claim free-threaded support until the module declares the
correct GIL policy, all unsafe and synchronization assumptions are audited, and
the complete suite passes with concurrent calls and object destruction.

## 4. Package wheels and typing metadata

Publish one distribution name, tentatively `rumqttc`, containing the private
extension, Python facade, inline annotations, and `py.typed`. Do not require end
users to install Rust, Cargo, a C compiler, or system OpenSSL for supported
wheel combinations.

Build with `maturin` and publish wheels for the CPython versions and platforms
that CI can execute. The initial target matrix should include at least:

- Linux x86_64 and aarch64 using an appropriate manylinux baseline;
- Linux x86_64 musl using a declared musllinux baseline;
- macOS x86_64 and arm64 with an explicit minimum deployment target; and
- Windows x86_64.

Add Windows arm64 and other architectures only when CI can run import and MQTT
smoke tests, not merely cross-compile. If universal2 macOS wheels are produced,
test both slices. Audit Linux wheels for forbidden external dependencies and
verify wheel tags against their actual libc and Python ABI requirements.

An sdist may be provided for unsupported systems, but document that it requires
the Rust toolchain and native build prerequisites. Failure to find a compatible
wheel must produce an ordinary packaging error; never download an executable
from an install-time script or silently substitute a different Python MQTT
implementation.

Generate checksums and provenance in the release workflow. Test installation
into clean virtual environments using the minimum and newest supported `pip`,
and verify that the wheel contains `py.typed`, annotations, license files, and
the expected extension binary.

## 5. Verification matrix

Run the same behavior suite for MQTT 3.1.1 and MQTT 5:

- connect and CONNACK, including concurrent and repeated `connect()`;
- binary publish payloads and mutable buffers containing zero bytes;
- QoS 0, 1, and 2 tracked completion semantics;
- subscribe, incoming publish, and unsubscribe;
- automatic and manual acknowledgement, including double-ack rejection;
- reconnect after broker interruption;
- TLS verification and rejected certificates;
- bounded request backpressure and event-buffer overflow;
- graceful close, timeout, immediate close, and repeated close;
- cancellation before admission, after admission, and racing completion;
- cancellation of a pending `__anext__()` call;
- event-loop closure and interpreter exit with a live client;
- use from a different event loop and scheduling from another thread;
- repeated create/connect/close without native thread, task, or object leaks;
- panic containment through a test-only injected native failure; and
- exception attributes and reason-code preservation.

Run type-checking tests with pyrefly and lint/format checks with ruff, plus
runtime API tests confirming that annotations match exported names. Test
both `asyncio.run()` and manually created loops. If another event-loop policy
such as uvloop is advertised, execute the complete lifecycle suite under it
rather than assuming asyncio compatibility.

Build and install each wheel in a clean environment, then execute an import,
connect, publish, receive, and close smoke test on every host architecture
available in CI. Use a deterministic local broker fixture; release tests must
not depend on a public MQTT service.

Recommended repository checks should include the actually selected Python
versions and tools, for example:

```text
cargo fmt --manifest-path native-wrappers/Cargo.toml --all
cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next
cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-python-next
uv run maturin develop
uv run pytest
uv run ruff check
uv run ruff format --check
uv run pyrefly check
uv run maturin build --release
```

Run Rust clippy for the extension and use ruff for Python lint and formatting
selected by the repository. Add sanitizer or leak-check runs for the Rust
boundary where the Python build and platform support them, plus child-process
tests that can detect hung native threads at interpreter exit.

## Documentation and completion criteria

Document the supported Python implementations and versions, wheel platforms,
installation from wheels and source, `asyncio` loop affinity, admission versus
MQTT completion, cancellation ambiguity, continuous event consumption,
reconnect behavior, manual acknowledgements, TLS configuration, and graceful
versus immediate shutdown. Include examples using `async with`, a dedicated
event-consumer task, tracked publish, cancellation-safe application cleanup,
and MQTT 5 properties.

State unsupported behavior plainly: no synchronous facade, no callback API,
no cross-loop client use, and no subinterpreter, free-threaded, PyPy, or other
interpreter support until its matrix passes. Add the wrapper to `CHANGELOG.md`
when it becomes user-facing.

This TODO is complete when one typed Python distribution passes the shared MQTT
behavior, asyncio cancellation, bounded-memory, interpreter-shutdown, wheel,
and leak suites on every advertised CPython version and platform, with no Rust
panic, borrowed Python memory, or background callback able to cross an invalid
Python interpreter boundary.
