use rumqttc::{AsyncClient, Client, ClientError, DisconnectOptions, PublishOptions, QoS, Request};
use std::time::Duration;

const DRAIN: Duration = Duration::from_secs(3);

fn policies() -> [DisconnectOptions; 3] {
    [
        DisconnectOptions::default(),
        DisconnectOptions::graceful_with_timeout(DRAIN),
        DisconnectOptions::immediate(),
    ]
}

fn check_policy(request: &Request, policy: usize) {
    match (policy, request) {
        (0, Request::Disconnect(_)) | (2, Request::DisconnectNow(_)) => {}
        (1, Request::DisconnectWithTimeout(_, timeout)) => assert_eq!(*timeout, DRAIN),
        _ => panic!("wrong disconnect policy: {request:?}"),
    }
}

#[tokio::test]
async fn async_and_try_configured_calls_preserve_policy_and_external_fifo() {
    for trying in [false, true] {
        for (policy, options) in policies().into_iter().enumerate() {
            let (tx, rx) = flume::bounded(2);
            let client = AsyncClient::from_senders(tx);
            client
                .try_publish("before", "payload", PublishOptions::at_most_once())
                .unwrap();
            if trying {
                client.try_disconnect_with(options).unwrap();
            } else {
                client.disconnect_with(options).await.unwrap();
            }
            assert!(matches!(rx.try_recv().unwrap(), Request::Publish(_)));
            check_policy(&rx.try_recv().unwrap(), policy);
        }
    }
}

#[test]
fn blocking_and_try_configured_calls_preserve_policy_and_external_fifo() {
    for trying in [false, true] {
        for (policy, options) in policies().into_iter().enumerate() {
            let (tx, rx) = flume::bounded(2);
            let client = Client::from_sender(tx);
            client
                .try_publish("before", "payload", PublishOptions::at_most_once())
                .unwrap();
            if trying {
                client.try_disconnect_with(options).unwrap();
            } else {
                client.disconnect_with(options).unwrap();
            }
            assert!(matches!(rx.try_recv().unwrap(), Request::Publish(_)));
            check_policy(&rx.try_recv().unwrap(), policy);
        }
    }
}

#[test]
fn full_channel_errors_retain_the_configured_request() {
    for (policy, options) in policies().into_iter().enumerate() {
        let (tx, rx) = flume::bounded(1);
        let client = AsyncClient::from_senders(tx);
        client
            .try_publish("full", "payload", PublishOptions::new(QoS::AtMostOnce))
            .unwrap();
        let ClientError::RequestChannelFull(request) =
            client.try_disconnect_with(options).unwrap_err()
        else {
            panic!("expected full channel");
        };
        check_policy(&request, policy);
        assert!(matches!(rx.try_recv().unwrap(), Request::Publish(_)));
        assert!(rx.is_empty());
    }
}

use rumqttc::mqttbytes::v5::Packet;
use rumqttc::{
    ConnectionError, DisconnectProperties, DisconnectReasonCode, EventLoop, MqttOptions,
};
use tokio::net::TcpListener;
#[allow(dead_code)]
mod broker;

fn properties() -> DisconnectProperties {
    DisconnectProperties {
        session_expiry_interval: Some(0),
        reason_string: Some("configured shutdown".into()),
        user_properties: vec![("source".into(), "options-test".into())],
        server_reference: None,
    }
}

async fn drive(mut eventloop: EventLoop) -> Result<(), ConnectionError> {
    loop {
        match eventloop.poll().await {
            Ok(_) => {}
            Err(ConnectionError::RequestsDone) => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

#[tokio::test]
async fn every_policy_preserves_reason_and_properties_on_the_wire() {
    for options in policies() {
        for customization in 0..4 {
            let reason = if customization & 1 != 0 {
                DisconnectReasonCode::DisconnectWithWillMessage
            } else {
                DisconnectReasonCode::NormalDisconnection
            };
            let expected_properties = (customization & 2 != 0).then(properties);
            let mut configured = options.clone();
            if customization & 1 != 0 {
                configured = configured.reason(reason);
            }
            if let Some(properties) = &expected_properties {
                configured = configured.properties(properties.clone());
            }
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let (client, eventloop) =
                AsyncClient::builder(MqttOptions::new("configured-wire", ("127.0.0.1", port)))
                    .capacity(1)
                    .build();
            let driver = tokio::spawn(drive(eventloop));
            let mut broker = tokio::time::timeout(
                DRAIN,
                broker::Broker::from_listener(
                    listener,
                    broker::ConnectBehavior::Accept {
                        session_saved: false,
                    },
                ),
            )
            .await
            .unwrap();
            client.disconnect_with(configured).await.unwrap();
            let Some(Packet::Disconnect(packet)) = broker.read_packet_with_timeout(DRAIN).await
            else {
                panic!("expected configured DISCONNECT");
            };
            assert_eq!(packet.reason_code, reason);
            assert_eq!(packet.properties, expected_properties);
            tokio::time::timeout(DRAIN, driver)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }
}
