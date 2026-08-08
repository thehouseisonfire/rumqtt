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

## Prerequisite

Implement the useful portions of `TODO5.md` first, especially the owned event
model, tracked completions, dedicated driver, bounded event delivery, and
deterministic shutdown. Do not duplicate MQTT lifecycle or v4/v5 translation
inside Python callbacks.

## Proposed layout

```text
rumqtt-python/
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

Use PyO3 and `maturin` unless a compatibility spike demonstrates a concrete
blocker. Keep the compiled module private, for example `rumqttc._native`, and
export the supported surface from Python modules. This permits small ergonomic
and typing adapters without making generated PyO3 details public API.

Choose the minimum CPython version from maintained Python releases when
implementation begins. Initially build version-specific CPython wheels. Adopt
PyO3's `abi3` limited API only after tests prove that it supports every API used
for futures, loop scheduling, exceptions, buffers, and finalization; do not use
`abi3` solely to reduce the wheel count.

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

Support the common first-release options from `TODO5.md`, with contained v4
and v5 option types for protocol-specific behavior. Validate:

- integers without accepting `bool` accidentally, and all Rust numeric ranges;
- finite, nonnegative timeout values before conversion to durations;
- protocol-specific fields instead of silently ignoring them;
- topic and topic-filter validity through the client library;
- mutually exclusive TLS credential sources; and
- explicit opt-in for an unbounded request channel, if exposed at all.

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
    retryable: bool | None
    ambiguous: bool | None

class ConfigurationError(MqttError): ...
class BackpressureError(MqttError): ...
class ProtocolError(MqttError): ...
class BrokerRejectedError(MqttError): ...
class ClientClosedError(MqttError): ...
```

Keep exception text diagnostic and non-stable. Applications match `code`,
`kind`, and documented subclasses, not formatted Rust messages. Preserve MQTT
5 reason codes and relevant completion details as typed attributes. Use built-in
`TypeError` for incorrect Python call shapes and `ValueError` for locally
invalid scalar values only when the distinction is clear; configuration and
runtime failures use the wrapper hierarchy consistently.

Catch panics inside every native task boundary. Convert them to an
`INTERNAL_PANIC` terminal driver error, fail outstanding futures, and shut down
the affected client. Never unwind through Python or abort the interpreter as
ordinary error handling.

## 2. Integrate with asyncio safely

### 2.1 Driver and event-loop ownership

Start the shared dedicated driver when `connect()` is first awaited. Never run
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
Distinguish initial connection failure from a recoverable disconnection after
a successful connection so the connect future and event stream do not report
contradictory states.

### 2.2 Future completion and cancellation

Create and complete `asyncio.Future` objects only while holding the GIL on a
valid interpreter thread. The Rust driver sends owned Rust results through a
native channel; a small bridge schedules result delivery with the bound loop's
thread-safe callback facility. Never call arbitrary Python code directly from
the MQTT driver thread.

Maintain a registry from shared `OperationId` to Python completion state.
Remove entries exactly once on completion, cancellation of the waiter,
interpreter shutdown, or terminal driver failure. If the Python waiter is
cancelled after admission, discard only its eventual result. Keep enough native
state to process the MQTT acknowledgement correctly without retaining the
cancelled future indefinitely.

Handle races among completion, `Future.cancel()`, loop closure, and client
shutdown without `InvalidStateError` leaking from a scheduling callback.
Scheduling onto a closed loop must transition native state to cleanup; it must
not retry forever or leave the driver blocked.

### 2.3 Event delivery and overload

Bridge the shared bounded event receiver into `__anext__()` without an
unbounded Python-side queue. Keep at most the documented bounded native events
plus one pending iterator future. Do not repeatedly schedule callbacks merely
to discover that no consumer is waiting.

Apply the `TODO5.md` terminal overflow policy and surface
`EVENT_BUFFER_OVERFLOW` through the iterator, diagnostics, and all pending
operations. After overflow, require a new client. An iterator cancelled while
waiting must release its pending receive slot without consuming or silently
dropping the next event.

Preserve event order. Once terminal closure is delivered, later `__anext__()`
calls raise `StopAsyncIteration`; terminal driver failure raises its documented
exception exactly once before iteration ends, unless the selected iterator
contract documents persistent failure instead.

## 3. Manage the GIL, objects, and interpreter shutdown

Keep all Python references out of `rumqtt-wrapper-core` and out of long-lived
driver state wherever possible. Native threads may manipulate only owned Rust
data until explicitly entering a short Python delivery callback.

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
subinterpreter support until clients can be created, used, destroyed, and the
subinterpreter finalized repeatedly without global PyObject references or
callbacks crossing interpreter boundaries. Similarly, do not claim support for
free-threaded CPython until PyO3 supports the chosen mode and the suite passes
with concurrent calls and object destruction; retaining a GIL-era safety
assumption is not acceptable.

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

Run type-checking tests with the selected supported versions of mypy and pyright
and runtime API tests confirming that annotations match exported names. Test
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
cargo fmt --all
cargo test -p rumqtt-wrapper-core
cargo test -p rumqtt-python
maturin develop
python -m pytest
python -m mypy python/rumqttc tests/typing
python -m pyright python/rumqttc tests/typing
maturin build --release
```

Run Rust clippy for the extension and use Python linters/formatters selected by
the repository. Add sanitizer or leak-check runs for the Rust boundary where
the Python build and platform support them, plus child-process tests that can
detect hung native threads at interpreter exit.

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
