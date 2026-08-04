# Browser JavaScript and WebAssembly Wrapper

## Goal

Ship a browser-focused JavaScript/TypeScript MQTT client compiled to WebAssembly
and backed by the v4 and v5 rumqtt clients. The wrapper must provide idiomatic
promises, asynchronous event consumption, bounded memory behavior, explicit
MQTT completion semantics, and browser-native MQTT-over-WebSocket transport.

This TODO deliberately does not choose how the clients are decoupled from
Tokio, how the runtime-neutral core is divided into crates, or whether the
portable client is expressed through traits, callbacks, polling interfaces, or
another design. Implement this wrapper against the architecture that has
actually landed.

## Scope and non-goals

The supported environment is an ordinary browser page or Dedicated Worker that
can load a WebAssembly module and open a browser `WebSocket`.

The initial wrapper supports:

- MQTT 3.1.1 and MQTT 5;
- `ws://` and `wss://` MQTT-over-WebSocket connections;
- QoS 0, 1, and 2 publish completion;
- subscribe, unsubscribe, incoming publish, and manual acknowledgements;
- reconnect and MQTT session behavior provided by the underlying clients;
- bounded command and event buffering;
- typed JavaScript errors and MQTT 5 properties;
- optional browser-backed persistent session storage; and
- direct browser and bundler-based npm consumption.

The following are out of scope unless browser capabilities materially change:

- raw MQTT over TCP or Unix sockets;
- arbitrary TCP socket connectors;
- native TLS provider selection, custom trust roots, or custom certificate
  verification;
- HTTP and SOCKS proxy selection by the MQTT library;
- arbitrary WebSocket upgrade headers;
- Node.js, Deno, and Bun native-addon support, which belongs in `TODO6.md`;
- Deno Deploy and other non-browser WASM hosts without the required web APIs;
- automatic operation in a Service Worker whose lifetime the browser may end;
- a pure-JavaScript MQTT fallback; and
- a stable raw WebAssembly ABI for consumers bypassing the JavaScript facade.

Do not claim general WASM support merely because the crate compiles for one
WASM target. The released contract is the tested browser environments and web
APIs described here.

## Readiness criteria

Begin implementation only when the landed clients can demonstrate all of the
following without target-specific patches in the wrapper repository:

- the selected browser WASM target compiles with the required client features;
- no enabled dependency unconditionally requires Tokio networking, `mio`, a
  native thread, filesystem access, or another unavailable host facility;
- the client can be continuously driven on a single-threaded browser executor;
- transport input/output can be connected to the browser WebSocket adapter;
- time, timers, and randomness have working browser implementations;
- cancellation of one poll/future does not corrupt connection or session state;
- packet payloads and queues can use `alloc` or an equivalent available
  allocator; and
- core v4/v5 protocol tests pass under the chosen WASM test environment where
  they do not require native sockets.

These are capability requirements, not a prescribed decoupling architecture.
If the landed API names or ownership model differ from examples in this TODO,
use the actual APIs while preserving the behavior and boundaries below.

## Proposed deliverables

Use names consistent with the final repository organization. A tentative
layout is:

```text
rumqtt-browser/
├── Cargo.toml
├── README.md
├── src/
│   ├── lib.rs
│   ├── client.rs
│   ├── completion.rs
│   ├── config.rs
│   ├── error.rs
│   ├── event.rs
│   ├── persistence.rs
│   ├── runtime.rs
│   └── websocket.rs
├── js/
│   ├── index.ts
│   ├── loader.ts
│   ├── types.ts
│   └── worker.ts
├── package.json
└── tests/
    ├── browser/
    ├── fixtures/
    └── types/
```

Publish a dedicated package such as `@rumqtt/browser`. Do not hide the browser
implementation behind runtime detection in the native Node-API package. A
small package containing TypeScript types shared with `TODO6.md` is acceptable
only when it avoids duplication without forcing browser users to install or
resolve native-addon packages.

## 1. Implement the browser transport

### 1.1 WebSocket establishment

Use the browser `WebSocket` API and request the MQTT subprotocol:

```js
new WebSocket(url, ["mqtt"])
```

Require an explicit `ws://` or `wss://` URL, including any broker-specific path
and query. Do not guess `/mqtt`, rewrite schemes, or silently downgrade WSS to
WS. After connection, verify that the browser reports the negotiated `mqtt`
subprotocol; fail clearly if the broker accepts the upgrade without the
required subprotocol.

Set `binaryType = "arraybuffer"` before processing messages. Treat a text frame
or an unexpected `Blob` after configuration as a transport error rather than
coercing it into MQTT bytes. Preserve WebSocket message order while feeding
received byte sequences to the MQTT decoder.

Map `open`, `message`, `error`, and `close` callbacks into the landed client
transport/runtime interface. Retain callback closures only while the transport
is live and unregister them during every close/failure path. A late callback
from an earlier socket generation must not mutate a reconnected client.

### 1.2 Browser-owned networking behavior

Document that the browser controls DNS, TCP, TLS, certificate validation,
system proxy use, cookies allowed by browser policy, and the `Origin` header.
Reject configuration fields that imply the library controls those facilities.

Surface useful close code and reason information without treating browser
WebSocket error events as if they contained native I/O error detail. Explain
common failures involving broker origin policy, missing MQTT subprotocol,
mixed-content restrictions, Content Security Policy `connect-src`, and an
untrusted WSS certificate.

Do not place credentials in a URL generated by the wrapper. MQTT username and
password belong in CONNECT fields. If a caller explicitly supplies URL userinfo,
reject it unless a future browser compatibility investigation establishes a
safe, portable contract.

### 1.3 Read and write backpressure

Browser WebSocket sends do not expose an awaitable flush operation. Implement a
bounded outbound adapter using `WebSocket.bufferedAmount` and browser timers:

- define configurable low and high watermarks with safe defaults;
- stop accepting additional transport writes above the high watermark;
- recheck after a bounded timer interval without busy-spinning;
- resume below the low watermark;
- fail the connection on a configured stall timeout; and
- wake the MQTT driver promptly on close or error.

Keep the MQTT request-channel limit separate from the browser WebSocket byte
watermark. A successful `send()` call means bytes were accepted by the browser,
not flushed to the network or acknowledged by the broker. Map the underlying
client's tracked QoS 0 completion only at the milestone the landed transport
contract can honestly support; do not strengthen it based solely on
`WebSocket.send()` returning.

Bound inbound buffering between JavaScript callbacks and the MQTT decoder.
Never accumulate arbitrary `ArrayBuffer` objects because the application has
stopped consuming events.

## 2. Drive the client in a browser executor

Run one connection driver per client using browser-compatible local futures and
wakeups. Do not block the JavaScript thread, emulate blocking with spin loops,
or assume WebAssembly threads/atomics are available.

The driver must:

- start only once for a client;
- continuously make MQTT progress while running;
- continue after recoverable connection errors so reconnection remains active;
- serialize state changes even when JavaScript calls methods reentrantly;
- avoid polling after terminal close;
- release WebSocket callbacks, timers, futures, and JavaScript references on
  shutdown; and
- turn an unexpected Rust panic into a terminal JavaScript `MqttError` rather
  than leaving promises pending forever.

Support construction inside a Dedicated Worker and on the window main thread.
Recommend a worker for high message rates, but do not require cross-origin
isolation, `SharedArrayBuffer`, or WASM threads in the baseline build.

Do not automatically create a Worker in the first release. Applications may
import and construct the client inside their own worker. A later worker-proxy
entry point must define transferable payload ownership, event backpressure,
termination, and error propagation before it is added.

## 3. Define the JavaScript and TypeScript API

Keep names and result semantics aligned with the native wrapper in `TODO6.md`
where browser capabilities allow. The initial surface should be equivalent to:

```ts
export type ProtocolVersion = "3.1.1" | "5.0";
export type QoS = 0 | 1 | 2;

export class MqttClient {
  static connect(options: BrowserMqttClientOptions): Promise<MqttClient>;

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

`connect()` resolves only after an accepted CONNACK. Coalesce concurrent
connection attempts for the same object if the implementation exposes a public
constructor; reject calls after closing begins. Initial failure and later
recoverable disconnection must have unambiguous promise/event ordering.

`enqueuePublish` resolves at bounded request admission. `publish`, `subscribe`,
and `unsubscribe` use tracked completion notices. Preserve the underlying
client's exact QoS milestones and broker reason codes. Promise cancellation or
garbage collection drops only the JavaScript waiter and must not claim that
already admitted MQTT work was cancelled.

### 3.1 JavaScript values

Accept strings for MQTT UTF-8 fields and `Uint8Array` for arbitrary bytes.
Optionally accept string publish payloads by encoding them as UTF-8 explicitly.
Do not use Node.js `Buffer` in the browser declarations.

Copy a mutable `Uint8Array` before retaining it across an asynchronous boundary,
unless a documented consuming API transfers ownership. Returned payloads must
remain valid after the next event is polled. Avoid repeated conversions through
JSON, base64, or per-byte JavaScript calls.

Represent packet identifiers and bounded MQTT integers as JavaScript numbers
only when their full range is exactly representable. Use `bigint` for counters
that may exceed the safe integer range.

### 3.2 Events and acknowledgement

Use the discriminated `MqttEvent` union defined for the native wrapper, with
browser-relevant transport details. Make `events()` single-consumer initially
and reject a second active iterator.

In manual-ack mode, attach a one-shot acknowledgement operation backed by an
opaque token. Reject token reuse, use with another client, and use after a
session boundary that invalidates it. Do not expose an arbitrary packet-ID
acknowledgement API.

Do not silently drop incoming publishes. Use a configurable bounded event
buffer and an independent terminal-status path. If the consumer fails to drain
the buffer within the configured delivery timeout, close the connection,
reject pending wrapper operations appropriately, and surface
`EVENT_BUFFER_OVERFLOW`. Revisit this behavior if the landed core can continue
protocol-critical progress independently of application notification delivery.

### 3.3 Errors

Export the same stable `MqttError` shape and error categories as `TODO6.md`
where applicable. Add browser-specific stable codes for at least:

- WebSocket construction failure;
- MQTT subprotocol not negotiated;
- unexpected WebSocket data type;
- WebSocket close before CONNACK;
- browser outbound-buffer stall;
- event-buffer overflow;
- WASM initialization failure;
- unsupported browser configuration; and
- persistence unavailable or denied.

Keep browser-provided messages and close reasons diagnostic. Do not infer a
retryable authentication, TLS, or network category from an opaque WebSocket
error when the browser does not reveal that information.

## 4. Package and initialize WebAssembly

Use the established Rust-to-browser binding toolchain selected at implementation
time. Generate an ES module, `.wasm` artifact, and TypeScript declarations, but
treat the hand-reviewed TypeScript facade as the stable API rather than exposing
all generated Rust bindings.

Support:

- modern bundlers through normal npm ESM imports;
- direct browser ESM loading through a documented initialization entry point;
- asynchronous streaming instantiation when the server supplies the correct
  WASM MIME type, with a clear fallback or error contract; and
- construction inside a Dedicated Worker.

Do not start network activity at module import time. Module initialization may
be cached, but clients must not share mutable protocol state. Report CSP and
MIME-type initialization failures with actionable messages.

Pin the binding CLI and crate versions together in CI. Verify that the npm
tarball contains the exact `.wasm`, JavaScript, declarations, license, and
README files referenced by package exports. Add release checksums and
provenance.

Track release-build size and initialization time. Strip debug/name sections
from production artifacts unless deliberately shipped separately, enable
appropriate size optimization, and record material regressions. Do not trade
away protocol validation or bounded-memory behavior solely to reduce WASM size.

## 5. Browser persistence

Make browser persistence optional and capability-detected. If the landed client
session-store interface can be implemented correctly with IndexedDB, provide a
store that:

- namespaces records by origin, broker identity, protocol version, client ID,
  and an explicit application scope;
- preserves the checkpoint versioning and validation used by the clients;
- commits one logical checkpoint atomically in a transaction;
- detects corrupt or incompatible data and reports a structured restore error;
- supports explicit clear and storage-scope deletion;
- handles quota, denied access, private-browsing restrictions, and database
  closure without panicking; and
- never stores URL credentials or unrelated JavaScript configuration.

Do not advertise durable recovery until real-browser crash/reload tests prove
the ordering contract. IndexedDB completion and network transmission cannot be
one atomic operation; retain the clients' documented conservative duplicate
and ambiguous-delivery semantics.

If the landed persistence interface cannot be driven safely in a browser, ship
the first wrapper without persistence and record the precise missing capability
instead of inventing a second session format in JavaScript.

## 6. Browser lifecycle and security

`close()` performs a finite graceful MQTT shutdown; `closeNow()` performs
immediate teardown without a delivery claim. Both are idempotent. Object
finalization may request best-effort local cleanup but must not be required for
correctness or timely broker notification.

Do not promise graceful DISCONNECT from `beforeunload`, page termination,
worker termination, browser crash, or mobile suspension. Do not disconnect
merely because a page becomes hidden. Document that browser timer throttling
and suspension can delay keepalive and cause broker reconnects.

Listen to `online`/`offline` only as hints if doing so improves retry timing.
Those events must not override MQTT session reconciliation or claim that a
connection is healthy. Remove every global lifecycle listener on client close.

Document:

- the broker must expose a browser-reachable MQTT-over-WebSocket endpoint;
- WSS is required when page mixed-content policy disallows WS;
- the broker may need to allow the page's `Origin` and the `mqtt` subprotocol;
- CSP must permit the broker in `connect-src` and permit the chosen WASM loading
  method;
- native addons and WASM execute trusted package code outside any MQTT-level
  security boundary; and
- credentials and decrypted payloads exist in page/WASM memory and are subject
  to the application's XSS threat model.

## 7. Feature and compatibility reporting

Publish a maintained feature matrix comparing browser and native wrappers.
Mark unsupported configuration at construction time instead of silently
ignoring it. At minimum, distinguish:

| Capability | Browser wrapper |
| --- | --- |
| MQTT 3.1.1 / MQTT 5 | Supported |
| WS / WSS | Supported |
| Raw TCP / Unix socket | Unsupported |
| Browser-managed TLS validation | Supported through WSS |
| Custom CA / TLS provider | Unsupported |
| HTTP / SOCKS proxy configuration | Unsupported |
| Custom WebSocket headers | Unsupported |
| Automatic and manual ACK | Supported |
| Session persistence | Optional, after IndexedDB verification |
| WASM threads | Not required |

Choose minimum Chrome, Firefox, Safari/WebKit, and Edge versions from executed
CI evidence, not from syntax assumptions. Avoid enabling WASM proposals not
available across that baseline. Feature-detect optional browser APIs and report
a structured error when a required API is absent.

## 8. Verification

Use real headless browser engines and a deterministic local MQTT broker with
WebSocket support. Do not substitute Node.js WASM tests for browser tests.

Run the same behavioral suite in Chromium, Firefox, and WebKit for:

- module initialization through npm/bundler and direct ESM loading;
- v4 and v5 connect with verified `mqtt` subprotocol;
- binary payloads containing zero bytes;
- multiple MQTT packets in one WebSocket message, back-to-back WebSocket
  messages, and a large message fragmented into WebSocket frames by the test
  server;
- QoS 0, 1, and 2 tracked completion;
- subscribe, incoming publish, unsubscribe;
- automatic and manual acknowledgement;
- MQTT 5 properties and negative reason codes;
- reconnect after broker close and browser offline/online transitions;
- browser outbound high-watermark and stall behavior;
- bounded command and event overload;
- graceful close, immediate close, and close during connection establishment;
- dropped JavaScript completion waiters;
- repeated create/connect/close with no retained timers, callbacks, or sockets;
- construction and operation inside a Dedicated Worker;
- invalid URL, missing subprotocol, text frame, and opaque WebSocket failure;
- WSS with a CI-trusted test certificate;
- IndexedDB save, reload/restore, clear, corruption, and quota failure when the
  persistence feature is enabled; and
- TypeScript declarations matching runtime exports.

Add targeted Rust/WASM tests for callback-generation isolation, cancellation,
timer cleanup, byte conversion, error mapping, and parser/state-machine parity.
Test a production-built npm tarball rather than only a workspace import.

The release workflow should run formatting, native core tests affected by the
portable changes, WASM compilation, browser unit tests, the three-engine
integration matrix, TypeScript checks, package-content validation, and artifact
size reporting. Use the exact commands selected by the eventual toolchain and
record them in the wrapper README and CI configuration.

## Documentation and completion criteria

Document installation, asynchronous module initialization, broker WebSocket
configuration, the TypeScript API, admission versus MQTT completion, manual
acknowledgement, reconnect behavior, event-consumption requirements, browser
limitations, worker usage, persistence guarantees, security, and shutdown.
Add the wrapper to `CHANGELOG.md` when it becomes user-facing.

This TODO is complete when a published browser package passes the full v4/v5
behavioral suite in every advertised browser engine, operates on the main
thread and in a Dedicated Worker without blocking, uses bounded memory, cleans
up all browser resources deterministically, exposes no unsupported native
configuration, and requires no commitment to a decoupling architecture beyond
the capabilities listed in this document.
