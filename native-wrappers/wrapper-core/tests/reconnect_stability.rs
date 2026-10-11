mod support;

use std::io::Write;
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rumqttc_wrapper_core::*;
use support::*;

fn assert_capture_published_before_delivery(client: &NativeClient, previous: u64) {
    let snapshot = client.handle().diagnostics_snapshot();
    let native = snapshot.native.unwrap();
    assert!(native.generation > previous);
    for _ in 0..100 {
        let current = client.handle().diagnostics_snapshot();
        assert!(std::sync::Arc::ptr_eq(
            &native,
            current.native.as_ref().unwrap()
        ));
    }
}

#[test]
fn broker_disconnect_ends_stability_before_backpressured_event_delivery() {
    for buffered_publish in [false, true] {
        for observe_while_blocked in [false, true] {
            disconnect_with_backpressure(buffered_publish, observe_while_blocked);
        }
    }
}

fn disconnect_with_backpressure(buffered_publish: bool, observe_while_blocked: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let broker_listener = listener.try_clone().unwrap();
    let (disconnect_tx, disconnect_rx) = mpsc::channel();
    let broker = Broker::spawn(move || {
        let mut first = accept(&broker_listener);
        assert_eq!(frame(&mut first)[0], 0x10);
        first.write_all(&[0x20, 3, 0, 0x88, 0]).unwrap();
        drop(first);
        let mut second = accept(&broker_listener);
        connect(&mut second, true);
        disconnect_rx.recv_timeout(DEADLINE).unwrap();
        // A single network batch can leave PUBLISH queued before DISCONNECT.
        // Both packets must retain their order while stability ends immediately.
        second
            .write_all(if buffered_publish {
                b"\x30\x05\x00\x01a\x00x\xe0\x02\x00\x00"
            } else {
                b"\xe0\x02\x00\x00"
            })
            .unwrap();
    });
    let stability = Duration::from_millis(500);
    let mut config = config(true, listener.local_addr().unwrap().port());
    config.common.event_buffer_capacity = 1;
    config.common.event_delivery_timeout = DEADLINE;
    config.common.connection_timeout = Duration::from_secs(1);
    config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
        initial_delay: Duration::ZERO,
        maximum_delay: Duration::ZERO,
        jitter: ReconnectJitter::None,
        budget: RetryBudget::Limited(1),
        stability_interval: stability,
        ..ReconnectConfig::default()
    });
    let mut client = start(config).unwrap();
    let mut events = client.take_events().unwrap();
    until(&mut events, |event| {
        matches!(event, WrapperEvent::Disconnected { .. })
    });
    let deadline = Instant::now() + DEADLINE;
    loop {
        let snapshot = client.handle().reconnect_diagnostics();
        if snapshot.phase == ReconnectPhase::Connected {
            assert_eq!(snapshot.cycles_started, 2);
            assert_eq!(snapshot.retries_since_reset, 1);
            assert_eq!(snapshot.reset_count, 0);
            assert!(snapshot.remaining_stability.is_some());
            break;
        }
        assert!(Instant::now() < deadline, "second connection timed out");
        thread::sleep(Duration::from_millis(1));
    }
    // Keep Connected in the single event slot, blocking delivery of the next
    // event beyond the stability interval. Test snapshot and failed independently.
    let before = client.handle().diagnostics_snapshot().native.unwrap();
    disconnect_tx.send(()).unwrap();
    broker.join();
    thread::sleep(stability * 2);
    assert_capture_published_before_delivery(&client, before.generation);
    if observe_while_blocked {
        let snapshot = client.handle().reconnect_diagnostics();
        assert_eq!(snapshot.retries_since_reset, 1);
        assert_eq!(snapshot.reset_count, 0);
        assert_eq!(snapshot.remaining_stability, None);
    }
    assert!(matches!(
        events.recv_timeout(DEADLINE).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));
    if buffered_publish {
        let Some(WrapperEvent::IncomingPublish(publish)) = events.recv_timeout(DEADLINE).unwrap()
        else {
            panic!("expected queued PUBLISH before DISCONNECT");
        };
        assert_eq!(publish.payload.as_ref(), b"x");
    }
    let Some(WrapperEvent::BrokerDisconnect(disconnect)) = events.recv_timeout(DEADLINE).unwrap()
    else {
        panic!("expected broker DISCONNECT");
    };
    assert_eq!(disconnect.reason_code, 0);
    let Some(WrapperEvent::Disconnected { phase, error }) = events.recv_timeout(DEADLINE).unwrap()
    else {
        panic!("expected deferred connection error");
    };
    assert_eq!(phase, ConnectionPhase::Established);
    assert!(error.retryable());
    let Some(WrapperEvent::DriverTerminated(error)) = events.recv_timeout(DEADLINE).unwrap() else {
        panic!("expected retry exhaustion");
    };
    assert_eq!(error.code(), ErrorCode::ReconnectExhausted);
    client.join(DEADLINE).unwrap();
    let snapshot = client.handle().reconnect_diagnostics();
    assert_eq!(snapshot.cycles_started, 2);
    assert_eq!(snapshot.retries_since_reset, 1);
    assert_eq!(snapshot.reset_count, 0);
    assert_eq!(snapshot.stop_reason, ReconnectStopReason::Exhausted);
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
