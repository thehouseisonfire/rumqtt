use rumqttc::{
    AsyncClient, ClientError, MqttOptions, PublishAdmissionPolicy, PublishBudgetError,
    PublishBudgetLimits, PublishNoticeError, PublishOptions, PublishProperties, QoS,
};

fn client(count: usize, bytes: usize, capacity: usize) -> (AsyncClient, rumqttc::EventLoop) {
    AsyncClient::builder(MqttOptions::new("budget", "localhost"))
        .capacity(capacity)
        .publish_admission_policy(PublishAdmissionPolicy::EventLoopValidated)
        .publish_budget(PublishBudgetLimits {
            max_outstanding: count,
            max_bytes: bytes,
        })
        .build()
}

#[test]
fn cleanup_does_not_replenish_capacity_and_observers_do_not_own_reservations() {
    let (client, mut driver) = client(3, 64, 1);
    for expected in 1..=3 {
        let observer = client
            .try_publish_tracked("a", "data", PublishOptions::at_least_once())
            .unwrap();
        drop(observer);
        for _ in 0..8 {
            driver.clean();
        }
        let snapshot = client.publish_budget_snapshot().unwrap();
        assert_eq!(snapshot.outstanding, expected);
        assert_eq!(snapshot.retained_bytes, expected * 5);
    }
    assert!(matches!(
        client.try_publish("a", "data", PublishOptions::at_least_once()),
        Err(ClientError::PublishBudget {
            reason: PublishBudgetError::CountExhausted,
            ..
        })
    ));
    drop(driver);
    assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 0);
    assert!(matches!(
        client.try_publish("a", "data", PublishOptions::at_least_once()),
        Err(ClientError::RequestChannelDisconnected(_))
    ));
}

#[test]
fn failed_channel_admission_rolls_back_bytes_and_count() {
    let (client, _driver) = client(4, 64, 1);
    client
        .try_publish("a", "one", PublishOptions::at_least_once())
        .unwrap();
    assert!(matches!(
        client.try_publish("a", "two", PublishOptions::at_least_once()),
        Err(ClientError::RequestChannelFull(_))
    ));
    let snapshot = client.publish_budget_snapshot().unwrap();
    assert_eq!((snapshot.outstanding, snapshot.retained_bytes), (1, 4));
}

#[test]
fn bytes_properties_and_alias_expansion_are_bounded() {
    let (client, mut driver) = client(8, 16, 8);
    client
        .try_publish("a", [0; 15], PublishOptions::at_least_once())
        .unwrap();
    assert!(matches!(
        client.try_publish("a", "", PublishOptions::at_least_once()),
        Err(ClientError::PublishBudget {
            reason: PublishBudgetError::BytesExhausted,
            ..
        })
    ));
    assert!(matches!(
        client.try_publish("a", [0; 16], PublishOptions::at_least_once()),
        Err(ClientError::PublishBudget {
            reason: PublishBudgetError::TooLarge,
            waiter: None,
            ..
        })
    ));
    let mut properties = PublishProperties::default();
    properties.user_properties.push(("".into(), "".into()));
    assert!(matches!(
        client.try_publish(
            "a",
            "",
            PublishOptions::at_least_once().properties(properties)
        ),
        Err(ClientError::PublishBudget {
            reason: PublishBudgetError::TooLarge,
            ..
        })
    ));
    let properties = PublishProperties {
        topic_alias: Some(1),
        ..Default::default()
    };
    assert!(matches!(
        client.try_publish(
            "",
            "",
            PublishOptions::at_least_once().properties(properties)
        ),
        Err(ClientError::PublishBudget {
            reason: PublishBudgetError::TooLarge,
            ..
        })
    ));
    driver.clean();
    assert_eq!(client.publish_budget_snapshot().unwrap().retained_bytes, 16);
}

#[tokio::test]
async fn unsent_alias_replay_failure_releases_capacity_and_wakes_waiter() {
    let (client, mut driver) = client(1, 65536, 1);
    let properties = PublishProperties {
        topic_alias: Some(1),
        ..Default::default()
    };
    let observer = client
        .try_publish_tracked(
            "",
            "",
            PublishOptions::new(QoS::AtLeastOnce).properties(properties),
        )
        .unwrap();
    let progress = client.publish_admission_waiter().unwrap();
    driver.clean(); // Alias has never been bound or sent.
    tokio::time::timeout(std::time::Duration::from_secs(1), progress.wait_async())
        .await
        .unwrap();
    let outcome = observer.wait_outcome_async().await;
    assert_eq!(
        outcome.result,
        Err(PublishNoticeError::TopicAliasReplayUnavailable(1))
    );
    assert!(!outcome.possibly_transmitted);
    assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 0);
    client
        .try_publish("a", "ok", PublishOptions::at_least_once())
        .unwrap();
}

#[test]
fn direct_native_builders_keep_the_budget_opt_in() {
    let (client, _driver) = AsyncClient::builder(MqttOptions::new("legacy", "localhost"))
        .capacity(1)
        .build();
    assert_eq!(client.publish_budget_snapshot(), None);
}

#[test]
fn concurrent_producers_cannot_overbook_the_budget() {
    use std::sync::{Arc, Barrier};
    let (client, mut driver) = client(8, 40, 32);
    let barrier = Arc::new(Barrier::new(33));
    let producers: Vec<_> = (0..32)
        .map(|_| {
            let client = client.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                client.try_publish("a", "data", PublishOptions::at_least_once())
            })
        })
        .collect();
    barrier.wait();
    let mut admitted = 0;
    for producer in producers {
        match producer.join().unwrap() {
            Ok(()) => admitted += 1,
            Err(ClientError::PublishBudget {
                reason: PublishBudgetError::CountExhausted,
                ..
            }) => {}
            result => panic!("unexpected concurrent admission: {result:?}"),
        }
    }
    assert_eq!(admitted, 8);
    driver.clean();
    let snapshot = client.publish_budget_snapshot().unwrap();
    assert_eq!((snapshot.outstanding, snapshot.retained_bytes), (8, 40));
    drop(driver);
    assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 0);
}

#[test]
fn dropping_an_unpolled_driver_releases_all_queued_reservations() {
    let (client, driver) = client(1, 5, 1);
    let observer = client
        .try_publish_tracked("a", "data", PublishOptions::at_least_once())
        .unwrap();
    drop(driver);
    assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 0);
    assert_eq!(
        observer.wait_outcome().result,
        Err(PublishNoticeError::Recv)
    );
}

#[tokio::test]
async fn failed_channel_reservation_does_not_wake_its_own_admission_retry() {
    use futures_util::FutureExt;
    let (client, mut driver) = client(4, 20, 1);
    client
        .try_publish("a", "data", PublishOptions::at_least_once())
        .unwrap();
    let progress = client.publish_admission_waiter().unwrap();
    assert!(matches!(
        client.try_publish_tracked("a", "data", PublishOptions::at_least_once()),
        Err(ClientError::RequestChannelFull(_))
    ));
    assert!(progress.wait_async().now_or_never().is_none());
    // One poll must yield while the channel remains full, rather than retrying in that poll.
    assert!(
        client
            .publish("a", "data", PublishOptions::at_least_once())
            .now_or_never()
            .is_none()
    );
    assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 1);
    driver.clean();
    assert!(progress.wait_async().now_or_never().is_some());
    client
        .publish("a", "data", PublishOptions::at_least_once())
        .await
        .unwrap();
    assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 2);
}

#[test]
fn active_producers_and_repeated_cleanup_share_one_retained_work_bound() {
    use std::sync::{Arc, Barrier};
    let (client, mut driver) = client(8, 40, 2);
    let barrier = Arc::new(Barrier::new(9));
    let producers: Vec<_> = (0..8)
        .map(|_| {
            let client = client.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let mut admitted = 0;
                for _ in 0..128 {
                    match client.try_publish("a", "data", PublishOptions::at_least_once()) {
                        Ok(()) => admitted += 1,
                        Err(ClientError::RequestChannelFull(_))
                        | Err(ClientError::PublishBudget {
                            reason: PublishBudgetError::CountExhausted,
                            ..
                        }) => {}
                        result => panic!("unexpected admission: {result:?}"),
                    }
                    std::thread::yield_now();
                }
                admitted
            })
        })
        .collect();
    barrier.wait();
    for _ in 0..128 {
        driver.clean();
        let snapshot = client.publish_budget_snapshot().unwrap();
        assert!(snapshot.outstanding <= 8);
        assert!(snapshot.retained_bytes <= 40);
        std::thread::yield_now();
    }
    let admitted: usize = producers
        .into_iter()
        .map(|producer| producer.join().unwrap())
        .sum();
    assert!(admitted > 0);
    driver.clean();
    let snapshot = client.publish_budget_snapshot().unwrap();
    assert_eq!(
        (snapshot.outstanding, snapshot.retained_bytes),
        (admitted, admitted * 5)
    );
    drop(driver);
    assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 0);
}
