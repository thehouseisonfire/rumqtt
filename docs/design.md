## Library Design

This document describes the high-level design shared by the `rumqttc-v4` and
`rumqttc-v5` client crates. The crates publish separate Cargo packages for MQTT
3.1.1 and MQTT 5, but both expose the library target as `rumqttc` and share the
same core shape: an application-facing client handle queues work, while an
event loop owns network and protocol progress.

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

### Event Loop

`EventLoop` is the component that owns the live MQTT connection. Calling
`EventLoop::poll().await` advances the connection by connecting or reconnecting,
reading broker packets, writing queued application requests, updating MQTT
state, sending keep-alive pings, retransmitting pending packets when needed, and
yielding `Event` values to the application.

The event loop should be polled continuously. If polling stops, the connection
does not make progress: broker packets are not read, outgoing requests are not
written, keep-alive work stalls, acknowledgements are delayed, reconnects do not
happen, and bounded request channels can fill. For async applications, the usual
pattern is to drive the event loop in a dedicated task while cloned
`AsyncClient` handles are used elsewhere.

The event loop supports multiple transports through `MqttOptions` and
`Transport`:

- TCP by default.
- Unix sockets on Unix platforms.
- TLS over TCP when a TLS feature is enabled.
- WebSocket and secure WebSocket when the `websocket` feature is enabled.
- Optional proxying, custom socket connectors, and low-level `NetworkOptions`.

### Client Handles

`AsyncClient` is a lightweight, cloneable handle that sends application work to
the event loop. It does not perform network I/O itself. It validates and queues
requests such as publishes, subscribes, unsubscribes, manual acknowledgements,
and disconnects.

Built clients use separate request paths internally:

- A publish request channel for flow-controlled `PUBLISH` traffic.
- A control request channel for operations such as subscribe, unsubscribe,
  acknowledgement, and graceful disconnect.
- A dedicated immediate-disconnect channel for requests that may bypass queued
  application work.

The request channels are admission queues, not a strict global wire-order
guarantee. Under publish flow-control pressure, control packets can pass earlier
QoS 1 or QoS 2 publishes that are not currently sendable. Application publishes
preserve FIFO order relative to other application publishes.

Both crates also expose synchronous `Client` and `Connection` wrappers. The sync
client is built from the same async client/event-loop pair and runs the event
loop through an internal current-thread Tokio runtime owned by `Connection`.

### Requests, Events, and State

The event loop returns events that describe incoming broker packets and outgoing
client activity. In MQTT 5, the event stream can also include enhanced
authentication lifecycle events.

MQTT protocol state is kept in `MqttState`. This state tracks packet identifiers,
in-flight QoS handshakes, pending retransmission work, incoming acknowledgement
state, topic aliases in MQTT 5, and protocol errors. The event loop combines
this state with an outbound scheduler so it can respect flow-control limits and
still admit ready control work.

### Acknowledgements

Incoming publish acknowledgements are automatic by default. In automatic mode,
the event loop sends the MQTT-required response for incoming QoS 1 and QoS 2
publishes.

Applications that need to defer or customize acknowledgements can configure
`MqttOptions` with `AckMode::Manual`. In manual mode, the application must
acknowledge every incoming QoS 1 or QoS 2 publish by using `ack`,
`try_ack`, `prepare_ack` plus `manual_ack`, or `try_manual_ack`. The library
validates manual acknowledgements, but it cannot guarantee that the application
eventually sends them.

### Notices and Completion Tracking

The basic client methods report whether a request was accepted into the event
loop. Tracked variants such as `publish_tracked`, `subscribe_tracked`, and
`unsubscribe_tracked` return notices that complete when the corresponding MQTT
milestone is reached. For example, tracked QoS 1 publishes complete on `PUBACK`,
tracked QoS 2 publishes complete on `PUBCOMP`, tracked subscribes complete on
`SUBACK`, and tracked unsubscribes complete on `UNSUBACK`.

### Sessions and Reconnection

The event loop reconnects when polling continues after a recoverable network
failure. It keeps local session state for in-flight MQTT work and can persist
session checkpoints through a configured `SessionStore`. On reconnect, pending
work is replayed according to MQTT rules and the broker's reported session
state.

Persistent sessions are available in both crates. MQTT 5 additionally has
policy for broker-only session resumes, because a broker can report a retained
session when a newly constructed client has no matching local packet-id state.

### MQTT Version Differences

The shared architecture is intentionally similar between `rumqttc-v4` and
`rumqttc-v5`, but MQTT 5 has extra protocol surface:

- Publish, subscribe, unsubscribe, connect, disconnect, and acknowledgement
  properties.
- Topic alias policy and validation.
- Enhanced authentication and re-authentication support.
- MQTT 5-specific session and broker capability handling.

When changing behavior that is common to both protocol versions, update both
crates consistently. Version-specific behavior should stay in the corresponding
crate's protocol and state paths.
