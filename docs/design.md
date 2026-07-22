# Library Design

This document describes the architecture shared by the MQTT 3.1.1 and MQTT 5
client crates. The source directories are `rumqttc-v4/` and `rumqttc-v5/`; the
published Cargo packages are `rumqttc-v4-next` and `rumqttc-v5-next`. Both
packages expose their library target as `rumqttc`.

Version-specific protocol semantics are documented separately:

- [MQTT 3.1.1 client design](../rumqttc-v4/design.md)
- [MQTT 5 client design](../rumqttc-v5/design.md)

```text
      ┌────────┐
      │ Broker │
      └───┬▲───┘
          ││
          ││ MQTT packets over TCP, Unix sockets, TLS, WebSocket, or WSS
          ││
    ┌─────▼┴─────┐
    │ EventLoop  │
    │            │
    │ - network  │
    │ - state    │
    │ - session  │
    │ - timers   │
    └──┬──────▲──┘
       │      │
       │      │ request channels
       │      │
       │  ┌───┴─────────┐
       │  │ AsyncClient │
       │  └────▲────────┘
       │       │
       │       │ publish, subscribe, unsubscribe, ack, disconnect
       ▼       │
    ┌──────────┴───────┐
    │ User Application │
    └──────────────────┘
```

## Ownership and Execution

An asynchronous client is a pair:

- `AsyncClient` is a lightweight, cloneable request producer. It validates an
  operation and submits a `Request` to an internal channel.
- `EventLoop` owns the network connection, timers, outbound scheduler, replay
  queue, and `MqttState`.

The client handle does not perform MQTT I/O. A successful ordinary client
method means that a request entered an admission channel; it does not mean that
the packet was written, flushed, received by the broker, or acknowledged.

The asynchronous core neither creates a worker thread nor owns an executor.
Applications must continuously call `EventLoop::poll`, drive
`EventLoop::into_stream` when the `stream` feature is enabled, or run either in
an application-owned task. The synchronous `Connection` wrapper owns a
current-thread Tokio runtime, but it advances only while the application
iterates or polls it.

> **Progress contract:** if the event loop is not driven, reads, writes,
> acknowledgements, keepalive, replay, and reconnection all stop. Bounded
> request channels may consequently fill and block producers.

The event loop supports TCP and, on Unix, Unix sockets. Feature-dependent
transports include TLS, WebSocket, secure WebSocket, and proxies. Custom socket
connectors and `NetworkOptions` provide lower-level connection control.

## Request Admission and Backpressure

Constructed clients use separate paths for flow-controlled publishes, control
requests, and immediate disconnect. Channels are bounded by default; unbounded
channels require an explicit builder choice. Bounded admission exposes overload
to producers instead of allowing memory use to grow without limit.

The channels are not a single global wire-order queue. Application publishes
remain FIFO relative to other application publishes, but a ready control packet
can pass a QoS 1 or QoS 2 publish blocked by the in-flight window. MQTT progress
traffic must not wait behind application traffic that cannot currently be sent.

The event loop batches ready requests and network reads. Batch limits trade
throughput against latency and fairness. A request that has merely entered a
channel is not network activity.

## Protocol State and Events

`MqttState` is the authority for packet identifiers and MQTT handshakes. It
tracks outgoing QoS flows, PUBREL state, pending SUBSCRIBE and UNSUBSCRIBE
exchanges, incoming acknowledgement state, manual-ack state, and events exposed
to the application. State transitions produce packets for the event loop to
write; they do not perform network I/O themselves.

Packet-identifier-indexed vectors and bit sets make acknowledgement lookup
independent of the number and order of outstanding packets. Packet identifiers
are reserved across publish and control flows so that an identifier is not
reused while its earlier exchange is incomplete.

The event loop returns `Event` values for incoming broker packets and outgoing
client activity. MQTT 5 can additionally report enhanced-authentication
lifecycle events.

Incoming publish acknowledgements are automatic by default. With
`AckMode::Manual`, the application must acknowledge every incoming QoS 1 and
QoS 2 publish. The library validates submitted manual acknowledgements, but it
cannot ensure that the application eventually submits one.

> **MQTT warning:** an application using manual acknowledgements must complete
> every required PUBACK or PUBREC. Otherwise, the exchange remains incomplete
> and can exhaust broker or client flow-control resources.

## Submission and Completion

Queue admission and MQTT completion are deliberately different concepts.
Tracked operations provide operation-specific completion notices:

- A QoS 0 publish completes after the packet is flushed to the local network
  transport. MQTT provides no broker acknowledgement, so delivery is not
  proven.
- A QoS 1 publish completes when its PUBACK is received.
- A QoS 2 publish completes when PUBCOMP finishes the four-packet exchange.
- Tracked subscriptions and unsubscriptions complete on the matching SUBACK
  and UNSUBACK.

Incoming acknowledgement packets remain visible as events even when they also
resolve a notice.

A timeout or transport failure cannot prove that a packet partially or fully
written to the network was not delivered.

### Persistent recovery and the DUP flag

When a `SessionStore` is configured, admission of an outgoing QoS 1 or QoS 2
PUBLISH and its packet-identifier ownership is durably checkpointed before the
packet is visible to the transport. The uninterrupted first wire packet is a
separate value and keeps `DUP=0`; the checkpoint is a conservative recovery
instruction with `DUP=1`, and restoration sends that recovery PUBLISH with
`DUP=1`.

The store commit and network transmission cannot be one atomic operation.
Persisting "attempted" before transport can leave durable `attempted=true`
although no packet was sent. Persisting it after transport can leave durable
`attempted=false` although some packet bytes became visible. The conservative
representation avoids either unverifiable attempted-state choice and preserves
admitted work and packet-identifier ownership.

Consequently, a process can terminate after the admission checkpoint commits
but before any transport attempt. After restoration, the broker's first
physically observed copy can therefore carry `DUP=1`. Strict conformance with
the MQTT first-transmission DUP requirement is not claimed for that exact
recovery case. This qualification is limited to persistent recovery: ordinary
uninterrupted operation still sends the first PUBLISH with `DUP=0`, and clients
without a configured `SessionStore` do not enter this path.

This policy belongs to the v4 and v5 client state machines and applies to every
`SessionStore` backend. A file-store adapter only persists the state supplied
by its client; its envelope, CRC, atomic replacement, and synchronization do
not determine the DUP flag.

## Keepalive

MQTT Keep Alive limits the interval between MQTT Control Packets transmitted by
the client. The event loop starts a timer after connection and resets it after
an actual request flush or an automatic protocol response is written. When the
timer expires, it sends PINGREQ. If another keepalive attempt occurs while the
previous PINGRESP is still outstanding, the connection is considered
unhealthy.

Incoming traffic that produces no client response does not reset the client's
transmission deadline. Accepting a request into a channel also does not count as
sending a Control Packet.

> **MQTT warning:** incoming publications alone do not satisfy the client's
> keepalive obligation. Resetting the deadline on every read can violate MQTT
> even while the connection appears busy.

## Reconnection and Sessions

`EventLoop::poll` establishes a connection when none exists. A failed connection
attempt returns its error without accepting a network. After a non-terminal
established connection failure, the event loop drops the network, moves
replayable work into the pending queue, persists eligible session state, and
returns the error. If the caller continues polling, the next poll can attempt
another connection.

This is a mechanism, not a recommendation to retry every error. The caller
decides whether and when another poll is appropriate. The event loop currently
does not impose backoff, jitter, attempt limits, or error-class policy.

Reconnection requires MQTT session reconciliation, not only a new socket.
Packet-identifier ownership and incomplete QoS handshakes can be replayed only
when the broker and client agree that the earlier session survives. The exact
Clean Session, Clean Start, Session Expiry Interval, and Session Present rules
are version-specific.

The optional `SessionStore` persists protocol recovery state already admitted
into the state machine: in-flight QoS flows, packet-identifier ownership and
progress, SUBSCRIBE and UNSUBSCRIBE exchanges, and incoming QoS 2 state.
Canonical checkpoint encodings are versioned, restore validates checkpoint
contents against client configuration, and storage implementations must commit
or clear whole checkpoints crash consistently.

Exactly one active event loop may own a session-store key. `SessionStore` does
not provide leases, fencing, compare-and-swap, or active/passive coordination.

Accepted but not-yet-admitted requests can survive an ordinary reconnect while
the same event loop remains alive. They are not persisted in `SessionStore` and
can be lost when the process exits, crashes, or drops the event loop.
Applications that need every submitted request to survive restart must maintain
their own durable outbound queue.

## Disconnect and Shutdown

`disconnect` is a terminal graceful barrier carried by the control-request lane.
Once the event loop processes it, new application work is no longer admitted to
protocol processing; QoS 0 work already admitted to protocol processing is
flushed, and outstanding QoS 1, QoS 2, tracked SUBSCRIBE, and tracked UNSUBSCRIBE
exchanges are drained before DISCONNECT is written and flushed. Queued but
unsent flow-controlled publishes are not part of that drain. Because cloned
clients do not share an admission gate, a concurrent send can still be accepted
by its channel after the barrier and then discarded without protocol processing.

`disconnect_with_timeout` applies the same behavior with a deadline. If the
deadline expires, the event loop returns `DisconnectTimeout` and does not send
DISCONNECT. `disconnect_now` uses a dedicated priority path that may bypass
queued work and does not wait for unresolved handshakes. It is observed at
event-loop scheduling points and does not interrupt connection setup, work
already executing, buffered events, or an application that is not polling.

MQTT defines no server acknowledgement for a client DISCONNECT. Graceful
shutdown waits for earlier work, not for a response to DISCONNECT itself.
Dropping all client senders is also not an MQTT graceful shutdown; it
eventually ends polling with `RequestsDone`.

## Design Boundaries

Provider-specific credential creation, token refresh, cloud SDKs, and
deployment-specific signing remain outside the MQTT core. Protocol-level
authentication mechanisms can remain version-specific without coupling the
core to a cloud provider.

`SessionStore` is protocol recovery storage, not a durable application outbox.
Application outboxes and storage backends belong in application code or
companion crates under the current architecture.

A full network pause is not a safe connected state. Any pause facility must
stop only application traffic while allowing keepalives, acknowledgements,
authentication, and QoS handshakes to continue.

## Version-Specific Design

The common architecture is intentionally similar, but protocol behavior must
remain in the corresponding crate:

- [MQTT 3.1.1](../rumqttc-v4/design.md) defines Clean Session semantics and a
  property-free protocol surface.
- [MQTT 5](../rumqttc-v5/design.md) defines negotiated properties, reason
  codes, topic aliases, enhanced authentication, and explicit session expiry.

Changes to shared behavior should update both crates consistently. Protocol
differences should not be hidden behind a misleading common abstraction.
