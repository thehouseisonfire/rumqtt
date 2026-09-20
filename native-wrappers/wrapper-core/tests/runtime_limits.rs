mod support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use rumqttc_wrapper_core::*;
use support::*;

#[test]
fn outgoing_inflight_obeys_local_and_broker_limits() {
    for (mqtt5, local, remote) in [(false, 2, 5), (true, 2, 5), (true, 5, 2)] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        match &mut config.protocol {
            ProtocolConfig::V4(v4) => v4.inflight_limit = local,
            ProtocolConfig::V5(v5) => v5.outgoing_inflight_upper_limit = Some(local),
        }
        let (admitted_tx, admitted_rx) = std::sync::mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            assert_eq!(frame(&mut socket)[0], 0x10);
            if mqtt5 {
                socket
                    .write_all(&[0x20, 6, 0, 0, 3, 0x21, 0, remote])
                    .unwrap();
            } else {
                socket.write_all(&[0x20, 2, 0, 0]).unwrap();
            }
            let first = publish_id(&mut socket, mqtt5);
            let second = publish_id(&mut socket, mqtt5);
            assert_ne!(first, second);
            admitted_rx.recv_timeout(DEADLINE).unwrap();
            // The third operation has been admitted, but cannot reach the wire
            // until one of the two occupied slots is acknowledged.
            socket
                .set_read_timeout(Some(Duration::from_millis(150)))
                .unwrap();
            let error = socket.read(&mut [0]).unwrap_err();
            assert!(matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ));
            socket.set_read_timeout(Some(DEADLINE)).unwrap();
            puback(&mut socket, first);
            let third = publish_id(&mut socket, mqtt5);
            assert_ne!(third, second);
            puback(&mut socket, second);
            puback(&mut socket, third);
            assert_eq!(frame(&mut socket)[0], 0xe0);
        });
        let mut client = NativeClient::start(config).unwrap();
        let _events = connected(&mut client);
        let operations: Vec<_> = (0..3).map(|_| publish(&client, b"payload")).collect();
        admitted_tx.send(()).unwrap();
        for operation in operations {
            assert_eq!(
                terminal(&operation).unwrap(),
                Completion::Publish(PublishCompletion::Qos1Acknowledged)
            );
        }
        client.closer().close(DEADLINE).unwrap();
        broker.join();
    }
}

#[test]
fn oversized_outgoing_publish_reports_protocol_failure_and_resolves_on_close() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        if let ProtocolConfig::V4(v4) = &mut config.protocol {
            v4.max_outgoing_packet_size = 32;
        }
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            frame(&mut socket);
            if mqtt5 {
                socket
                    .write_all(&[0x20, 8, 0, 0, 5, 0x27, 0, 0, 0, 32])
                    .unwrap();
            } else {
                socket.write_all(&[0x20, 2, 0, 0]).unwrap();
            }
            let mut byte = [0];
            assert_eq!(
                socket.read(&mut byte).unwrap(),
                0,
                "oversized packet reached broker"
            );
        });
        let mut client = NativeClient::start(config).unwrap();
        let mut events = connected(&mut client);
        let oversized = publish(&client, &[b'x'; 64]);
        let event = until(&mut events, |e| {
            matches!(e, WrapperEvent::Disconnected { .. })
        });
        let WrapperEvent::Disconnected { error, .. } = event else {
            unreachable!()
        };
        assert_eq!(error.kind(), ErrorKind::Protocol);
        client.closer().close_now(DEADLINE).unwrap();
        assert_eq!(
            terminal(&oversized).unwrap_err().delivery_status(),
            DeliveryStatus::Ambiguous
        );
        broker.join();
    }
}

#[test]
fn incoming_decoder_limit_is_independent_of_advertised_maximum() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.incoming_packet_size_limit = IncomingPacketLimit::Bytes(32);
        if let ProtocolConfig::V5(v5) = &mut config.protocol {
            v5.connect_properties.maximum_packet_size = Some(1024);
        }
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            let mut connect = frame(&mut socket);
            if mqtt5 {
                let rumqttc_v5::Packet::Connect(connect, _, _) =
                    rumqttc_v5::Packet::read(&mut connect, None).unwrap()
                else {
                    panic!("CONNECT")
                };
                assert_eq!(connect.properties.unwrap().max_packet_size, Some(1024));
                socket.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
            } else {
                socket.write_all(&[0x20, 2, 0, 0]).unwrap();
            }
            let _pending = publish_id(&mut socket, mqtt5);
            let mut packet = vec![0x30, if mqtt5 { 68 } else { 67 }, 0, 1, b'a'];
            if mqtt5 {
                packet.push(0);
            }
            packet.extend_from_slice(&[b'x'; 64]);
            socket.write_all(&packet).unwrap();
            // Keep the listener alive until shutdown, without acknowledging the
            // pending publish or accepting a new connection.
            let _ = socket.read(&mut [0]);
        });
        let mut client = NativeClient::start(config).unwrap();
        let mut events = connected(&mut client);
        let pending = publish(&client, b"small");
        let event = until(&mut events, |e| {
            matches!(
                e,
                WrapperEvent::Disconnected { .. } | WrapperEvent::DriverTerminated(_)
            )
        });
        let (WrapperEvent::Disconnected { error, .. } | WrapperEvent::DriverTerminated(error)) =
            event
        else {
            unreachable!()
        };
        assert_eq!(error.kind(), ErrorKind::Protocol);
        client.closer().close_now(DEADLINE).unwrap();
        let error = terminal(&pending).unwrap_err();
        assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
        broker.join();
    }
}
