use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use bytes::Bytes;
use rumqttc_wrapper_core::*;

struct Broker(Child);
impl Drop for Broker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "requires the mosquitto executable (or MOSQUITTO_BIN)"]
fn real_broker_publishes_will_only_after_ungraceful_disconnect() {
    let directory = tempfile::tempdir().unwrap();
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    let path = directory.path().join("mosquitto.conf");
    std::fs::write(
        &path,
        format!("listener {port} 127.0.0.1\nallow_anonymous true\npersistence false\n"),
    )
    .unwrap();
    drop(reservation);
    let binary = std::env::var_os("MOSQUITTO_BIN").unwrap_or_else(|| "mosquitto".into());
    let mut broker = Broker(
        ProcessCommand::new(binary)
            .arg("-c")
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            Instant::now() < deadline && broker.0.try_wait().unwrap().is_none(),
            "broker did not become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    for mqtt5 in [false, true] {
        for graceful in [false, true] {
            let make_config = |id: &str| {
                if mqtt5 {
                    ClientConfig::v5(id, "127.0.0.1", port)
                } else {
                    ClientConfig::v4(id, "127.0.0.1", port)
                }
            };
            let mut observer = NativeClient::start(make_config("will-observer")).unwrap();
            let mut events = observer.take_events().unwrap();
            assert!(matches!(
                events.recv_timeout(Duration::from_secs(3)).unwrap(),
                Some(WrapperEvent::Connected { .. })
            ));
            observer
                .handle()
                .try_admit(Command::Subscribe(SubscribeCommand {
                    filters: vec![Subscription {
                        filter: "will/#".into(),
                        qos: QoS::AtLeastOnce,
                        protocol: SubscriptionProtocolOptions::VersionNeutral,
                    }],
                    protocol: SubscribeProtocolOptions::VersionNeutral,
                }))
                .unwrap()
                .completion
                .wait_timeout(Duration::from_secs(3))
                .unwrap();
            let mut config = make_config("will-source");
            config.common.last_will = Some(LastWillConfig {
                topic: "will/payload".into(),
                payload: Bytes::from_static(b"\0\xff"),
                qos: QoS::AtLeastOnce,
                retain: false,
                protocol: if mqtt5 {
                    LastWillProtocolOptions::V5(V5WillProperties {
                        will_delay_interval: Some(0),
                        payload_format_indicator: Some(0),
                        content_type: Some(String::new()),
                        correlation_data: Some(Bytes::new()),
                        user_properties: vec![
                            ("key".into(), "one".into()),
                            ("key".into(), String::new()),
                        ],
                        ..Default::default()
                    })
                } else {
                    LastWillProtocolOptions::VersionNeutral
                },
            });
            let mut source = NativeClient::start(config).unwrap();
            let mut source_events = source.take_events().unwrap();
            assert!(matches!(
                source_events.recv_timeout(Duration::from_secs(3)).unwrap(),
                Some(WrapperEvent::Connected { .. })
            ));
            if graceful {
                source.closer().close(Duration::from_secs(3)).unwrap();
            } else {
                source.handle().terminate_for_internal_panic();
                source.join(Duration::from_secs(3)).unwrap();
            }
            if graceful {
                assert!(
                    events
                        .recv_timeout(Duration::from_millis(250))
                        .unwrap()
                        .is_none()
                );
            } else {
                let Some(WrapperEvent::IncomingPublish(publish)) =
                    events.recv_timeout(Duration::from_secs(3)).unwrap()
                else {
                    panic!("missing will")
                };
                assert_eq!(publish.topic.as_ref(), b"will/payload");
                assert_eq!(publish.payload.as_ref(), b"\0\xff");
                assert_eq!(publish.qos, QoS::AtLeastOnce);
                if mqtt5 {
                    let properties = publish.v5_properties.unwrap();
                    assert_eq!(properties.correlation_data, Some(Bytes::new()));
                    assert_eq!(properties.content_type.as_deref(), Some(""));
                    assert_eq!(
                        properties.user_properties,
                        vec![("key".into(), "one".into()), ("key".into(), String::new())]
                    );
                }
            }
            observer.closer().close(Duration::from_secs(3)).unwrap();
        }
    }
}
