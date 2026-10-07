#![cfg(feature = "socks-proxy")]

mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use rumqttc_wrapper_core::*;
use support::*;

fn socks_reply(socket: &mut TcpStream, reply: u8) {
    let mut greeting = [0; 3];
    socket.read_exact(&mut greeting).unwrap();
    assert_eq!(greeting, [5, 1, 0]);
    socket.write_all(&[5, 0]).unwrap();
    let mut request = [0; 10];
    socket.read_exact(&mut request).unwrap();
    assert_eq!(&request[..4], &[5, 1, 0, 1]);
    socket
        .write_all(&[5, reply, 0, 1, 127, 0, 0, 1, 0, 1])
        .unwrap();
}

fn proxied_config(mqtt5: bool, port: u16, budget: u64) -> ClientConfig {
    let mut config = config(mqtt5, 1883);
    config.common.proxy = Some(ProxyConfig::Socks5 {
        host: "127.0.0.1".into(),
        port,
        credentials: None,
    });
    config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
        initial_delay: Duration::from_millis(1),
        maximum_delay: Duration::from_millis(1),
        jitter: ReconnectJitter::None,
        budget: RetryBudget::Limited(budget),
        ..ReconnectConfig::default()
    });
    config
}

#[test]
fn transient_socks_replies_recover_in_both_protocols() {
    for mqtt5 in [false, true] {
        // General server failure, unreachable network/host, refusal and TTL expiry.
        for reply in [1, 3, 4, 5, 6] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker = Broker::spawn(move || {
                socks_reply(&mut accept(&listener), reply);
                let mut socket = accept(&listener);
                socks_reply(&mut socket, 0);
                connect(&mut socket, mqtt5);
                assert_eq!(frame(&mut socket)[0], 0xe0);
            });
            let mut client = start(proxied_config(mqtt5, port, 1)).unwrap();
            let mut events = client.take_events().unwrap();
            let WrapperEvent::Disconnected { error, .. } = until(&mut events, |event| {
                matches!(event, WrapperEvent::Disconnected { .. })
            }) else {
                unreachable!()
            };
            assert!(error.retryable());
            until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            let snapshot = client.handle().reconnect_diagnostics();
            assert_eq!(snapshot.cycles_started, 2);
            assert_eq!(snapshot.retries_since_reset, 1);
            client.closer().close(DEADLINE).unwrap();
            broker.join();
        }
    }
}

#[test]
fn transient_socks_replies_exhaust_the_budget_and_permanent_replies_stop_immediately() {
    for mqtt5 in [false, true] {
        // Ruleset denial, unsupported command/address, and unknown peer reply.
        for reply in [2, 3, 4, 5, 7, 8, 0xff] {
            let transient = matches!(reply, 3..=5);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker_listener = listener.try_clone().unwrap();
            let broker = Broker::spawn(move || {
                for _ in 0..if transient { 2 } else { 1 } {
                    socks_reply(&mut accept(&broker_listener), reply);
                }
            });
            let client = start(proxied_config(mqtt5, port, 1)).unwrap();
            client.join(DEADLINE).unwrap();
            let error = client
                .handle()
                .connection()
                .try_wait()
                .unwrap()
                .unwrap_err();
            let snapshot = client.handle().reconnect_diagnostics();
            assert_eq!(snapshot.cycles_started, if transient { 2 } else { 1 });
            assert_eq!(snapshot.retries_since_reset, u64::from(transient));
            assert_eq!(snapshot.last_failure.unwrap().retryable(), transient);
            if transient {
                assert_eq!(error.code(), ErrorCode::ReconnectExhausted);
                assert_eq!(snapshot.stop_reason, ReconnectStopReason::Exhausted);
            } else {
                assert_eq!(snapshot.stop_reason, ReconnectStopReason::TerminalFailure);
            }
            broker.join();
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
}
