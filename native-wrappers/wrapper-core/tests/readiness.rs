#![cfg(feature = "readiness")]
mod support;

use rumqttc_wrapper_core::readiness::{NotificationContext, SourceKind};
use rumqttc_wrapper_core::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use support::*;

#[test]
fn stalled_host_preserves_bounded_readiness_and_backpressure_termination_while_shared_peer_progresses()
 {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (send, receive) = mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            receive.recv_timeout(DEADLINE).unwrap();
            socket
                .write_all(if mqtt5 {
                    b"\x30\x07\x00\x01a\x00one\x30\x07\x00\x01a\x00two"
                } else {
                    b"\x30\x06\x00\x01aone\x30\x06\x00\x01atwo"
                })
                .unwrap();
            assert_eq!(socket.read(&mut [0]).unwrap(), 0);
        });
        let execution = ExecutionContext::new(ExecutionOptions {
            worker_threads: 1,
            ..ExecutionOptions::default()
        })
        .unwrap();
        let context = NotificationContext::new(64).unwrap();
        let mut stalled_config = config(mqtt5, port);
        stalled_config.common.keep_alive = Duration::ZERO;
        stalled_config.common.event_buffer_capacity = 1;
        stalled_config.common.event_delivery_timeout = Duration::from_millis(150);
        let mut stalled = NativeClient::start_in(stalled_config, &execution).unwrap();
        let mut events = connected(&mut stalled);
        let event_key = context.register_events(&events.readiness(), 0).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer_broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            for _ in 0..33 {
                let publish = frame(&mut socket);
                assert_eq!(publish[0] >> 4, 3);
                socket
                    .write_all(&[0x40, 2, publish[5], publish[6]])
                    .unwrap();
            }
            assert_eq!(frame(&mut socket)[0] >> 4, 14);
        });
        let mut peer_config = config(mqtt5, port);
        peer_config.common.keep_alive = Duration::ZERO;
        peer_config.common.emit_outgoing_events = false;
        let mut peer = NativeClient::start_in(peer_config, &execution).unwrap();
        let _peer_events = connected(&mut peer);
        send.send(()).unwrap();
        // Deliberately do not drain the notification context or stalled events.
        for tag in 1..=32 {
            let admission = publish(&peer, b"peer");
            context
                .register_completion(&admission.completion, tag)
                .unwrap();
            terminal(&admission).unwrap();
        }
        let deadline = Instant::now() + DEADLINE;
        while !stalled.handle().diagnostics_snapshot().terminated {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        terminal(&publish(&peer, b"still-progressing")).unwrap();
        let mut observed = Vec::new();
        context.collect(64, |entry| observed.push(entry)).unwrap();
        assert_eq!(observed.len(), 33);
        assert_eq!(
            observed
                .iter()
                .filter(|entry| entry.kind == SourceKind::Completion)
                .count(),
            32
        );
        assert_eq!(context.usage().unwrap(), (33, 64));
        assert!(matches!(
            events.recv_timeout(DEADLINE).unwrap(),
            Some(WrapperEvent::IncomingPublish(_))
        ));
        let Some(WrapperEvent::DriverTerminated(error)) = events.recv_timeout(DEADLINE).unwrap()
        else {
            panic!("missing terminal status")
        };
        assert_eq!(error.code(), ErrorCode::EventBufferOverflow);
        let revision = observed
            .iter()
            .find(|entry| entry.key == event_key)
            .unwrap()
            .revision;
        context.acknowledge(event_key, revision).unwrap();
        stalled.join(DEADLINE).unwrap();
        peer.closer().close(DEADLINE).unwrap();
        drop(stalled);
        drop(peer);
        drop(events);
        broker.join();
        peer_broker.join();
        context.request_close();
        execution.request_shutdown();
        execution.join(DEADLINE).unwrap();
    }
}
