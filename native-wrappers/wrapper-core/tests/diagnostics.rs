mod support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use rumqttc_wrapper_core::*;
use support::*;

fn wait_snapshot(
    handle: &ClientHandle,
    ready: impl Fn(&ClientDiagnosticsSnapshot) -> bool,
) -> ClientDiagnosticsSnapshot {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let snapshot = handle.diagnostics_snapshot();
        if ready(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "diagnostic observation timed out: {snapshot:?}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep tuning staging, wire barriers and native capture comparisons in one scenario"
)]
fn idle_native_capture_stays_dated_while_configuration_stages_and_advances_with_mqtt() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sent, sent_rx) = mpsc::channel();
        let (ack, ack_rx) = mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            let publish = frame(&mut socket);
            assert_eq!(publish[0] >> 4, 3);
            sent.send(()).unwrap();
            ack_rx.recv_timeout(DEADLINE).unwrap();
            socket
                .write_all(&[0x40, 2, publish[5], publish[6]])
                .unwrap();
            assert_eq!(frame(&mut socket)[0] >> 4, 14);
        });
        let mut config = config(mqtt5, port);
        config.common.keep_alive = Duration::ZERO;
        let mut client = start(config).unwrap();
        let _events = connected(&mut client);
        let handle = client.handle();
        let initial = handle.diagnostics_snapshot();
        let native = initial.native.as_ref().unwrap();
        assert!(native.connected);
        assert!(!native.session.connack.unwrap().session_resumed);
        assert_eq!(
            native.session.broker_only_session_resume,
            mqtt5.then_some(false)
        );
        assert_eq!(native.redirect.is_some(), mqtt5);
        let update = handle
            .try_configuration_update(RuntimeConfigUpdate {
                read_batch_size: FieldUpdate::Replace(11),
                ..Default::default()
            })
            .unwrap();
        let Completion::ConfigurationStaged(receipt) = terminal(&update).unwrap() else {
            panic!("expected staging");
        };
        let staged = handle.diagnostics_snapshot();
        let configuration = staged.configuration.as_ref().unwrap();
        assert_eq!(configuration.desired_tuning_revision, receipt.revision);
        assert_eq!(configuration.desired_tuning.read_batch_size, 11);
        assert_eq!(receipt.activation().0, ActivationState::Staged);
        assert!(Arc::ptr_eq(native, staged.native.as_ref().unwrap()));
        let capture_age = native.captured_at.elapsed();
        for _ in 0..500 {
            let observed = handle.diagnostics_snapshot();
            assert!(Arc::ptr_eq(native, observed.native.as_ref().unwrap()));
            assert!(observed.native.as_ref().unwrap().captured_at.elapsed() >= capture_age);
        }
        assert!(staged.captured_at >= native.captured_at);
        let work = publish(&client, b"diagnostic");
        sent_rx.recv_timeout(DEADLINE).unwrap();
        let inflight = wait_snapshot(&handle, |snapshot| {
            snapshot.native.as_ref().unwrap().outbound.inflight == 1
        });
        let current = inflight.native.as_ref().unwrap();
        assert!(current.generation > native.generation);
        assert_eq!(current.outbound.outgoing_publish, 1);
        assert_eq!(current.outbound.outgoing_publish_notices, 1);
        assert!(!current.outbound.outbound_drained);
        // This capture precedes tuning activation at the following safe boundary.
        assert_eq!(current.batching.configured_read_batch_size, 0);
        let legacy = handle.try_admit(Command::Diagnostics).unwrap();
        let Completion::Diagnostics(legacy) = terminal(&legacy).unwrap() else {
            panic!("expected legacy diagnostics");
        };
        assert_eq!(legacy.pending_requests, current.queues.pending_len);
        assert_eq!(
            legacy.queued_requests,
            current.queues.requests_rx_len + current.queues.control_requests_rx_len
        );
        assert_eq!(legacy.inflight_publishes, 1);
        ack.send(()).unwrap();
        terminal(&work).unwrap();
        let drained = wait_snapshot(&handle, |snapshot| {
            snapshot.native.as_ref().unwrap().outbound.outbound_drained
        });
        assert_eq!(
            drained
                .native
                .as_ref()
                .unwrap()
                .batching
                .configured_read_batch_size,
            11
        );
        assert_eq!(
            drained
                .native
                .as_ref()
                .unwrap()
                .batching
                .effective_read_batch_size,
            11
        );
        client.closer().close(DEADLINE).unwrap();
        let final_snapshot = handle.diagnostics_snapshot();
        assert!(final_snapshot.terminated);
        assert_eq!(final_snapshot.lifecycle, LifecycleState::Closed);
        assert!(final_snapshot.native.as_ref().unwrap().disconnect_complete);
        drop(client);
        drop(handle);
        assert_eq!(current.outbound.inflight, 1); // retained capture is immutable
        broker.join();
    }
}

#[test]
fn full_event_queue_keeps_reads_responsive_and_preserves_overflow_deadline_and_events() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (send, send_rx) = mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            send_rx.recv_timeout(DEADLINE).unwrap();
            socket
                .write_all(if mqtt5 {
                    b"\x30\x07\x00\x01a\x00one\x30\x07\x00\x01a\x00two"
                } else {
                    b"\x30\x06\x00\x01aone\x30\x06\x00\x01atwo"
                })
                .unwrap();
            assert_eq!(socket.read(&mut [0]).unwrap(), 0);
        });
        let mut config = config(mqtt5, port);
        config.common.keep_alive = Duration::ZERO;
        config.common.event_buffer_capacity = 1;
        config.common.event_delivery_timeout = Duration::from_millis(400);
        let mut client = start(config).unwrap();
        let mut events = connected(&mut client);
        let handle = client.handle();
        let initial_generation = handle.diagnostics_snapshot().native.unwrap().generation;
        let began = Instant::now();
        send.send(()).unwrap();
        let blocked = wait_snapshot(&handle, |snapshot| {
            snapshot.native.as_ref().unwrap().generation >= initial_generation + 2
        });
        let native = blocked.native.unwrap();
        for _ in 0..1000 {
            let read = handle.diagnostics_snapshot();
            assert!(Arc::ptr_eq(&native, read.native.as_ref().unwrap()));
        }
        assert!(began.elapsed() < Duration::from_millis(400));
        let ended = wait_snapshot(&handle, |snapshot| snapshot.terminated);
        assert!(began.elapsed() < Duration::from_secs(2));
        assert_eq!(ended.lifecycle, LifecycleState::Failed);
        assert!(Arc::ptr_eq(&native, ended.native.as_ref().unwrap()));
        assert!(!native.disconnect_complete);
        let Some(WrapperEvent::IncomingPublish(publish)) = events.recv_timeout(DEADLINE).unwrap()
        else {
            panic!("queued event was consumed by observation");
        };
        assert_eq!(publish.payload.as_ref(), b"one");
        let Some(WrapperEvent::DriverTerminated(error)) = events.recv_timeout(DEADLINE).unwrap()
        else {
            panic!("expected overflow termination");
        };
        assert_eq!(error.code(), ErrorCode::EventBufferOverflow);
        assert_eq!(
            ended.reconnect.last_failure.unwrap().code(),
            ErrorCode::EventBufferOverflow
        );
        broker.join();
    }
}

struct PendingConnector(mpsc::Sender<()>);
impl TransportConnector for PendingConnector {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        self.0.send(()).unwrap();
        Box::pin(std::future::pending())
    }
}

#[test]
fn stalled_attempt_and_immediate_abort_keep_initial_native_capture_without_completion_claim() {
    for mqtt5 in [false, true] {
        let (entered, entered_rx) = mpsc::channel();
        let mut config = config(mqtt5, 1883);
        config.common.connector = Some(TransportConnectorConfig {
            connector: Arc::new(PendingConnector(entered)),
            mode: TransportMode::Base,
        });
        let client = start(config).unwrap();
        entered_rx.recv_timeout(DEADLINE).unwrap();
        let handle = client.handle();
        let before = handle.diagnostics_snapshot();
        let native = before.native.unwrap();
        assert_eq!(native.generation, 1);
        assert!(!native.connected);
        assert_eq!(
            before.configuration.unwrap().connection.outcome,
            AttemptOutcome::Pending
        );
        for _ in 0..1000 {
            assert!(Arc::ptr_eq(
                &native,
                handle.diagnostics_snapshot().native.as_ref().unwrap()
            ));
        }
        client.closer().close_now(DEADLINE).unwrap();
        let after = handle.diagnostics_snapshot();
        assert!(after.terminated);
        assert!(!after.native.as_ref().unwrap().disconnect_complete);
        assert!(Arc::ptr_eq(&native, after.native.as_ref().unwrap()));
        assert_eq!(
            after.configuration.unwrap().connection.outcome,
            AttemptOutcome::Cancelled
        );
        drop(client);
        drop(handle);
        assert_eq!(native.generation, 1);
    }
}

#[cfg(feature = "ordered-shutdown")]
#[test]
fn wrapper_fence_observation_never_redates_or_overlays_the_native_queue_capture() {
    for mqtt5 in [false, true] {
        let (entered, entered_rx) = mpsc::channel();
        let mut config = config(mqtt5, 1883);
        config.common.connector = Some(TransportConnectorConfig {
            connector: Arc::new(PendingConnector(entered)),
            mode: TransportMode::Base,
        });
        let client = start(config).unwrap();
        entered_rx.recv_timeout(DEADLINE).unwrap();
        let handle = client.handle();
        let initial = handle.diagnostics_snapshot().native.unwrap();
        assert_eq!(
            initial.ordered.as_ref().unwrap().phase,
            OrderedShutdownPhase::Open
        );
        let fence = handle
            .try_admit(Command::OrderedDisconnect {
                timeout: Some(Duration::from_millis(50)),
            })
            .unwrap();
        let snapshot = handle.diagnostics_snapshot();
        assert!(Arc::ptr_eq(&initial, snapshot.native.as_ref().unwrap()));
        let wrapper = snapshot.ordered_wrapper.unwrap();
        assert_eq!(wrapper.phase, OrderedShutdownPhase::Approaching);
        assert!(wrapper.fence_sequence.is_some());
        assert_eq!(wrapper.local_queued_publishes, None);
        assert_eq!(initial.ordered.as_ref().unwrap().fence_sequence, None);
        assert!(terminal(&fence).is_err());
        client.closer().close_now(DEADLINE).unwrap();
        assert!(!initial.disconnect_complete);
    }
}

#[test]
fn concurrent_reads_preserve_peer_keepalive_and_close_in_both_execution_modes() {
    for shared in [false, true] {
        for mqtt5 in [false, true] {
            concurrent_reads(mqtt5, shared);
        }
    }
}

fn concurrent_reads(mqtt5: bool, shared: bool) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let context = shared.then(|| {
        ExecutionContext::new(ExecutionOptions {
            worker_threads: 1,
            ..ExecutionOptions::default()
        })
        .unwrap()
    });
    let (entered, entered_rx) = mpsc::channel();
    let mut stalled = config(mqtt5, 1883);
    stalled.common.connector = Some(TransportConnectorConfig {
        connector: Arc::new(PendingConnector(entered)),
        mode: TransportMode::Base,
    });
    let stalled = if let Some(context) = &context {
        NativeClient::start_in(stalled, context)
    } else {
        NativeClient::start(stalled)
    }
    .unwrap();
    entered_rx.recv_timeout(DEADLINE).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let stop = stop.clone();
            let handle = stalled.handle();
            thread::spawn(move || {
                let mut count = 0;
                while !stop.load(Ordering::Acquire) {
                    assert_eq!(handle.diagnostics_snapshot().native.unwrap().generation, 1);
                    count += 1;
                }
                count
            })
        })
        .collect();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (ping, ping_rx) = mpsc::channel();
    let broker = Broker::spawn(move || {
        let mut socket = accept(&listener);
        connect(&mut socket, mqtt5);
        for _ in 0..20 {
            let publish = frame(&mut socket);
            assert_eq!(publish[0] >> 4, 3);
            socket
                .write_all(&[0x40, 2, publish[5], publish[6]])
                .unwrap();
        }
        assert_eq!(frame(&mut socket)[0], 0xc0);
        socket.write_all(&[0xd0, 0]).unwrap();
        ping.send(()).unwrap();
        assert_eq!(frame(&mut socket)[0] >> 4, 14);
    });
    let mut config = config(mqtt5, port);
    config.common.keep_alive = Duration::from_secs(1);
    let mut peer = if let Some(context) = &context {
        NativeClient::start_in(config, context)
    } else {
        NativeClient::start(config)
    }
    .unwrap();
    let _events = connected(&mut peer);
    for _ in 0..20 {
        terminal(&publish(&peer, b"peer")).unwrap();
    }
    ping_rx.recv_timeout(DEADLINE).unwrap();
    peer.closer().close(DEADLINE).unwrap();
    stalled.closer().close_now(DEADLINE).unwrap();
    stop.store(true, Ordering::Release);
    for reader in readers {
        assert!(reader.join().unwrap() > 0);
    }
    broker.join();
}
