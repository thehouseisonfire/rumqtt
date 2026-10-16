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
