# Node-API JavaScript and TypeScript Wrapper

## Goal

Ship one native JavaScript/TypeScript client backed by `rumqttc-v4-next` and
`rumqttc-v5-next`. Target Node.js directly through stable Node-API, and support
local Deno and Bun runtimes through their Node-API compatibility layers.

Do not create independent Deno and Bun native implementations initially. The
runtime implementation language is not a supported extension boundary; Rust
has no stable cross-project ABI. Treat Node-API behavior and the wrapper's test
matrix as the compatibility contract.

### Protocol packaging contract

Ship one JavaScript/TypeScript package and one `MqttClient` type supporting both
MQTT 3.1.1 and MQTT 5. Each `MqttClient` instance explicitly selects one
protocol in its construction options, and that selection is immutable for the
instance's lifetime. Using another protocol requires constructing another
client.

Do not auto-negotiate, silently fall back between versions, or retry a rejected
connection with a different protocol. Do not expose separate v4-only and
v5-only npm packages unless measured artifact constraints later justify them.
Protocol-specific convenience classes may be thin adapters over the same
implementation, but they must retain identical lifecycle, completion,
backpressure, and shutdown behavior.

Reject protocol-incompatible options before starting the native client. In
particular, reject MQTT 5 properties and session settings for an MQTT 3.1.1
instance instead of ignoring them. Preserve MQTT 3.1.1 clean-session and MQTT 5
clean-start/session-expiry semantics as distinct TypeScript option shapes.

Browser JavaScript, Web Workers, Deno Deploy, and other sandboxes that cannot
load native addons are out of scope. Track browser/WASM transport support as a
separate project.

## Current foundation and role in the wrapper plan

`native-wrappers/wrapper-core` and `native-wrappers/c` now implement the first
production native boundary. This wrapper must consume
`rumqttc-wrapper-core-next` for protocol selection, command admission, tracked
completion, event delivery, error classification, manual acknowledgements, and
client shutdown. Keep Node-API values, promises, environment state, package
loading, and JavaScript ergonomics in this wrapper.

This is the second native wrapper required by `TODO5.md`; `TODO5.md` is not an
implementation prerequisite for this document. Record genuine shared-boundary
gaps found here as tests or concrete `TODO11.md` work. Change the core only for
a correctness invariant or behavior now required by both the C and JavaScript
wrappers.

## Proposed layout

```text
native-wrappers/js/
├── Cargo.toml
├── package.json
├── README.md
├── npm/
│   ├── linux-x64-gnu/
│   ├── linux-x64-musl/
│   ├── linux-arm64-gnu/
│   ├── darwin-x64/
│   ├── darwin-arm64/
│   ├── win32-x64-msvc/
│   └── win32-arm64-msvc/
├── src/
│   ├── lib.rs
│   ├── client.rs
│   ├── config.rs
│   ├── completion.rs
│   ├── error.rs
│   └── event.rs
├── js/
│   ├── index.ts
│   ├── loader.ts
│   └── types.ts
└── tests/
    ├── shared/
    ├── node/
    ├── deno/
    └── bun/
```

Add the Rust crate to the independent `native-wrappers/Cargo.toml` workspace,
tentatively as package `rumqttc-js-next`. Do not add it to the repository's
main Cargo workspace.

Use `napi-rs` and `napi-build` unless a compatibility spike demonstrates a
specific unsupported Node-API operation. Pin an explicit stable Node-API level
supported by the minimum Node.js release and verified in Deno and Bun. Do not
call V8, JavaScriptCore, libuv, `deno_core`, or Bun internal APIs directly.

## 1. Define and freeze the initial TypeScript API

Export ESM and CommonJS-compatible entry points from one npm package. Prefer an
ESM-first TypeScript declaration surface that does not depend on Node-only
types for ordinary use. Accept `Uint8Array`; additionally accept and return
`Buffer` under Node.js/Bun without requiring Deno users to import it.

The initial public surface should include:

```ts
export type ProtocolVersion = "3.1.1" | "5.0";
export type QoS = 0 | 1 | 2;

export type ProtocolOptions =
  | {
      protocol: "3.1.1";
      cleanSession?: boolean;
    }
  | {
      protocol: "5.0";
      cleanStart?: boolean;
      sessionExpiryInterval?: number;
    };

export type MqttClientOptions = CommonMqttClientOptions & ProtocolOptions;

export class MqttClient {
  constructor(options: MqttClientOptions);

  connect(): Promise<ConnectResult>;

  enqueuePublish(
    topic: string,
    payload: Uint8Array | string,
    options?: PublishOptions,
  ): Promise<AdmissionResult>;

  publish(
    topic: string,
    payload: Uint8Array | string,
    options?: PublishOptions,
  ): Promise<PublishCompletion>;

  subscribe(filters: Subscription[]): Promise<SubscribeCompletion>;
  unsubscribe(filters: string[]): Promise<UnsubscribeCompletion>;
  events(): AsyncIterable<MqttEvent>;
  diagnostics(): Promise<ClientDiagnostics>;
  close(options?: CloseOptions): Promise<void>;
  closeNow(): Promise<void>;
}
```

`enqueuePublish` resolves on request-channel admission. `publish`, `subscribe`,
and `unsubscribe` use tracked operations and resolve only at their documented
MQTT milestones. Document QoS 0 as local transport flush, not broker delivery;
QoS 1 as PUBACK; and QoS 2 as completed PUBCOMP exchange.

Do not name an admission-only operation `publish` without qualification. Do
not reject a JavaScript promise on cancellation and then claim the MQTT packet
was cancelled: dropping/aborting a waiter does not recall already admitted
network work.

### 1.1 Options

Project the implemented `rumqttc-wrapper-core-next` configuration rather than
reconstructing options from an older TODO. The first release includes broker
host and port, client identifier, TCP/TLS/WebSocket/WSS transport, keep-alive,
connection timeout, username and byte-valued password, request and event
capacities, event-delivery timeout, acknowledgement mode, incoming-packet size
limit, outgoing-event control, and the distinct v4/v5 session settings.

Expose typed MQTT 5 outgoing PUBLISH properties, SUBSCRIBE packet properties,
per-filter subscription options, and UNSUBSCRIBE properties. Keep these scopes
distinct, as they are in the core. Make the protocol portion a discriminated
union so TypeScript rejects incompatible options before runtime where possible.
Native validation remains authoritative for plain JavaScript callers and values
crossing the Node-API boundary. Validate:

- finite integers and numeric ranges before converting to Rust integers;
- protocol-specific options instead of silently ignoring them;
- topic and topic-filter validity through the client library;
- TLS client-certificate/private-key pairing and transport-specific inputs; and
- nonzero finite channel capacities and timeouts.

Preserve the core's credential distinction: MQTT 3.1.1 requires a username when
a password is supplied, while MQTT 5 permits username-only, password-only, and
combined credentials. Do not expose an unbounded request or event channel; the
shared core intentionally requires finite nonzero capacities.

Copy mutable JavaScript inputs at call admission. Do not retain a borrowed view
of an `ArrayBuffer` across an await or native-thread handoff.

### 1.2 Events and manual acknowledgements

Expose a discriminated union:

```ts
export type MqttEvent =
  | { type: "connected"; protocol: ProtocolVersion; sessionPresent: boolean }
  | {
      type: "disconnected";
      phase: "attempt" | "established";
      error: MqttError;
      reconnecting: true;
    }
  | { type: "publish"; message: IncomingMessage }
  | { type: "outgoing"; packet: OutgoingSummary }
  | { type: "closed"; graceful: boolean }
  | { type: "driverError"; error: MqttError };
```

Map the core event model explicitly. `ConnectionPhase::Attempt` means a
connection attempt failed before any successful CONNACK; `Established` means a
previously established connection was lost. While the client remains running,
both are recoverable and the core continues reconnecting. Map
`GracefulShutdownCompleted` to `{ type: "closed", graceful: true }`. Map the
terminal status caused by a requested immediate close to
`{ type: "closed", graceful: false }`; do not report ordinary `closeNow()` as a
driver error. Reserve `driverError` for a terminal failed driver.

Make `events()` a single-consumer async iterator in the first release. Reject a
second active iterator explicitly. This avoids undefined fan-out semantics and
prevents accidental duplication of manual-ack responsibility.

For manual-ack mode, attach an `ack(): Promise<void>` method or opaque ack
object to eligible incoming messages. It must consume the shared `AckToken` at
most once. QoS 0 messages must not expose an acknowledgement operation.

### 1.3 Errors

Export `MqttError extends Error` with stable fields:

```ts
class MqttError extends Error {
  readonly code: string;
  readonly kind: MqttErrorKind;
  readonly operationId?: bigint;
  readonly brokerReason?: number;
  readonly retryable?: boolean;
  readonly delivery: MqttDeliveryStatus;
  readonly ambiguous: boolean;
}
```

Keep `message` diagnostic and non-stable. Map behavior using `code` and `kind`,
not formatted Rust error text. Preserve broker reason codes in typed completion
errors. Attach `operationId` from the relevant `CompletionHandle`; a general
driver error has no operation identifier.

The current core exposes `ErrorKind`, `DeliveryStatus`, and broker reason but
does not yet expose a stable fine-grained error code or retryability. Before
freezing this API, add host-neutral machine-readable classification to the core
and use it from both C and JavaScript. Refactor the C wrapper's current local
retryability mapping to consume the shared classification. At minimum,
event-buffer overflow and an internally caught panic must be distinguishable
without matching error text. Adding a C accessor for the fine-grained code is a
compatible pre-1.0 API addition.

Catch panics at every Node-API entry/task boundary. The core driver-thread
boundary must likewise convert a caught panic into an `INTERNAL_PANIC` terminal
error, fail outstanding completions, and shut down. Never unwind through
Node-API or leave the core driver thread without publishing terminal status.

## 2. Implement Node-API integration safely

### 2.1 Runtime and promise delivery

Start `NativeClient` and its shared dedicated driver when `connect()` is first
called. Never run
`EventLoop::poll`, blocking channel receive, thread join, DNS, or TLS work on
the JavaScript event-loop thread.

Make `connect()` idempotent while connected and coalesce concurrent initial
calls onto one pending connection result. Reject calls made after closing has
started. Configuration, TLS construction, and driver-start failures reject the
promise immediately. A recoverable connection-attempt failure produces a
`disconnected` event with `phase: "attempt"` while the coalesced connect promise
remains pending for a later CONNACK.

Do not consume or hide the public `Connected` event merely to resolve
`connect()`. The current core has no independent connection waiter, and draining
the sole `EventConsumer` into an extra host queue changes the overload bound and
can deadlock initial connection behind unconsumed attempt events. Add a
host-neutral, repeatable connection observation in the core, with terminal
failure and shutdown wakeups, and use it for the connect promise while leaving
the ordered event stream untouched. This is a correctness requirement, not a
JavaScript convenience.

Use Node-API async work, deferred promises, or thread-safe functions only in
their documented modes. All creation and resolution of JavaScript values must
occur on an allowed JavaScript thread with a valid environment. The Rust driver
may pass only owned Rust data across threads.

Use the core's `CompletionHandle` as the authoritative, repeatable MQTT
operation state. Keep only the host-delivery registry needed to associate an
`OperationId`, retained completion handle, and JavaScript deferred promise.
Remove host entries exactly once on delivery, environment shutdown, or terminal
driver failure. JavaScript garbage collection of a promise drops only its host
waiter; it must not cancel or remove the core's admitted MQTT operation.

### 2.2 Event delivery and overload

Bridge the shared bounded event receiver into the async iterator without an
unbounded JavaScript-side queue. Preserve the core's independent terminal-status
path. On event-buffer overflow, fail pending operations and surface the stable
`EVENT_BUFFER_OVERFLOW` error through the iterator and terminal client status.
Do not promise a post-failure diagnostics snapshot: the core rejects new
diagnostics after it enters `Failed`.

For MQTT 5, preserve capability-aware admission. Before the first CONNACK and
while reconnecting, asynchronous admission of QoS 1/2, retained, or Topic Alias
publishes waits for negotiated capabilities; a nonblocking admission API reports
transient backpressure. Alias-free, non-retained QoS 0 remains admissible.

Do not implement an `EventEmitter` facade until its behavior for absent/slow
listeners, listener exceptions, and manual acknowledgements is specified. It
may be added later as a thin adapter over the single event stream.

### 2.3 Cleanup

Register environment cleanup so process shutdown, worker-thread termination,
module unloading where supported, and ordinary object finalization all stop
native activity safely.

- `close()` is idempotent and performs the graceful barrier with a finite
  default timeout.
- `closeNow()` is idempotent and requests immediate shutdown.
- A finalizer requests nonblocking immediate shutdown but does not synchronously
  join the driver on the JavaScript thread.
- Environment cleanup prevents any later thread-safe callback into an invalid
  Node-API environment and performs a bounded native join. Prefer Node-API's
  asynchronous cleanup hook when the selected Node-API level and `napi-rs`
  expose it; do not perform the join in an ordinary synchronous finalizer.
- Worker threads get independent client instances and cleanup state; no global
  mutable Node-API environment is shared between them.

Test process exit with live clients so the addon neither hangs indefinitely nor
uses a destroyed JavaScript environment.

## 3. Package native artifacts

Publish prebuilt binaries rather than requiring end users to install Rust,
Python, `node-gyp`, or a C++ compiler. Use a small platform loader and optional
platform packages, following established `napi-rs` packaging conventions.

Initial supported targets should be based on CI capacity, with at least:

- Linux x86_64 glibc;
- Linux x86_64 musl;
- Linux aarch64 glibc;
- macOS x86_64 and aarch64; and
- Windows x86_64 MSVC.

Add Windows aarch64 when CI can execute smoke tests rather than merely
cross-compile it.

The loader must report unsupported OS/architecture/libc combinations clearly.
Never silently fall back to a pure-JavaScript MQTT implementation with
different behavior. Generate checksums/provenance in the release workflow and
verify that every advertised platform package contains the expected `.node`
file.

## 4. Node.js, Deno, and Bun compatibility

### 4.1 Node.js baseline

Choose a maintained Node.js LTS as the minimum version and document it. Test
both CommonJS `require` and ESM `import`, worker threads, process shutdown, TLS,
and prebuilt artifact selection. Use only stable Node-API calls at the selected
API level.

### 4.2 Deno

Support local Deno through the npm package:

```ts,ignore
import { MqttClient } from "npm:@rumqtt/rumqttc";
```

Document and test Deno's requirements for a local `node_modules` directory and
the `--allow-ffi` permission. Ensure installation does not depend solely on an
npm lifecycle script that Deno skips by default; prebuilt optional packages
must be resolvable without compiling locally.

Do not claim support for Deno Deploy or other sandboxes that prohibit native
addons. A separate Deno FFI backend is allowed only after a recorded Node-API
compatibility or distribution failure justifies duplicating the boundary. If
needed, build that backend on the checked-in C API in `native-wrappers/c`,
following `docs/c-abi-compatibility.md`, not Deno runtime internals.

### 4.3 Bun

Load the same npm package and `.node` artifacts under Bun. Maintain a test that
exercises every Node-API feature the addon actually uses, because Bun's Node-API
implementation may be incomplete even when the addon loads successfully.

Do not depend on Bun native-plugin lifecycle hooks or internal Rust/Zig crates.
Pin the minimum tested Bun release only after the full MQTT smoke suite passes;
if that is Bun 1.4, state the exact minimum patch version. Add a runtime-specific
loader shim only for a demonstrated resolution issue, and keep the public
TypeScript API identical.

## 5. Verification matrix

Run the same behavioral suite under all three runtimes:

- v4 and v5 connect/CONNACK;
- binary publish payload with embedded zero bytes;
- QoS 0, 1, and 2 tracked completion semantics;
- subscribe, incoming publish, unsubscribe;
- automatic and manual acknowledgement;
- reconnect after broker interruption;
- TLS verification and rejected certificate;
- bounded request backpressure;
- bounded event-buffer overflow;
- graceful and immediate close;
- dropped JavaScript completion waiter;
- worker-thread creation and termination where supported;
- runtime exit with a live client; and
- repeated create/connect/close without native thread or handle leaks.

Run TypeScript API tests to ensure declarations match runtime exports. Add one
package-install smoke test per prebuilt target and execute, rather than only
compile, every host architecture available in CI.

Recommended commands should include the actual selected toolchain, for example:

```text
cargo fmt --manifest-path native-wrappers/Cargo.toml --all
cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-js-next
node --test native-wrappers/js/tests/node
deno test --allow-net --allow-ffi --node-modules-dir=auto native-wrappers/js/tests/deno
bun test native-wrappers/js/tests/bun
```

Invoke each runtime directly. Do not use `npm`, `pnpm`, or `yarn` as a substitute
for executing the Node.js, Deno, and Bun suites independently.

Use a local deterministic broker fixture; do not make release tests depend on
a public MQTT broker.

## Documentation and completion criteria

Document runtime prerequisites, native-addon security implications, admission
versus MQTT completion, event-consumption requirements, reconnect behavior,
manual acknowledgements, shutdown, supported platforms, and unsupported
sandbox/browser environments. Add the wrapper to `CHANGELOG.md` when it becomes
user-facing.

This TODO is complete when one published package and TypeScript API pass the
shared MQTT behavior suite on the supported Node.js, Deno, and Bun versions,
with prebuilt artifacts, bounded memory behavior, deterministic cleanup, and no
runtime-internal extension APIs.
