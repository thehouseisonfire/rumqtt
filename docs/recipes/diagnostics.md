# Runtime Diagnostics Recipes

`EventLoop::diagnostics()` returns an observation-only snapshot of connection,
queue, session, batching, and outbound protocol state. It requires no Cargo
feature and performs no network I/O.

## Sample Between Polls

The task that owns and drives the event loop can sample it after each poll:

```rust,no_run
use rumqttc::{AsyncClient, ConnectionError, MqttOptions};

# async fn run() {
let options = MqttOptions::new("diagnostics-client", ("localhost", 1883));
let (_client, mut eventloop) = AsyncClient::builder(options).capacity(100).build();

loop {
    let poll_result = eventloop.poll().await;
    let snapshot = eventloop.diagnostics();

    println!(
        "connected={}, queued={}, channel={}, inflight={}/{}",
        snapshot.connected,
        snapshot.queues.pending_len,
        snapshot.queues.requests_rx_len,
        snapshot.outbound.inflight,
        snapshot.outbound.max_inflight,
    );

    match poll_result {
        Ok(_event) => {}
        Err(ConnectionError::RequestsDone) => break,
        Err(error) => eprintln!("connection error: {error}"),
    }
}
# }
```

Runnable MQTT 3.1.1 and MQTT 5 versions are available as
[`diagnostics_snapshot.rs`](../../rumqttc-v4/examples/diagnostics_snapshot.rs)
and
[`diagnostics_snapshot_v5.rs`](../../rumqttc-v5/examples/diagnostics_snapshot_v5.rs).

## Interpret Queue Depths Correctly

`queues.pending_len` is already the sum of `pending_replay_len` and
`queued_len`; do not add those three fields together. Request-channel lengths
can change while the snapshot is being assembled because cloned clients may
enqueue concurrently. Treat every snapshot as an approximate current
observation, not a transactional view or an event history.

Useful operational signals include:

- `connected`, `disconnecting`, and `disconnect_complete` for local lifecycle
  state. `connected` does not probe the broker or guarantee a live transport.
- `queues.*` for retained replay work, admitted work, and client-channel depth.
- `outbound.inflight`, `max_inflight`, and `publish_window_full` for MQTT flow
  control pressure.
- `session.*` for persistent-session state. MQTT 5 additionally reports
  `broker_only_session_resume`.
- `config.*` for configured and currently effective batching limits.

Constructing the nested outbound snapshot scans protocol tracking structures,
so sample at an interval appropriate for observability rather than on every
high-volume application operation. Export selected fields as gauges and keep
counters or history in the application's metrics system.
