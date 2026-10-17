mod support;

use rumqttc_wrapper_core::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

#[test]
fn ready_completions_remain_observable_while_diagnostics_fill_the_queue() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
        config.common.event_buffer_capacity = 1;
        config.common.protocol_observations = ProtocolObservationSelection {
            incoming_control: true,
            precise_outgoing: true,
            raw_auth: false,
        };
        let broker = thread::spawn(move || {
            let mut socket = support::accept(&listener);
            support::connect(&mut socket, mqtt5);
            let id = support::publish_id(&mut socket, mqtt5).to_be_bytes();
            socket.write_all(&[0x40, 2, id[0], id[1]]).unwrap();
            // Immediate close can terminate without a DISCONNECT packet.
            let mut bytes = [0; 64];
            let _ = socket.read(&mut bytes);
        });
        let mut client = support::start(config).unwrap();
        let mut events = support::connected(&mut client);
        let completion = support::publish(&client, b"value").completion;
        // Outgoing fills the single queue slot; incoming ACK observation cannot enqueue.
        assert!(matches!(
            completion.wait_timeout(support::DEADLINE).unwrap(),
            Completion::Publish(PublishCompletion::Qos1Acknowledged)
        ));
        let outgoing = events.recv_timeout(support::DEADLINE).unwrap().unwrap();
        let incoming = events.recv_timeout(support::DEADLINE).unwrap().unwrap();
        assert_eq!(
            outgoing.protocol_observation().unwrap().kind(),
            PacketKind::Publish
        );
        assert_eq!(
            incoming.protocol_observation().unwrap().kind(),
            PacketKind::PubAck
        );
        assert_eq!(
            incoming
                .protocol_observation()
                .unwrap()
                .connection_generation(),
            Some(1)
        );
        client.closer().close_now(support::DEADLINE).unwrap();
        drop(client);
        assert_eq!(
            incoming.protocol_observation().unwrap().packet_id(),
            outgoing.protocol_observation().unwrap().packet_id()
        );
        broker.join().unwrap();
    }
}

fn queued_client(
    mqtt5: bool,
    context: &ExecutionContext,
    timeout: std::time::Duration,
) -> (NativeClient, EventConsumer, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
    config.common.event_buffer_capacity = 1;
    config.common.event_delivery_timeout = timeout;
    config.common.protocol_observations = ProtocolObservationSelection {
        incoming_control: true,
        precise_outgoing: true,
        raw_auth: false,
    };
    let broker = thread::spawn(move || {
        let mut socket = support::accept(&listener);
        support::connect(&mut socket, mqtt5);
        let id = support::publish_id(&mut socket, mqtt5).to_be_bytes();
        socket.write_all(&[0x40, 2, id[0], id[1]]).unwrap();
        let mut bytes = [0; 64];
        while socket.read(&mut bytes).is_ok_and(|n| n != 0) {}
    });
    let mut client = NativeClient::start_in(config, context).unwrap();
    let events = support::connected(&mut client);
    (client, events, broker)
}

#[test]
fn blocked_observations_do_not_starve_another_client_on_a_single_worker() {
    let context = ExecutionContext::new(ExecutionOptions {
        worker_threads: 1,
        ..Default::default()
    })
    .unwrap();
    let (first, _first_events, first_peer) = queued_client(false, &context, support::DEADLINE);
    let first_result = support::publish(&first, b"first");
    first_result
        .completion
        .wait_timeout(support::DEADLINE)
        .unwrap();
    let (second, _second_events, second_peer) = queued_client(true, &context, support::DEADLINE);
    support::publish(&second, b"second")
        .completion
        .wait_timeout(support::DEADLINE)
        .unwrap();
    second.closer().close_now(support::DEADLINE).unwrap();
    first.closer().close_now(support::DEADLINE).unwrap();
    first_peer.join().unwrap();
    second_peer.join().unwrap();
}

#[test]
fn selected_observations_keep_the_existing_overflow_failure_contract() {
    for mqtt5 in [false, true] {
        let context = ExecutionContext::new(ExecutionOptions::default()).unwrap();
        let (client, mut events, peer) =
            queued_client(mqtt5, &context, std::time::Duration::from_millis(100));
        support::publish(&client, b"queued")
            .completion
            .wait_timeout(support::DEADLINE)
            .unwrap();
        client.join(support::DEADLINE).unwrap();
        assert!(matches!(
            events.recv_timeout(support::DEADLINE).unwrap(),
            Some(WrapperEvent::Outgoing(_))
        ));
        match events.recv_timeout(support::DEADLINE).unwrap() {
            Some(WrapperEvent::DriverTerminated(error)) => {
                assert_eq!(error.code(), ErrorCode::EventBufferOverflow);
                assert!(!error.retryable());
            }
            other => panic!("unexpected terminal event: {other:?}"),
        }
        peer.join().unwrap();
    }
}

#[cfg(feature = "ordered-shutdown")]
#[test]
fn selected_observations_do_not_hide_an_ordered_shutdown_deadline() {
    for mqtt5 in [false, true] {
        let context = ExecutionContext::new(ExecutionOptions::default()).unwrap();
        let (client, _events, peer) = queued_client(mqtt5, &context, support::DEADLINE);
        let publish = support::publish(&client, b"queued").completion;
        publish.wait_timeout(support::DEADLINE).unwrap();
        let close = client
            .handle()
            .try_admit(Command::OrderedDisconnect {
                timeout: Some(std::time::Duration::from_millis(50)),
            })
            .unwrap();
        let error = close
            .completion
            .wait_timeout(support::DEADLINE)
            .unwrap_err();
        assert_eq!(
            error.ordered_disconnect_failure(),
            Some(OrderedDisconnectFailure::Timeout)
        );
        client.join(support::DEADLINE).unwrap();
        // Queue pressure and shutdown failure never replace the committed ACK result.
        assert!(matches!(
            publish.wait_timeout(support::DEADLINE).unwrap(),
            Completion::Publish(PublishCompletion::Qos1Acknowledged)
        ));
        peer.join().unwrap();
    }
}
