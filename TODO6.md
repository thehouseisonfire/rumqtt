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

## Prerequisite

Implement the useful portions of `TODO5.md` first, especially the owned event
model, tracked completions, dedicated driver, bounded event delivery, and
deterministic shutdown. Do not block this wrapper on speculative shared
abstractions that it does not consume.

## Proposed layout

```text
rumqtt-js/
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

Support the first-release common options defined in `TODO5.md`, plus typed MQTT
5 option/property objects where implemented. Make the protocol portion a
discriminated union so TypeScript rejects incompatible session options before
runtime where possible. Native validation remains authoritative for plain
JavaScript callers and values crossing the Node-API boundary. Validate:

- finite integers and numeric ranges before converting to Rust integers;
- protocol-specific options instead of silently ignoring them;
- topic and topic-filter validity through the client library;
- mutually exclusive TLS credential sources; and
- explicit opt-in for an unbounded request channel, if exposed at all.

Copy mutable JavaScript inputs at call admission. Do not retain a borrowed view
of an `ArrayBuffer` across an await or native-thread handoff.

### 1.2 Events and manual acknowledgements

Expose a discriminated union:

```ts
export type MqttEvent =
  | { type: "connected"; protocol: ProtocolVersion; sessionPresent: boolean }
  | { type: "disconnected"; error: MqttError; reconnecting: boolean }
  | { type: "publish"; message: IncomingMessage }
  | { type: "outgoing"; packet: OutgoingSummary }
  | { type: "closed" }
  | { type: "driverError"; error: MqttError };
```

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
  readonly retryable?: boolean;
  readonly ambiguous?: boolean;
}
```

Keep `message` diagnostic and non-stable. Map behavior using `code` and `kind`,
not formatted Rust error text. Preserve broker reason codes in typed completion
errors. Convert Rust panics caught inside native task boundaries into an
`INTERNAL_PANIC` driver error and shut the client down; never unwind through
Node-API.

## 2. Implement Node-API integration safely

### 2.1 Runtime and promise delivery

Start the shared dedicated driver when `connect()` is first called. Never run
`EventLoop::poll`, blocking channel receive, thread join, DNS, or TLS work on
the JavaScript event-loop thread.

Make `connect()` idempotent while connected and coalesce concurrent initial
calls onto one pending connection result. Reject calls made after closing has
started. Define initial connection failure separately from later recoverable
disconnect events so callers do not receive contradictory promise and stream
state.

Use Node-API async work, deferred promises, or thread-safe functions only in
their documented modes. All creation and resolution of JavaScript values must
occur on an allowed JavaScript thread with a valid environment. The Rust driver
may pass only owned Rust data across threads.

Keep a registry from shared `OperationId` to native promise completion state.
Remove entries exactly once on completion, environment shutdown, or terminal
driver failure. JavaScript garbage collection of a promise must not cancel the
corresponding MQTT operation.

### 2.2 Event delivery and overload

Bridge the shared bounded event receiver into the async iterator without an
unbounded JavaScript-side queue. Apply the `TODO5.md` terminal overflow policy
and surface `EVENT_BUFFER_OVERFLOW` through both the iterator and client status.

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
  Node-API environment and performs a bounded native join.
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
needed, build that backend on the stable C API from `TODO7.md`, not Deno runtime
internals.

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
cargo test -p rumqtt-js
deno test --allow-net --allow-ffi --node-modules-dir=auto
bun test
```

Do not run the suite with `npm`, `pnpm`, or `yarn`; use Bun and Deno only.

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
