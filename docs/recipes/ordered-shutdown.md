# Ordered publish shutdown

Choose the boundary that your application needs:

| Method | Work completed before DISCONNECT | Method return |
| --- | --- | --- |
| `disconnect()` | Work already admitted into MQTT protocol state; may overtake queued publishes | Channel admission |
| `disconnect_after_queued()` | All preceding queued/replayed publishes through their QoS milestones | A separately awaited `DisconnectNotice` |
| `disconnect_now()` | No publish-handshake drain | Channel admission |

All three retain their distinct policies. Ordered shutdown also has
`disconnect_after_queued_with_timeout`, `try_disconnect_after_queued`, and
`try_disconnect_after_queued_with_timeout` forms on async and blocking clients.
MQTT 5 additionally offers `_with_properties` and `_with_properties_timeout`
forms (including `try_*`) preserving the caller's reason code and properties.

## Burst followed by shutdown

Drive the event loop independently while submitting the burst and waiting for
completion. Runnable, compile-checked versions are
[`ordered_shutdown`](../../rumqttc-v4/examples/ordered_shutdown.rs) and
[`ordered_shutdown_v5`](../../rumqttc-v5/examples/ordered_shutdown_v5.rs).

```rust
use rumqttc::{AsyncClient, PublishOptions, QoS};
use std::time::Duration;

async fn finish_burst(client: &AsyncClient) -> Result<(), Box<dyn std::error::Error>> {
    for index in 0..32u8 {
        client.publish("shutdown/burst", vec![index], PublishOptions::new(QoS::AtLeastOnce)).await?;
    }
    let completion = client.disconnect_after_queued_with_timeout(Duration::from_secs(5)).await?;
    completion.wait_async().await?;
    Ok(())
}
```

Method success means the fence entered the request stream. Notice success means
every preceding publish completed and DISCONNECT was flushed successfully, with
required terminal persistence completed. An outgoing DISCONNECT event represents
the packet's outgoing state transition; it is not a substitute for the notice.

QoS 0 completion proves only transport flush. QoS 1 completes on successful
PUBACK; QoS 2 completes through successful PUBCOMP, following the tracked-publish
recovery rules. MQTT 5 negative acknowledgements fail the collective notice even
for untracked publishes. `publish_tracked` remains preferable for individual
results, especially when diagnosing which publish a broker rejected.

Subscriptions, unsubscriptions, independent inbound acknowledgements, and MQTT 5
authentication are outside the completion contract. Wait for their tracked
notices separately when their completion matters.

## Concurrency and cancellation

Clones share one successful-admission order across async, blocking, and `try_*`
calls. Task spawn order and wall-clock intent do not establish ordering. Only a
publish actually accepted before the fence belongs before it; a producer waiting
for capacity has not yet been admitted. Producers must handle
`ClientError::Closing` once the fence commits. No next-session queue is provided.

Cancelling admission while it waits for capacity leaves the client open. A full
`try_*` call returns `RequestChannelFull` with the recoverable public request and
installs no latent fence. After admission, dropping the notice does not cancel
shutdown. Concurrent fence calls are first-successful-admission wins; later calls
return `Closing`. Immediate disconnect can supersede the fence explicitly.

## Deadlines, reconnect, and persistence

Use a timeout for application shutdown: the no-timeout form can wait forever for
a broker acknowledgement. The absolute monotonic deadline starts when the fence
is admitted and covers queue delays, flow control, network work, reconnect,
persistence, and DISCONNECT flush. Waiting for channel capacity is outside this
deadline; apply an application timeout to the admission call if needed. Zero
expires immediately after admission; duration overflow is rejected.

Expiry abandons transport and resolves the notice as timed out. It does not start
DISCONNECT unless the publish precondition was already met and the terminal write
had begun. No later packet is encoded, and a failed or timed-out flush is never
reported as success. Delivery may be ambiguous: a peer may have received bytes
whose flush or acknowledgement the client could not observe.

Continue polling after a timeout until `RequestsDone` if session persistence is
configured. The timeout notice is resolved at expiry; an unfinished terminal
checkpoint/clear is retained for subsequent polls, without extending the shutdown
deadline or turning it into success. A ready cleanup completes immediately.
Dropping the event loop cancels unfinished storage work under the existing
crash-consistent, potentially indeterminate-commit `SessionStore` contract.
Nonzero MQTT 5 expiry preserves replay state; zero expiry clears it.

Recoverable transport loss can reconnect using a persistent session while the
same event loop and deadline remain alive. Earlier replayable work stays before
the fence. Clean-session loss, reset, redirect, unavailable replay, protocol
failure, or ambiguous unflushed QoS 0 work fail the shutdown instead of silently
downgrading its guarantee. Fences and their completion senders are never persisted:
session checkpoints are MQTT recovery state, not a durable application outbox.

Normal DISCONNECT can suppress a Will only when the server receives and processes
it. Local flush does not prove server receipt or broker consumption. MQTT 5's
explicit Disconnect-with-Will reason retains its protocol meaning.

`from_senders` clients return `TrackingUnavailable` from the ordered notice APIs.
External receivers may use the public ordered request variants, but receipt of a
variant alone proves neither MQTT execution nor completion.

Diagnostics expose the phase, fence sequence, absolute deadline, and local queued
publish count. The local count excludes channels and in-flight state; it is not a
remaining-delivery count. Shutdown tracing excludes payloads and credentials.

## Runtime costs

Ordered shutdown uses the same managed admission path for every builder-created
client; it is not a construction-time opt-in. Admission still serializes queue
insertion across lanes, and preceding publishes retain completion observations
even before shutdown is requested. This is necessary to retain earlier failures.
Untracked observations do not allocate individual completion channels.

Ordinary queue progress wakes only its lane, not the lifecycle/deadline watcher.
Pre-fence lifecycle reads use an atomic indicator, and successful collective
completion accounting uses an atomic counter rather than a mutex. A pending poll
still listens for fence admission so a newly admitted deadline can interrupt
stalled work. Paired release measurements found no consistent regression when
the feature was disabled, although one confidence interval exceeded a strict 3%
cost bound. Enabling the feature reduced synthetic publish-admission throughput
by 31--68% and broker-backed MQTT v4 QoS 1 throughput by about 10% in the measured
workloads. Treat these figures as workload- and machine-specific, and benchmark
representative traffic before enabling ordered shutdown in a throughput-sensitive
deployment.
