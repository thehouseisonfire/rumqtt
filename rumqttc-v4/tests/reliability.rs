use matches::assert_matches;
use std::io::ErrorKind;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::{task, time};

mod broker;

use broker::Broker as TestBroker;
use rumqttc::*;

const SETUP_TIMEOUT: Duration = Duration::from_secs(3);
const PHASE_TIMEOUT: Duration = Duration::from_secs(5);
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const KEEP_ALIVE_EARLY_TOLERANCE: Duration = Duration::from_secs(1);
const KEEP_ALIVE_LATE_TOLERANCE: Duration = Duration::from_secs(2);
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

        client
            .publish(topic, qos, false, payload)
            .await
            .expect("test publish should be queued");
        time::sleep(Duration::from_secs(delay)).await;
    }
}

async fn start_requests_with_payload(
    count: u8,
    qos: QoS,
    delay: u64,
    client: AsyncClient,
    payload: usize,
) {
    for i in 1..=count {
        let topic = "hello/world".to_owned();
        let payload = vec![i; payload];

        client
            .publish(topic, qos, false, payload)
            .await
            .expect("test publish should be queued");
        time::sleep(Duration::from_secs(delay)).await;
    }
}

async fn run(eventloop: &mut EventLoop, reconnect: bool) -> Result<(), ConnectionError> {
    'reconnect: loop {
        loop {
            let o = eventloop.poll().await;
            println!("Polled = {o:?}");
            match o {
                Ok(_) => {}
                Err(_) if reconnect => continue 'reconnect,
                Err(e) => return Err(e),
            }
        }
    }
}

async fn poll_ignoring_connect_races(eventloop: &mut EventLoop) -> Result<Event, ConnectionError> {
    loop {
        match eventloop.poll().await {
            Err(ConnectionError::Io(error)) if error.kind() == ErrorKind::ConnectionRefused => {}
            value => return value,
        }
    }
}

async fn _tick(
    eventloop: &mut EventLoop,
    reconnect: bool,
    count: usize,
) -> Result<(), ConnectionError> {
    'reconnect: loop {
        for i in 0..count {
            let o = eventloop.poll().await;
            println!("{i}. Polled = {o:?}");
            match o {
                Ok(_) => {}
                Err(_) if reconnect => continue 'reconnect,
                Err(e) => return Err(e),
            }
        }

        break;
    }

    Ok(())
}

fn assert_keep_alive_interval(elapsed: Duration, keep_alive: u16) {
    let expected = Duration::from_secs(u64::from(keep_alive));
    let lower = expected.saturating_sub(KEEP_ALIVE_EARLY_TOLERANCE);
    let upper = expected + KEEP_ALIVE_LATE_TOLERANCE;

    assert!(
        elapsed >= lower && elapsed <= upper,
        "keepalive interval out of bounds: expected {lower:?}..={upper:?}, got {elapsed:?}"
    );
}

#[tokio::test]
async fn connection_should_timeout_on_time() {
    let (listener, port) = reserve_listener().await;

    task::spawn(async move {
        let _broker = TestBroker::from_listener(listener, 3, false).await;
        time::sleep(Duration::from_secs(10)).await;
    });

    time::sleep(Duration::from_secs(1)).await;
    let options = MqttOptions::new("dummy", ("127.0.0.1", port));
    let mut eventloop = EventLoop::new(options, 5);

    let start = Instant::now();
    let o = eventloop.poll().await;
    let elapsed = start.elapsed();

    assert_matches!(o, Err(ConnectionError::NetworkTimeout));

    let expected = Duration::from_secs(5);
    assert!(
        elapsed >= expected && elapsed <= expected + Duration::from_secs(1),
        "connection timeout happened outside expected window: expected around {expected:?}, got {elapsed:?}"
    );
}

//
// All keep alive tests here
//

#[test]
fn test_zero_keep_alive_values() {
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", 1885));
    options.set_keep_alive(0);
}

#[test]
fn test_valid_keep_alive_values() {
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", 1885));
    options.set_keep_alive(1);
}

#[tokio::test]
async fn idle_connection_triggers_pings_on_time() {
    let keep_alive = 1;
    let (listener, port) = reserve_listener().await;

    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_keep_alive(keep_alive);

    // Create client eventloop and poll
    task::spawn(async move {
        let mut eventloop = EventLoop::new(options, 5);
        run(&mut eventloop, false).await.unwrap();
    });

    let mut broker = TestBroker::from_listener(listener, 0, false).await;
    let mut count = 0;
    let mut start = Instant::now();

    for _ in 0..3 {
        let packet = broker.read_packet().await.unwrap();
        match packet {
            Packet::PingReq => {
                count += 1;
                assert_keep_alive_interval(start.elapsed(), keep_alive);
                broker.pingresp().await;
                start = Instant::now();
            }
            _ => {
                panic!("Expecting ping, Received: {packet:?}");
            }
        }
    }

    assert_eq!(count, 3);
}

#[tokio::test]
async fn regular_outgoing_packets_delay_keepalive_ping() {
    let keep_alive = 2;
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));

    options.set_keep_alive(keep_alive);

    // start sending qos0 publishes. this makes sure that there is
    // outgoing activity but no incoming activity
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(5).build();
    let publisher = client.clone();

    // Start sending publishes
    task::spawn(async move {
        start_requests(4, QoS::AtMostOnce, 1, publisher).await;
    });

    // start the eventloop
    let eventloop_task = task::spawn(async move { run(&mut eventloop, false).await });

    let mut broker = TestBroker::from_listener(listener, 0, false).await;
    let mut publishes = 0;
    let mut last_publish_at = Instant::now();

    while publishes < 4 {
        let event = broker.tick().await;
        match event {
            Event::Incoming(Incoming::Publish(_)) => {
                publishes += 1;
                last_publish_at = Instant::now();
            }
            Event::Incoming(Incoming::PingReq) => {
                panic!(
                    "PINGREQ should not be sent while other Control Packets keep the connection active"
                )
            }
            event => panic!("unexpected event while waiting for outgoing publishes: {event:?}"),
        }
    }

    match broker.tick().await {
        Event::Incoming(Incoming::PingReq) => {
            assert_keep_alive_interval(last_publish_at.elapsed(), keep_alive);
            broker.pingresp().await;
        }
        event => panic!("expected idle PINGREQ after outgoing publishes stopped, got {event:?}"),
    }

    eventloop_task.abort();
    assert!(
        eventloop_task.await.is_err(),
        "eventloop task should be aborted"
    );
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

    let mut broker = TestBroker::from_listener(listener, 0, false).await;
    let mut count = 0;

    // Start sending qos 0 publishes to the client. This triggers
    // some incoming and no outgoing packets in the client
    broker.spawn_publishes(10, QoS::AtMostOnce, 1);

    let mut start = Instant::now();
    loop {
        let event = broker.tick().await;

        if event == Event::Incoming(Incoming::PingReq) {
            // wait for 3 pings
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
    assert!(
        eventloop_task.await.is_err(),
        "eventloop task should be aborted"
    );

    assert_eq!(count, 3);
}

#[tokio::test]
async fn detects_halfopen_connections_in_the_second_ping_request() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_keep_alive(5);

    // A broker which consumes packets but doesn't reply
    task::spawn(async move {
        let mut broker = TestBroker::from_listener(listener, 0, false).await;
        broker.blackhole().await;
    });

    time::sleep(Duration::from_secs(1)).await;
    let start = Instant::now();
    let mut eventloop = EventLoop::new(options, 5);
    loop {
        if let Err(e) = eventloop.poll().await {
            match e {
                ConnectionError::MqttState(StateError::AwaitPingResp) => break,
                v => panic!("Expecting pingresp error. Found = {v:?}"),
            }
        }
    }

    assert_eq!(start.elapsed().as_secs(), 10);
}

//
// All flow control tests here
//

#[tokio::test]
async fn requests_are_blocked_after_max_inflight_queue_size() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_inflight(5);
    let inflight = options.inflight();

    // start sending qos0 publishes. this makes sure that there is
    // outgoing activity but no incoming activity
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(5).build();
    task::spawn(async move {
        start_requests(10, QoS::AtLeastOnce, 1, client).await;
    });

    // start the eventloop
    task::spawn(async move {
        run(&mut eventloop, false).await.unwrap();
    });

    let mut broker = TestBroker::from_listener(listener, 0, false).await;
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
    options.set_inflight(3);

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(5).build();

    task::spawn(async move {
        start_requests(5, QoS::AtLeastOnce, 1, client).await;
        time::sleep(Duration::from_secs(60)).await;
    });

    // start the eventloop
    task::spawn(async move {
        run(&mut eventloop, true).await.unwrap();
    });

    let mut broker = TestBroker::from_listener(listener, 0, false).await;

    // packet 1, 2, and 3
    assert!(broker.read_publish().await.is_some());
    assert!(broker.read_publish().await.is_some());
    assert!(broker.read_publish().await.is_some());

    // no packet 4. client inflight full as there aren't acks yet
    assert!(broker.read_publish().await.is_none());

    // ack packet 1 and client would produce packet 4
    broker.ack(1).await;
    assert!(broker.read_publish().await.is_some());
    assert!(broker.read_publish().await.is_none());

    // ack packet 2 and client would produce packet 5
    broker.ack(2).await;
    assert!(broker.read_publish().await.is_some());
    assert!(broker.read_publish().await.is_none());
}

#[tokio::test]
async fn bounded_publish_backpressure_is_preserved_while_inflight_is_full() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_inflight(1);

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(0).build();
    let eventloop_task = task::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(_) => {}
                Err(ConnectionError::RequestsDone) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    });

    let mut broker = TestBroker::from_listener(listener, 0, false).await;

    client
        .publish("test/topic", QoS::AtLeastOnce, false, "first")
        .await
        .unwrap();
    let first = broker
        .read_publish_with_timeout(PHASE_TIMEOUT)
        .await
        .expect("missing first publish");

    client
        .publish("test/topic", QoS::AtLeastOnce, false, "second")
        .await
        .unwrap();

    let mut third = task::spawn({
        let client = client.clone();
        async move {
            client
                .publish("test/topic", QoS::AtLeastOnce, false, "third")
                .await
        }
    });

    assert!(
        time::timeout(Duration::from_millis(100), &mut third)
            .await
            .is_err(),
        "bounded publish should wait while a queued QoS publish is blocked"
    );

    broker.ack(first.pkid).await;
    third
        .await
        .expect("third publish task panicked")
        .expect("third publish failed");

    let second = broker
        .read_publish_with_timeout(PHASE_TIMEOUT)
        .await
        .expect("missing second publish after ack");
    assert_eq!(second.payload.as_ref(), b"second");

    drop(client);
    eventloop_task.abort();
    match eventloop_task.await {
        Ok(Ok(())) | Err(_) => {}
        Ok(Err(error)) => panic!("eventloop task should not fail: {error:?}"),
    }
}

#[tokio::test]
async fn control_request_bypasses_blocked_publish_without_ack_progress() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_inflight(1);

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(0).build();
    let eventloop_task = task::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(_) => {}
                Err(ConnectionError::RequestsDone) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    });

    let mut broker = TestBroker::from_listener(listener, 0, false).await;

    client
        .publish("test/topic", QoS::AtLeastOnce, false, "first")
        .await
        .unwrap();
    let first = broker
        .read_publish_with_timeout(PHASE_TIMEOUT)
        .await
        .expect("missing first publish");

    client
        .publish("test/topic", QoS::AtLeastOnce, false, "second")
        .await
        .unwrap();
    client
        .subscribe("control/topic", QoS::AtMostOnce)
        .await
        .unwrap();

    match time::timeout(PHASE_TIMEOUT, broker.tick())
        .await
        .expect("timed out waiting for control request behind blocked publish")
    {
        Event::Incoming(Packet::Subscribe(subscribe)) => {
            assert_eq!(subscribe.filters[0].path, "control/topic");
            assert_ne!(subscribe.pkid, first.pkid);
        }
        event => panic!("expected subscribe to bypass blocked publish, got {event:?}"),
    }

    drop(client);
    eventloop_task.abort();
    match eventloop_task.await {
        Ok(Ok(())) | Err(_) => {}
        Ok(Err(error)) => panic!("eventloop task should not fail: {error:?}"),
    }
}

#[tokio::test]
async fn tracked_unsubscribe_bypasses_blocked_publish_without_ack_progress() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_inflight(1);

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(0).build();
    let eventloop_task = task::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(_) => {}
                Err(ConnectionError::RequestsDone) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    });

    let mut broker = TestBroker::from_listener(listener, 0, false).await;

    client
        .publish("test/topic", QoS::AtLeastOnce, false, "first")
        .await
        .unwrap();
    let first = broker
        .read_publish_with_timeout(PHASE_TIMEOUT)
        .await
        .expect("missing first publish");

    client
        .publish("test/topic", QoS::AtLeastOnce, false, "second")
        .await
        .unwrap();
    let _notice = client
        .unsubscribe_tracked("control/topic")
        .await
        .expect("tracked unsubscribe should queue on control channel");

    match time::timeout(PHASE_TIMEOUT, broker.tick())
        .await
        .expect("timed out waiting for tracked unsubscribe behind blocked publish")
    {
        Event::Incoming(Packet::Unsubscribe(unsubscribe)) => {
            assert_eq!(unsubscribe.topics[0], "control/topic");
            assert_ne!(unsubscribe.pkid, first.pkid);
        }
        event => panic!("expected tracked unsubscribe to bypass blocked publish, got {event:?}"),
    }

    drop(client);
    eventloop_task.abort();
    match eventloop_task.await {
        Ok(Ok(())) | Err(_) => {}
        Ok(Err(error)) => panic!("eventloop task should not fail: {error:?}"),
    }
}

#[tokio::test]
async fn blocked_publish_uses_available_packet_id_after_out_of_order_acks() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_inflight(4);

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(5).build();
    let (out_of_order_acks_tx, out_of_order_acks_rx) = oneshot::channel::<()>();
    let (ack_first_tx, ack_first_rx) = oneshot::channel::<()>();
    let (stop_broker_tx, mut stop_broker_rx) = oneshot::channel::<()>();

    let requests = task::spawn(async move {
        start_requests(15, QoS::AtLeastOnce, 0, client).await;
    });

    let broker = task::spawn(async move {
        let mut broker = TestBroker::from_listener(listener, 0, false).await;

        // read all incoming packets first
        let packets = broker
            .wait_for_n_publishes(4, SETUP_TIMEOUT)
            .await
            .expect("didn't receive initial publishes");
        for (i, packet) in packets.iter().enumerate() {
            assert_eq!(
                packet.payload[0],
                u8::try_from(i + 1).expect("test payload index fits in u8")
            );
        }

        // out of order ack
        broker.ack_many(&[3, 4]).await;
        out_of_order_acks_tx
            .send(())
            .expect("eventloop task already exited");
        let packet = broker
            .read_publish_with_timeout(PHASE_TIMEOUT)
            .await
            .expect("missing publish after freeing packet id 3");
        assert_eq!(packet.pkid, 3);
        assert_eq!(packet.payload[0], 5);
        broker.ack(packet.pkid).await;
        ack_first_rx.await.expect("ack release signal dropped");

        loop {
            tokio::select! {
                _ = &mut stop_broker_rx => break,
                packet = broker.read_packet() => {
                    if let Some(Packet::Publish(publish)) = packet {
                        broker.ack(publish.pkid).await;
                    }
                }
            }
        }
    });

    // Sends 4 requests. The 5th request should use the first packet id made
    // available by the out-of-order acknowledgements instead of waiting for
    // the wrapped packet id 1.
    time::timeout(TEST_TIMEOUT, async {
        loop {
            let event = poll_ignoring_connect_races(&mut eventloop)
                .await
                .expect("poll should not fail");
            println!("Poll = {event:?}");
            if event == Event::Outgoing(Outgoing::Publish(4)) {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for initial publishes");

    out_of_order_acks_rx
        .await
        .expect("broker task already exited");

    let mut acked = 0;
    let mut reused_available_pkid = false;
    time::timeout(TEST_TIMEOUT, async {
        while acked < 2 || !reused_available_pkid {
            let event = poll_ignoring_connect_races(&mut eventloop)
                .await
                .expect("poll should not fail");
            println!("Poll = {event:?}");
            match event {
                Event::Incoming(Packet::PubAck(_)) => acked += 1,
                Event::Outgoing(Outgoing::Publish(pkid)) => {
                    assert_eq!(pkid, 3);
                    reused_available_pkid = true;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for out-of-order acknowledgements and packet id reuse");

    ack_first_tx.send(()).expect("broker task already exited");

    stop_broker_tx.send(()).expect("broker task already exited");
    broker.await.unwrap();
    requests.abort();
}

#[tokio::test]
async fn blocked_publish_does_not_trigger_collision_timeout_while_keepalive_progresses() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_inflight(4).set_keep_alive(1);

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
    let requests = task::spawn(async move {
        start_requests(6, QoS::AtLeastOnce, 0, client).await;
    });

    let broker = task::spawn(async move {
        let mut broker = TestBroker::from_listener(listener, 0, false).await;
        let packets = broker
            .wait_for_n_publishes(4, SETUP_TIMEOUT)
            .await
            .expect("didn't receive initial inflight publishes");

        for (i, packet) in packets.iter().enumerate() {
            assert_eq!(
                packet.payload[0],
                u8::try_from(i + 1).expect("test payload index fits in u8")
            );
        }

        // Keep the TCP session alive but don't ack any publishes. The queued
        // publish remains blocked while the inflight window is full.
        let mut pings = 0;
        time::timeout(TEST_TIMEOUT, async {
            while pings < 2 {
                match broker.read_packet().await {
                    Some(Packet::PingReq) => {
                        pings += 1;
                        broker.pingresp().await;
                    }
                    Some(Packet::Publish(publish)) => {
                        panic!("unexpected publish while packet ids 1 and 2 are still in use: {publish:?}")
                    }
                    Some(_) => {}
                    None => panic!("broker connection closed before keepalive progress"),
                }
            }
        })
        .await
        .expect("didn't observe keepalive pings");
    });

    let mut publishes = 0;
    let mut pings = 0;
    time::timeout(TEST_TIMEOUT, async {
        while pings < 2 {
            match poll_ignoring_connect_races(&mut eventloop).await {
                Ok(Event::Outgoing(Outgoing::Publish(pkid))) => {
                    publishes += 1;
                    assert!(
                        publishes <= 4,
                        "unexpected publish with packet id {pkid} while packet ids 1 and 2 are still in use"
                    );
                }
                Ok(Event::Outgoing(Outgoing::PingReq)) => pings += 1,
                Ok(event) => println!("Poll = {event:?}"),
                Err(ConnectionError::MqttState(StateError::CollisionTimeout)) => {
                    panic!("blocked publish should not enter collision timeout state")
                }
                Err(e) => panic!("poll failed while waiting for keepalive progress: {e:?}"),
            }
        }
    })
    .await
    .expect("timed out waiting for keepalive progress");

    broker.await.unwrap();
    requests.abort();
}

//
// All reconnection tests here
//
#[tokio::test]
async fn next_poll_after_connect_failure_reconnects() {
    let (listener, port) = reserve_listener().await;
    let options = MqttOptions::new("dummy", ("127.0.0.1", port));

    task::spawn(async move {
        let _broker = TestBroker::from_listener(listener, 1, false).await;
        let listener = listener_on(port).await;
        let _broker = TestBroker::from_listener(listener, 0, false).await;
        time::sleep(Duration::from_secs(15)).await;
    });

    time::sleep(Duration::from_secs(1)).await;
    let mut eventloop = EventLoop::new(options, 5);

    match eventloop.poll().await {
        Err(ConnectionError::ConnectionRefused(ConnectReturnCode::BadUserNamePassword)) => (),
        v => panic!("Expected bad username password error. Found = {v:?}"),
    }

    match eventloop.poll().await {
        Ok(Event::Incoming(Packet::ConnAck(ConnAck {
            code: ConnectReturnCode::Success,
            session_present: false,
        }))) => (),
        v => panic!("Expected ConnAck Success. Found = {v:?}"),
    }
}

#[tokio::test]
async fn reconnection_resumes_from_the_previous_state() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_keep_alive(5).set_clean_session(false);

    // start sending qos0 publishes. Makes sure that there is out activity but no in activity
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(5).build();
    task::spawn(async move {
        start_requests(10, QoS::AtLeastOnce, 1, client).await;
        time::sleep(Duration::from_secs(10)).await;
    });

    // start the eventloop
    task::spawn(async move {
        run(&mut eventloop, true).await.unwrap();
    });

    // broker connection 1
    let mut broker = TestBroker::from_listener(listener, 0, false).await;
    for i in 1..=2 {
        let packet = broker.read_publish().await.unwrap();
        assert_eq!(i, packet.payload[0]);
        broker.ack(packet.pkid).await;
    }

    // NOTE: An interesting thing to notice here is that reassigning a new broker
    // is behaving like a half-open connection instead of cleanly closing the socket
    // and returning error immediately
    // Manually dropping (`drop(broker.framed)`) the connection or adding
    // a block around broker with {} is closing the connection as expected

    // broker connection 2
    let listener = listener_on(port).await;
    let mut broker = TestBroker::from_listener(listener, 0, true).await;
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
    options.set_keep_alive(5).set_clean_session(false);

    // start sending qos0 publishes. this makes sure that there is
    // outgoing activity but no incoming activity
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(5).build();
    task::spawn(async move {
        start_requests(10, QoS::AtLeastOnce, 1, client).await;
        time::sleep(Duration::from_secs(10)).await;
    });

    // start the client eventloop
    task::spawn(async move {
        run(&mut eventloop, true).await.unwrap();
    });

    // broker connection 1. receive but don't ack
    let mut broker = TestBroker::from_listener(listener, 0, false).await;
    for i in 1..=2 {
        let packet = broker.read_publish().await.unwrap();
        assert_eq!(i, packet.payload[0]);
    }

    // broker connection 2 receives from scratch
    let listener = listener_on(port).await;
    let mut broker = TestBroker::from_listener(listener, 0, true).await;
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
        .set_clean_session(false)
        .set_inflight(4);

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
    task::spawn(async move {
        start_requests(8, QoS::AtLeastOnce, 0, client).await;
        time::sleep(Duration::from_secs(10)).await;
    });

    task::spawn(async move {
        run(&mut eventloop, true).await.unwrap();
    });

    {
        let mut broker = TestBroker::from_listener(listener, 0, false).await;
        let publishes = broker
            .wait_for_n_publishes(4, SETUP_TIMEOUT)
            .await
            .expect("didn't receive initial inflight publishes");
        for (i, publish) in publishes.iter().enumerate() {
            assert_eq!(
                publish.payload[0],
                u8::try_from(i + 1).expect("test payload index fits in u8")
            );
        }

        // Ack packet 2 while packet 1 is still unacked to force out-of-order ack boundary tracking.
        broker.ack(publishes[1].pkid).await;
    }

    let listener = listener_on(port).await;
    let mut broker = TestBroker::from_listener(listener, 0, true).await;
    let first_replayed = broker
        .read_publish_with_timeout(PHASE_TIMEOUT)
        .await
        .expect("missing first replayed publish after reconnect");
    assert!(first_replayed.dup, "replayed QoS1 publish must set DUP=1");
    assert_eq!(first_replayed.payload[0], 1);
    broker.ack(first_replayed.pkid).await;
}

#[tokio::test]
async fn graceful_disconnect_completes_qos2_handshakes_before_disconnect() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("issue-1031", ("127.0.0.1", port));
    options.set_keep_alive(60);

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(16).build();
    let eventloop_task = task::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(_) => {}
                Err(ConnectionError::RequestsDone) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    });

    let client_task = task::spawn(async move {
        for _ in 0..10 {
            client
                .publish("test/topic", QoS::ExactlyOnce, false, "Test payload.")
                .await
                .unwrap();
        }
        client.disconnect().await.unwrap();
    });

    let mut broker = TestBroker::from_listener(listener, 0, false).await;
    let mut pubrels = Vec::new();
    let mut disconnect_seen = false;

    while !disconnect_seen {
        let event = time::timeout(PHASE_TIMEOUT, broker.tick())
            .await
            .expect("timed out waiting for graceful disconnect packets");
        match event {
            Event::Incoming(Packet::Publish(publish)) => {
                assert!(
                    !disconnect_seen,
                    "PUBLISH observed after terminal DISCONNECT"
                );
                broker.pubrec(publish.pkid).await;
            }
            Event::Incoming(Packet::PubRel(pubrel)) => {
                assert!(
                    !disconnect_seen,
                    "PUBREL observed after terminal DISCONNECT"
                );
                pubrels.push(pubrel.pkid);
                broker.pubcomp(pubrel.pkid).await;
            }
            Event::Incoming(Packet::Disconnect) => {
                disconnect_seen = true;
            }
            event => panic!("unexpected broker event while draining QoS2: {event:?}"),
        }
    }

    assert_eq!(pubrels.len(), 10);
    assert_eq!(pubrels, (1..=10).collect::<Vec<_>>());

    client_task.await.unwrap();
    eventloop_task.await.unwrap().unwrap();
    assert!(
        broker
            .read_packet_with_timeout(Duration::from_millis(100))
            .await
            .is_none(),
        "DISCONNECT must be terminal"
    );
}

#[tokio::test]
async fn graceful_disconnect_timeout_does_not_send_disconnect() {
    let (listener, port) = reserve_listener().await;
    let options = MqttOptions::new("issue-1031-timeout", ("127.0.0.1", port));

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(4).build();
    let eventloop_task = task::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(_) => {}
                Err(error) => return error,
            }
        }
    });

    let client_task = task::spawn(async move {
        client
            .publish("test/topic", QoS::ExactlyOnce, false, "Test payload.")
            .await
            .unwrap();
        client
            .disconnect_with_timeout(Duration::from_millis(50))
            .await
            .unwrap();
    });

    let mut broker = TestBroker::from_listener(listener, 0, false).await;
    let publish = broker
        .read_publish_with_timeout(PHASE_TIMEOUT)
        .await
        .expect("expected QoS2 publish before disconnect timeout");
    assert_eq!(publish.qos, QoS::ExactlyOnce);

    client_task.await.unwrap();
    let error = eventloop_task.await.unwrap();
    assert!(matches!(error, ConnectionError::DisconnectTimeout));
    assert!(
        broker
            .read_packet_with_timeout(Duration::from_millis(100))
            .await
            .is_none(),
        "timed-out graceful disconnect must not send DISCONNECT"
    );
}

#[tokio::test]
async fn disconnect_now_sends_disconnect_without_waiting_for_qos2_completion() {
    let (listener, port) = reserve_listener().await;
    let options = MqttOptions::new("issue-1031-now", ("127.0.0.1", port));

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(4).build();
    let eventloop_task = task::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(_) => {}
                Err(ConnectionError::RequestsDone) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    });

    let client_task = task::spawn(async move {
        client
            .publish("test/topic", QoS::ExactlyOnce, false, "Test payload.")
            .await
            .unwrap();
        client.disconnect_now().await.unwrap();
    });

    let mut broker = TestBroker::from_listener(listener, 0, false).await;
    match time::timeout(PHASE_TIMEOUT, broker.tick())
        .await
        .expect("timed out waiting for immediate disconnect")
    {
        Event::Incoming(Packet::Disconnect) => {}
        Event::Incoming(Packet::Publish(publish)) => {
            assert_eq!(publish.qos, QoS::ExactlyOnce);
            match time::timeout(PHASE_TIMEOUT, broker.tick())
                .await
                .expect("timed out waiting for immediate disconnect after publish")
            {
                Event::Incoming(Packet::Disconnect) => {}
                event => panic!("expected immediate DISCONNECT after publish, got {event:?}"),
            }
        }
        event => panic!("expected immediate DISCONNECT, got {event:?}"),
    }

    client_task.await.unwrap();
    eventloop_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn disconnect_now_wakes_idle_eventloop() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("issue-1031-now-idle", ("127.0.0.1", port));
    options.set_keep_alive(60);

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(4).build();
    let eventloop_task = task::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(_) => {}
                Err(ConnectionError::RequestsDone) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    });

    let mut broker = TestBroker::from_listener(listener, 0, false).await;
    time::sleep(Duration::from_millis(100)).await;

    client.disconnect_now().await.unwrap();

    match time::timeout(PHASE_TIMEOUT, broker.tick())
        .await
        .expect("timed out waiting for idle immediate disconnect")
    {
        Event::Incoming(Packet::Disconnect) => {}
        event => panic!("expected immediate DISCONNECT from idle eventloop, got {event:?}"),
    }

    drop(client);
    eventloop_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn reconnection_clean_session_drops_pending_packets_after_reconnect() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options
        .set_inflight(2)
        .set_keep_alive(2)
        .set_clean_session(true);

    let broker = task::spawn(async move {
        let mut broker = TestBroker::from_listener(listener, 0, false).await;
        let publish = broker
            .read_publish_with_timeout(SETUP_TIMEOUT)
            .await
            .expect("missing publish before forced disconnect");
        assert_eq!(publish.pkid, 1);
        drop(broker);

        tokio::time::sleep(Duration::from_millis(200)).await;

        let listener = listener_on(port).await;
        let _broker = TestBroker::from_listener(listener, 0, false).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(5).build();
    let requests = task::spawn(async move {
        start_requests(3, QoS::AtLeastOnce, 0, client).await;
    });

    let mut saw_publish = false;
    let mut saw_disconnect = false;
    time::timeout(TEST_TIMEOUT, async {
        loop {
            match eventloop.poll().await {
                Ok(Event::Outgoing(Outgoing::Publish(_))) => saw_publish = true,
                Ok(Event::Incoming(Packet::ConnAck(ConnAck {
                    code: ConnectReturnCode::Success,
                    session_present: false,
                }))) if saw_disconnect => {
                    assert!(eventloop.pending_is_empty());
                    assert!(eventloop.state.collision.is_none());
                    break;
                }
                Ok(_) => {}
                Err(ConnectionError::RequestsDone) => panic!("eventloop ended before reconnect"),
                Err(_) if saw_publish => saw_disconnect = true,
                Err(error) => panic!("poll failed before first publish: {error:?}"),
            }
        }
    })
    .await
    .expect("timed out waiting for clean-session reconnect");

    requests.await.unwrap();
    broker.await.unwrap();
}

#[tokio::test]
async fn state_is_being_cleaned_properly_and_pending_request_calculated_properly() {
    let (listener, port) = reserve_listener().await;
    let mut options = MqttOptions::new("dummy", ("127.0.0.1", port));
    options.set_keep_alive(5);
    let mut network_options = NetworkOptions::new();
    network_options.set_tcp_send_buffer_size(1024);

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(5).build();
    eventloop.set_network_options(network_options);
    task::spawn(async move {
        start_requests_with_payload(100, QoS::AtLeastOnce, 0, client, 5000).await;
        time::sleep(Duration::from_secs(10)).await;
    });

    task::spawn(async move {
        let mut broker = TestBroker::from_listener(listener, 0, false).await;
        while (broker.read_packet().await).is_some() {
            time::sleep(Duration::from_secs_f64(0.5)).await;
        }
    });

    let handle = task::spawn(async move {
        let res = run(&mut eventloop, false).await;
        if let Err(e) = res {
            match e {
                ConnectionError::FlushTimeout => {
                    assert!(!eventloop.pending_is_empty());
                    assert!(eventloop.state.collision.is_none());
                    println!("State is being clean properly");
                }
                _ => {
                    println!(
                        "Couldn't fill the TCP send buffer to run this test properly. Try reducing the size of buffer."
                    );
                }
            }
        }
    });
    handle.await.unwrap();
}
