use matches::assert_matches;
use std::io::ErrorKind;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::{task, time};

mod broker;

use broker::{Broker as TestBroker, ConnectBehavior};
use rumqttc::mqttbytes::v5::{ConnAck, ConnectReturnCode, Packet};
use rumqttc::*;

const SETUP_TIMEOUT: Duration = Duration::from_secs(3);
const PHASE_TIMEOUT: Duration = Duration::from_secs(5);
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const KEEP_ALIVE_EARLY_TOLERANCE: Duration = Duration::from_secs(1);
const KEEP_ALIVE_LATE_TOLERANCE: Duration = Duration::from_secs(2);
const PERSISTENT_SESSION_EXPIRY: u32 = 60;

async fn reserve_listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

async fn listener_on(port: u16) -> TcpListener {
    TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap()
}

async fn start_requests(count: u8, qos: QoS, delay: u64, client: AsyncClient) {
    for i in 1..=count {
        let topic = "hello/world".to_owned();
        let payload = vec![i, 1, 2, 3];

        let _ = client.publish(topic, qos, false, payload).await;
        time::sleep(Duration::from_secs(delay)).await;
    }
}

async fn run(eventloop: &mut EventLoop, reconnect: bool) -> Result<(), ConnectionError> {
    'reconnect: loop {
        loop {
            let polled = eventloop.poll().await;
            println!("Polled = {polled:?}");
            match polled {
                Ok(_) => continue,
                Err(_) if reconnect => continue 'reconnect,
                Err(error) => return Err(error),
            }
        }
    }
}

async fn poll_ignoring_connect_races(eventloop: &mut EventLoop) -> Result<Event, ConnectionError> {
    loop {
        match eventloop.poll().await {
            Err(ConnectionError::Io(error)) if error.kind() == ErrorKind::ConnectionRefused => {
                continue;
            }
            value => return value,
        }
    }
}

fn assert_keep_alive_interval(elapsed: Duration, keep_alive: u16) {
    let expected = Duration::from_secs(u64::from(keep_alive));
    let lower = expected.saturating_sub(KEEP_ALIVE_EARLY_TOLERANCE);
    let upper = expected + KEEP_ALIVE_LATE_TOLERANCE;

    assert!(
        elapsed >= lower && elapsed <= upper,
        "keepalive interval out of bounds: expected {:?}..={:?}, got {:?}",
        lower,
        upper,
        elapsed
    );
}

#[tokio::test]
async fn connection_should_timeout_on_time() {
    let (listener, port) = reserve_listener().await;

    task::spawn(async move {
        let _broker = TestBroker::from_listener(listener, ConnectBehavior::StallAfterConnect).await;
        time::sleep(Duration::from_secs(10)).await;
    });

    time::sleep(Duration::from_secs(1)).await;
    let options = MqttOptions::new("dummy", ("127.0.0.1", port));
    let mut eventloop = EventLoop::new(options, 5);

    let start = Instant::now();
    let poll = eventloop.poll().await;
    let elapsed = start.elapsed();

    assert_matches!(poll, Err(ConnectionError::Timeout(_)));
    assert!(
        elapsed >= Duration::from_secs(5) && elapsed <= Duration::from_secs(7),
        "connect timeout should respect the configured timeout, got {elapsed:?}"
    );
}

#[tokio::test]
async fn idle_connection_triggers_pings_on_time() {
    let keep_alive = 1;
    let (listener, port) = reserve_listener().await;

    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_keep_alive(keep_alive);

    task::spawn(async move {
        let mut eventloop = EventLoop::new(options, 5);
        run(&mut eventloop, false).await.unwrap();
    });

    let mut broker = TestBroker::from_listener(
        listener,
        ConnectBehavior::Accept {
            session_saved: false,
        },
    )
    .await;
    let mut count = 0;
    let mut start = Instant::now();

    for _ in 0..3 {
        let packet = broker.read_packet().await.unwrap();
        match packet {
            Packet::PingReq(_) => {
                count += 1;
                assert_keep_alive_interval(start.elapsed(), keep_alive);
                broker.pingresp().await;
                start = Instant::now();
            }
            packet => panic!("Expecting ping, received {packet:?}"),
        }
    }

    assert_eq!(count, 3);
}

#[tokio::test]
async fn some_outgoing_and_no_incoming_should_trigger_pings_on_time() {
    let keep_alive = 5;
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_keep_alive(keep_alive);

    let (client, mut eventloop) = AsyncClient::new(options, 5);
    let publisher = client.clone();

    task::spawn(async move {
        start_requests(10, QoS::AtMostOnce, 1, publisher).await;
    });

    let eventloop_task = task::spawn(async move { run(&mut eventloop, false).await });

    let mut broker = TestBroker::from_listener(
        listener,
        ConnectBehavior::Accept {
            session_saved: false,
        },
    )
    .await;
    let mut count = 0;
    let mut start = Instant::now();

    loop {
        let event = broker.tick().await;

        if matches!(event, Event::Incoming(Incoming::PingReq(_))) {
            count += 1;
            assert_keep_alive_interval(start.elapsed(), keep_alive);
            broker.pingresp().await;
            if count == 3 {
                break;
            }

            start = Instant::now();
        }
    }

    eventloop_task.abort();
    let _ = eventloop_task.await;

    assert_eq!(count, 3);
}

#[tokio::test]
async fn some_incoming_and_no_outgoing_should_trigger_pings_on_time() {
    let keep_alive = 5;
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_keep_alive(keep_alive);

    let eventloop_task = task::spawn(async move {
        let mut eventloop = EventLoop::new(options, 5);
        run(&mut eventloop, false).await
    });

    let mut broker = TestBroker::from_listener(
        listener,
        ConnectBehavior::Accept {
            session_saved: false,
        },
    )
    .await;
    let mut count = 0;

    broker.spawn_publishes(10, QoS::AtMostOnce, 1).await;

    let mut start = Instant::now();
    loop {
        let event = broker.tick().await;

        if matches!(event, Event::Incoming(Incoming::PingReq(_))) {
            count += 1;
            assert_keep_alive_interval(start.elapsed(), keep_alive);
            broker.pingresp().await;
            if count == 3 {
                break;
            }

            start = Instant::now();
        }
    }

    eventloop_task.abort();
    let _ = eventloop_task.await;

    assert_eq!(count, 3);
}

#[tokio::test]
async fn detects_halfopen_connections_in_the_second_ping_request() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_keep_alive(5);

    task::spawn(async move {
        let mut broker = TestBroker::from_listener(
            listener,
            ConnectBehavior::Accept {
                session_saved: false,
            },
        )
        .await;
        broker.blackhole().await;
    });

    time::sleep(Duration::from_secs(1)).await;
    let start = Instant::now();
    let mut eventloop = EventLoop::new(options, 5);
    loop {
        if let Err(error) = eventloop.poll().await {
            match error {
                ConnectionError::MqttState(StateError::AwaitPingResp) => break,
                value => panic!("Expecting pingresp error. Found = {value:?}"),
            }
        }
    }

    assert_eq!(start.elapsed().as_secs(), 10);
}

#[tokio::test]
async fn requests_are_blocked_after_max_inflight_queue_size() {
    let inflight = 5;
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_outgoing_inflight_upper_limit(inflight);

    let (client, mut eventloop) = AsyncClient::new(options, 5);
    task::spawn(async move {
        start_requests(10, QoS::AtLeastOnce, 1, client).await;
    });

    task::spawn(async move {
        run(&mut eventloop, false).await.unwrap();
    });

    let mut broker = TestBroker::from_listener(
        listener,
        ConnectBehavior::Accept {
            session_saved: false,
        },
    )
    .await;
    for i in 1..=10 {
        let packet = broker.read_publish().await;

        if i > inflight {
            assert!(packet.is_none());
        }
    }
}

#[tokio::test]
async fn requests_are_recovered_after_inflight_queue_size_falls_below_max() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_outgoing_inflight_upper_limit(3);

    let (client, mut eventloop) = AsyncClient::new(options, 5);

    task::spawn(async move {
        start_requests(5, QoS::AtLeastOnce, 1, client).await;
        time::sleep(Duration::from_secs(60)).await;
    });

    task::spawn(async move {
        run(&mut eventloop, true).await.unwrap();
    });

    let mut broker = TestBroker::from_listener(
        listener,
        ConnectBehavior::Accept {
            session_saved: false,
        },
    )
    .await;

    assert!(broker.read_publish().await.is_some());
    assert!(broker.read_publish().await.is_some());
    assert!(broker.read_publish().await.is_some());
    assert!(broker.read_publish().await.is_none());

    broker.ack(1).await;
    assert!(broker.read_publish().await.is_some());
    assert!(broker.read_publish().await.is_none());

    broker.ack(2).await;
    assert!(broker.read_publish().await.is_some());
    assert!(broker.read_publish().await.is_none());
}

#[tokio::test]
async fn packet_id_collisions_are_detected_and_flow_control_is_applied() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_outgoing_inflight_upper_limit(4);

    let (client, mut eventloop) = AsyncClient::new(options, 5);
    let (release_acks_tx, release_acks_rx) = oneshot::channel::<()>();
    let (stop_broker_tx, mut stop_broker_rx) = oneshot::channel::<()>();

    let requests = task::spawn(async move {
        start_requests(15, QoS::AtLeastOnce, 0, client).await;
    });

    let broker = task::spawn(async move {
        let mut broker = TestBroker::from_listener(
            listener,
            ConnectBehavior::Accept {
                session_saved: false,
            },
        )
        .await;

        let packets = broker
            .wait_for_n_publishes(4, SETUP_TIMEOUT)
            .await
            .expect("didn't receive initial publishes");
        for (index, packet) in packets.iter().enumerate() {
            assert_eq!(packet.payload[0], (index + 1) as u8);
        }

        broker.ack_many(&[3, 4]).await;
        release_acks_rx.await.expect("ack release signal dropped");

        broker.ack_many(&[1, 2]).await;

        let packet = broker
            .read_publish_with_timeout(PHASE_TIMEOUT)
            .await
            .expect("missing publish after releasing ack blockage");
        broker.ack(packet.pkid).await;

        loop {
            tokio::select! {
                _ = &mut stop_broker_rx => break,
                packet = broker.read_packet() => {
                    if let Some(packet) = packet {
                        match packet {
                            Packet::Publish(publish) => broker.ack(publish.pkid).await,
                            Packet::PingReq(_) => {}
                            _ => {}
                        }
                    }
                }
            }
        }
    });

    time::timeout(TEST_TIMEOUT, async {
        loop {
            let event = poll_ignoring_connect_races(&mut eventloop)
                .await
                .expect("poll should not fail");
            println!("Poll = {event:?}");
            if event == Event::Outgoing(Outgoing::AwaitAck(1)) {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for collision await-ack signal");
    release_acks_tx
        .send(())
        .expect("broker task already exited");

    time::timeout(TEST_TIMEOUT, async {
        loop {
            let event = poll_ignoring_connect_races(&mut eventloop)
                .await
                .expect("poll should not fail");
            println!("Poll = {event:?}");
            if event == Event::Outgoing(Outgoing::Publish(1)) {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for republish of blocked packet id");

    stop_broker_tx.send(()).expect("broker task already exited");
    broker.await.unwrap();
    requests.abort();
}

#[tokio::test]
async fn packet_id_collisions_are_timed_out_on_second_ping() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options
        .set_outgoing_inflight_upper_limit(4)
        .set_keep_alive(1);

    let (client, mut eventloop) = AsyncClient::new(options, 10);
    let (stop_broker_tx, stop_broker_rx) = oneshot::channel::<()>();
    let requests = task::spawn(async move {
        start_requests(6, QoS::AtLeastOnce, 0, client).await;
    });

    let broker = task::spawn(async move {
        let mut broker = TestBroker::from_listener(
            listener,
            ConnectBehavior::Accept {
                session_saved: false,
            },
        )
        .await;
        let packets = broker
            .wait_for_n_publishes(4, SETUP_TIMEOUT)
            .await
            .expect("didn't receive initial inflight publishes");

        for (index, packet) in packets.iter().enumerate() {
            assert_eq!(packet.payload[0], (index + 1) as u8);
        }

        broker.ack_many(&[3, 4]).await;

        time::timeout(PHASE_TIMEOUT, async {
            loop {
                if matches!(broker.read_packet().await, Some(Packet::PingReq(_))) {
                    break;
                }
            }
        })
        .await
        .expect("didn't observe first keepalive ping");

        stop_broker_rx.await.expect("stop signal dropped");
    });

    time::timeout(TEST_TIMEOUT, async {
        loop {
            let event = poll_ignoring_connect_races(&mut eventloop)
                .await
                .expect("poll should not fail");
            println!("Poll = {event:?}");
            if event == Event::Outgoing(Outgoing::AwaitAck(1)) {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for collision await-ack signal");

    time::timeout(TEST_TIMEOUT, async {
        loop {
            match poll_ignoring_connect_races(&mut eventloop).await {
                Err(ConnectionError::MqttState(StateError::CollisionTimeout)) => break,
                Ok(event) => println!("Poll = {event:?}"),
                Err(error) => panic!("Expected CollisionTimeout, got {error:?}"),
            }
        }
    })
    .await
    .expect("timed out waiting for collision timeout error");

    stop_broker_tx.send(()).expect("broker task already exited");
    broker.await.unwrap();
    requests.abort();
}

#[tokio::test]
async fn next_poll_after_connect_failure_reconnects() {
    let (listener, port) = reserve_listener().await;
    let options = MqttOptions::new("dummy", ("127.0.0.1", port));

    task::spawn(async move {
        let _broker =
            TestBroker::from_listener(listener, ConnectBehavior::RefuseBadUserNamePassword).await;
        let listener = listener_on(port).await;
        let _broker = TestBroker::from_listener(
            listener,
            ConnectBehavior::Accept {
                session_saved: false,
            },
        )
        .await;
        time::sleep(Duration::from_secs(15)).await;
    });

    time::sleep(Duration::from_secs(1)).await;
    let mut eventloop = EventLoop::new(options, 5);

    match eventloop.poll().await {
        Err(ConnectionError::ConnectionRefused(ConnectReturnCode::BadUserNamePassword)) => {}
        value => panic!("Expected bad username password error. Found = {value:?}"),
    }

    match eventloop.poll().await {
        Ok(Event::Incoming(Packet::ConnAck(ConnAck {
            code: ConnectReturnCode::Success,
            session_present: false,
            properties: None,
        }))) => {}
        value => panic!("Expected ConnAck Success. Found = {value:?}"),
    }
}

#[tokio::test]
async fn reconnection_resumes_from_the_previous_state() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options
        .set_keep_alive(5)
        .set_clean_start(false)
        .set_session_expiry_interval(Some(PERSISTENT_SESSION_EXPIRY));

    let (client, mut eventloop) = AsyncClient::new(options, 5);
    task::spawn(async move {
        start_requests(10, QoS::AtLeastOnce, 1, client).await;
        time::sleep(Duration::from_secs(10)).await;
    });

    task::spawn(async move {
        run(&mut eventloop, true).await.unwrap();
    });

    let mut broker = TestBroker::from_listener(
        listener,
        ConnectBehavior::Accept {
            session_saved: false,
        },
    )
    .await;
    for i in 1..=2 {
        let packet = broker.read_publish().await.unwrap();
        assert_eq!(i, packet.payload[0]);
        broker.ack(packet.pkid).await;
    }

    let listener = listener_on(port).await;
    let mut broker = TestBroker::from_listener(
        listener,
        ConnectBehavior::Accept {
            session_saved: true,
        },
    )
    .await;
    for i in 3..=4 {
        let packet = broker.read_publish().await.unwrap();
        assert_eq!(i, packet.payload[0]);
        broker.ack(packet.pkid).await;
    }
}

#[tokio::test]
async fn reconnection_resends_unacked_packets_from_the_previous_connection_first() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options
        .set_keep_alive(5)
        .set_clean_start(false)
        .set_session_expiry_interval(Some(PERSISTENT_SESSION_EXPIRY));

    let (client, mut eventloop) = AsyncClient::new(options, 5);
    task::spawn(async move {
        start_requests(10, QoS::AtLeastOnce, 1, client).await;
        time::sleep(Duration::from_secs(10)).await;
    });

    task::spawn(async move {
        run(&mut eventloop, true).await.unwrap();
    });

    let mut broker = TestBroker::from_listener(
        listener,
        ConnectBehavior::Accept {
            session_saved: false,
        },
    )
    .await;
    for i in 1..=2 {
        let packet = broker.read_publish().await.unwrap();
        assert_eq!(i, packet.payload[0]);
    }

    let listener = listener_on(port).await;
    let mut broker = TestBroker::from_listener(
        listener,
        ConnectBehavior::Accept {
            session_saved: true,
        },
    )
    .await;
    for i in 1..=6 {
        let packet = broker.read_publish().await.unwrap();
        assert_eq!(i, packet.payload[0]);
    }
}

#[tokio::test]
async fn reconnection_with_out_of_order_pubacks_resends_oldest_unacked_publish_first() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options
        .set_keep_alive(5)
        .set_clean_start(false)
        .set_session_expiry_interval(Some(PERSISTENT_SESSION_EXPIRY))
        .set_outgoing_inflight_upper_limit(4);

    let (client, mut eventloop) = AsyncClient::new(options, 10);
    task::spawn(async move {
        start_requests(8, QoS::AtLeastOnce, 0, client).await;
        time::sleep(Duration::from_secs(10)).await;
    });

    task::spawn(async move {
        run(&mut eventloop, true).await.unwrap();
    });

    {
        let mut broker = TestBroker::from_listener(
            listener,
            ConnectBehavior::Accept {
                session_saved: false,
            },
        )
        .await;
        let publishes = broker
            .wait_for_n_publishes(4, SETUP_TIMEOUT)
            .await
            .expect("didn't receive initial inflight publishes");
        for (index, publish) in publishes.iter().enumerate() {
            assert_eq!(publish.payload[0], (index + 1) as u8);
        }

        broker.ack(publishes[1].pkid).await;
    }

    let listener = listener_on(port).await;
    let mut broker = TestBroker::from_listener(
        listener,
        ConnectBehavior::Accept {
            session_saved: true,
        },
    )
    .await;
    let first_replayed = broker
        .read_publish_with_timeout(PHASE_TIMEOUT)
        .await
        .expect("missing first replayed publish after reconnect");
    assert_eq!(first_replayed.payload[0], 1);
    broker.ack(first_replayed.pkid).await;
}

#[tokio::test]
async fn reconnection_clean_both_pending_packets_and_collision_when_clean_start_is_true() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options
        .set_outgoing_inflight_upper_limit(2)
        .set_keep_alive(2)
        .set_clean_start(true);

    task::spawn(async move {
        let mut first_listener = Some(listener);
        loop {
            let listener = match first_listener.take() {
                Some(listener) => listener,
                None => listener_on(port).await,
            };
            let mut broker = TestBroker::from_listener(
                listener,
                ConnectBehavior::Accept {
                    session_saved: false,
                },
            )
            .await;

            let mut first_publish_unacked = false;
            while let Some(packet) = broker.read_packet().await {
                match packet {
                    Packet::PingReq(_) => broker.pingresp().await,
                    Packet::Publish(publish) => {
                        if first_publish_unacked {
                            broker.ack(publish.pkid).await;
                        }
                        first_publish_unacked = true;
                    }
                    _ => {}
                }
                time::sleep(Duration::from_secs_f64(0.5)).await;
            }
        }
    });

    let (client, mut eventloop) = AsyncClient::new(options, 5);
    task::spawn(async move {
        start_requests(3, QoS::AtLeastOnce, 1, client).await;
        time::sleep(Duration::from_secs(10)).await;
    });

    loop {
        if let Err(ConnectionError::MqttState(StateError::CollisionTimeout)) =
            eventloop.poll().await
        {
            break;
        }
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    match eventloop.poll().await {
        Ok(Event::Incoming(Packet::ConnAck(ConnAck {
            code: ConnectReturnCode::Success,
            session_present: false,
            properties: None,
        }))) => {
            assert!(eventloop.pending_is_empty());
            assert!(eventloop.state.collision.is_none());
        }
        value => panic!("Expected ConnAck Success. Found = {value:?}"),
    }
}
