mod support;

use std::io::Write;
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rumqttc_wrapper_core::*;
use support::*;

#[test]
fn retained_failure_keeps_its_origin_after_profile_update_and_reconnection() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (drop_first, drop_rx) = mpsc::channel();
        let (second_accepted, accepted_rx) = mpsc::channel();
        let (allow_second, allow_rx) = mpsc::channel();
        let broker = thread::spawn(move || {
            let mut first = accept(&listener);
            connect(&mut first, mqtt5);
            drop_rx.recv_timeout(DEADLINE).unwrap();
            drop(first);
            let mut second = accept(&listener);
            let packet = frame(&mut second);
            assert_eq!(packet[0], 0x10);
            second_accepted.send(()).unwrap();
            allow_rx.recv_timeout(DEADLINE).unwrap();
            second
                .write_all(if mqtt5 {
                    &[0x20, 3, 0, 0, 0]
                } else {
                    &[0x20, 2, 0, 0]
                })
                .unwrap();
            assert_eq!(frame(&mut second)[0] >> 4, 14);
        });
        let mut config = config(mqtt5, port);
        config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
            initial_delay: Duration::from_millis(150),
            maximum_delay: Duration::from_millis(150),
            jitter: ReconnectJitter::None,
            ..Default::default()
        });
        let mut client = start(config).unwrap();
        let mut events = connected(&mut client);
        let handle = client.handle();
        let update = handle
            .try_configuration_update(RuntimeConfigUpdate {
                credentials: FieldUpdate::Replace(BrokerCredentials {
                    username: Some("new-user".into()),
                    password: None,
                }),
                ..Default::default()
            })
            .unwrap();
        let Completion::ConfigurationStaged(receipt) =
            update.completion.wait_timeout(DEADLINE).unwrap()
        else {
            panic!("expected configuration staging");
        };
        drop_first.send(()).unwrap();
        let WrapperEvent::Disconnected { error, .. } = until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        }) else {
            unreachable!()
        };
        let original = error.native_failure().copied().unwrap();
        assert!(error.retryable());
        assert_eq!(error.context().generation, Some(1));
        accepted_rx.recv_timeout(DEADLINE).unwrap();
        allow_second.send(()).unwrap();
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        assert_eq!(receipt.activation().1, ActivationState::Activated);
        let last = handle.reconnect_diagnostics().last_failure.unwrap();
        assert_eq!(last.context(), error.context());
        assert_eq!(
            last.configuration_revision(),
            error.configuration_revision()
        );
        assert_eq!(last.native_failure(), Some(&original));
        assert_eq!(last.message(), last.code().as_str());
        client.closer().close_now(DEADLINE).unwrap();
        drop(client);
        drop(handle);
        assert_eq!(last.native_failure(), Some(&original));
        broker.join().unwrap();
    }
}
