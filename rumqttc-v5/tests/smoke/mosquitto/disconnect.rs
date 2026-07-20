use rumqttc::mqttbytes::v5::{DisconnectProperties, DisconnectReasonCode, LastWill, Packet};
use rumqttc::{AsyncClient, ConnectionError, Event, MqttOptions, Outgoing, QoS};
use std::fs;
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};
use tokio::time;

const STARTUP_ATTEMPTS: usize = 50;
const STARTUP_RETRY: Duration = Duration::from_millis(50);
const OBSERVER_TIMEOUT: Duration = Duration::from_secs(3);
const PUBLISHER_TIMEOUT: Duration = Duration::from_secs(6);
const WILL_OBSERVATION_WINDOW: Duration = Duration::from_millis(750);

struct MosquittoBroker {
    child: Option<Child>,
    dir: PathBuf,
    port: u16,
}

impl MosquittoBroker {
    async fn start() -> Option<Self> {
        if !mosquitto_available() {
            eprintln!(
                "WARN: skipping Mosquitto disconnect smoke test: `mosquitto` is not available"
            );
            return None;
        }

        let port = reserve_port();
        let dir = smoke_dir();
        if let Err(err) = fs::create_dir_all(&dir) {
            eprintln!(
                "WARN: skipping Mosquitto disconnect smoke test: cannot create temp dir {dir:?}: {err}"
            );
            return None;
        }

        let config = dir.join("mosquitto.conf");
        if let Err(err) = write_config(&config, port) {
            eprintln!(
                "WARN: skipping Mosquitto disconnect smoke test: cannot write config {config:?}: {err}"
            );
            let _ = fs::remove_dir_all(&dir);
            return None;
        }

        let child = match Command::new("mosquitto")
            .arg("-c")
            .arg(&config)
            .arg("-v")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                eprintln!(
                    "WARN: skipping Mosquitto disconnect smoke test: cannot launch clean broker: {err}"
                );
                let _ = fs::remove_dir_all(&dir);
                return None;
            }
        };

        let mut broker = Self {
            child: Some(child),
            dir,
            port,
        };

        if wait_for_broker(port).await {
            Some(broker)
        } else {
            let logs = broker.stop();
            eprintln!(
                "WARN: skipping Mosquitto disconnect smoke test: clean broker did not accept connections:\n{logs}"
            );
            None
        }
    }

    fn stop(&mut self) -> String {
        let Some(mut child) = self.child.take() else {
            return String::new();
        };

        let _ = child.kill();
        let output = match child.wait_with_output() {
            Ok(output) => output,
            Err(err) => {
                let _ = fs::remove_dir_all(&self.dir);
                return format!("failed to collect mosquitto output: {err}");
            }
        };
        let _ = fs::remove_dir_all(&self.dir);

        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

impl Drop for MosquittoBroker {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn mosquitto_available() -> bool {
    Command::new("mosquitto")
        .arg("-h")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port allocation should work");
    listener
        .local_addr()
        .expect("ephemeral listener should have a local address")
        .port()
}

fn smoke_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "rumqttc-mosquitto-disconnect-smoke-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos()
    ))
}

fn write_config(config: &Path, port: u16) -> io::Result<()> {
    fs::write(
        config,
        format!(
            "\
listener {port} 127.0.0.1
allow_anonymous true
persistence false
log_type all
"
        ),
    )
}

async fn wait_for_broker(port: u16) -> bool {
    for _ in 0..STARTUP_ATTEMPTS {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return true;
        }
        time::sleep(STARTUP_RETRY).await;
    }

    false
}

async fn start_will_observer(
    port: u16,
    will_topic: String,
) -> (
    AsyncClient,
    oneshot::Receiver<()>,
    mpsc::UnboundedReceiver<String>,
    tokio::task::JoinHandle<Result<(), ConnectionError>>,
) {
    let options = MqttOptions::new(
        format!("rumqttc-smoke-{port}-observer"),
        ("127.0.0.1", port),
    );
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(16).build();
    let (suback_tx, suback_rx) = oneshot::channel();
    let (will_tx, will_rx) = mpsc::unbounded_channel();

    let task = tokio::spawn(async move {
        let mut suback_tx = Some(suback_tx);
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::SubAck(_))) => {
                    if let Some(tx) = suback_tx.take() {
                        let _ = tx.send(());
                    }
                }
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    if publish.topic.as_ref() == will_topic.as_bytes() {
                        let payload = String::from_utf8_lossy(&publish.payload).into_owned();
                        let _ = will_tx.send(payload);
                    }
                }
                Ok(_) => {}
                Err(ConnectionError::RequestsDone) => return Ok(()),
                Err(err) => return Err(err),
            }
        }
    });

    (client, suback_rx, will_rx, task)
}

async fn run_publisher_eventloop_until_disconnect(
    mut eventloop: rumqttc::EventLoop,
) -> Result<bool, ConnectionError> {
    let mut saw_disconnect = false;
    loop {
        match eventloop.poll().await {
            Ok(Event::Outgoing(Outgoing::Disconnect)) => saw_disconnect = true,
            Ok(_) => {}
            Err(ConnectionError::RequestsDone) => return Ok(saw_disconnect),
            Err(err) => return Err(err),
        }
    }
}

async fn run_subscribed_publisher_eventloop_until_disconnect(
    mut eventloop: rumqttc::EventLoop,
    echo_topic: String,
    suback_tx: oneshot::Sender<()>,
    echo_tx: mpsc::UnboundedSender<String>,
) -> Result<bool, ConnectionError> {
    let mut saw_disconnect = false;
    let mut suback_tx = Some(suback_tx);

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::SubAck(_))) => {
                if let Some(tx) = suback_tx.take() {
                    let _ = tx.send(());
                }
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                if publish.topic.as_ref() == echo_topic.as_bytes() {
                    let payload = String::from_utf8_lossy(&publish.payload).into_owned();
                    drop(echo_tx.send(payload));
                }
            }
            Ok(Event::Outgoing(Outgoing::Disconnect)) => saw_disconnect = true,
            Ok(_) => {}
            Err(ConnectionError::RequestsDone) => return Ok(saw_disconnect),
            Err(err) => return Err(err),
        }
    }
}

async fn assert_no_will_received(will_rx: &mut mpsc::UnboundedReceiver<String>) {
    assert!(
        time::timeout(WILL_OBSERVATION_WINDOW, will_rx.recv())
            .await
            .is_err(),
        "observer received publisher Last Will after graceful disconnect"
    );
}

async fn stop_observer(
    observer: AsyncClient,
    observer_task: tokio::task::JoinHandle<Result<(), ConnectionError>>,
) {
    observer.disconnect_now().await.unwrap();
    let _ = time::timeout(OBSERVER_TIMEOUT, observer_task).await;
}

#[derive(Clone, Copy)]
enum SubscriptionDisconnectMode {
    KeepSubscription,
    PlainUnsubscribe,
    TrackedUnsubscribe,
}

impl SubscriptionDisconnectMode {
    const fn suffix(self) -> &'static str {
        match self {
            Self::KeepSubscription => "subscribed",
            Self::PlainUnsubscribe => "plain-unsubscribe",
            Self::TrackedUnsubscribe => "tracked-unsubscribe",
        }
    }
}

async fn assert_subscribed_publisher_graceful_disconnect_suppresses_will(
    mode: SubscriptionDisconnectMode,
) {
    let Some(mut broker) = MosquittoBroker::start().await else {
        return;
    };
    let port = broker.port;
    let suffix = mode.suffix();
    let client_id = format!("rumqttc-smoke-{port}-{suffix}-publisher");
    let echo_topic = format!("rumqttc/smoke/{port}/{suffix}/echo");
    let will_topic = format!("rumqttc/smoke/{port}/{suffix}/will");

    let (observer, suback_rx, mut will_rx, observer_task) =
        start_will_observer(port, will_topic.clone()).await;
    observer
        .subscribe(will_topic.clone(), QoS::AtLeastOnce)
        .await
        .unwrap();
    time::timeout(OBSERVER_TIMEOUT, suback_rx)
        .await
        .unwrap()
        .unwrap();

    let will = LastWill::new(will_topic, "will-published", QoS::AtLeastOnce, false, None);
    let mut options = MqttOptions::new(client_id.clone(), ("127.0.0.1", port));
    options.set_keep_alive(5).set_last_will(will);

    let (client, eventloop) = AsyncClient::builder(options).capacity(16).build();
    let (publisher_suback_tx, publisher_suback_rx) = oneshot::channel();
    let (echo_tx, mut echo_rx) = mpsc::unbounded_channel();
    let publisher_task = tokio::spawn(run_subscribed_publisher_eventloop_until_disconnect(
        eventloop,
        echo_topic.clone(),
        publisher_suback_tx,
        echo_tx,
    ));

    client
        .subscribe(echo_topic.clone(), QoS::ExactlyOnce)
        .await
        .unwrap();
    time::timeout(OBSERVER_TIMEOUT, publisher_suback_rx)
        .await
        .unwrap()
        .unwrap();
    client
        .publish(echo_topic.clone(), QoS::ExactlyOnce, false, "echo")
        .await
        .unwrap();
    let echo = time::timeout(PUBLISHER_TIMEOUT, echo_rx.recv())
        .await
        .expect("timed out waiting for broker echo to subscribed publisher")
        .expect("publisher echo channel closed");
    assert_eq!(echo, "echo");

    match mode {
        SubscriptionDisconnectMode::KeepSubscription => {}
        SubscriptionDisconnectMode::PlainUnsubscribe => {
            client.unsubscribe(echo_topic.clone()).await.unwrap();
        }
        SubscriptionDisconnectMode::TrackedUnsubscribe => {
            let notice = client
                .unsubscribe_tracked(echo_topic.clone())
                .await
                .unwrap();
            notice.wait_completion_async().await.unwrap();
        }
    }

    let (reason, properties) = normal_disconnect_properties();
    client
        .disconnect_with_properties_timeout(reason, properties, Duration::from_secs(2))
        .await
        .unwrap();
    let saw_disconnect = time::timeout(PUBLISHER_TIMEOUT, publisher_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        saw_disconnect,
        "publisher eventloop did not emit Outgoing(Disconnect)"
    );

    assert_no_will_received(&mut will_rx).await;
    stop_observer(observer, observer_task).await;
    let logs = broker.stop();
    assert!(
        logs.contains(&format!("Received DISCONNECT from {client_id}")),
        "mosquitto did not log subscribed publisher DISCONNECT:\n{logs}"
    );
}

#[tokio::test]
async fn subscribed_publisher_graceful_disconnect_suppresses_will() {
    assert_subscribed_publisher_graceful_disconnect_suppresses_will(
        SubscriptionDisconnectMode::KeepSubscription,
    )
    .await;
}

#[tokio::test]
async fn subscribed_publisher_plain_unsubscribe_then_graceful_disconnect_suppresses_will() {
    assert_subscribed_publisher_graceful_disconnect_suppresses_will(
        SubscriptionDisconnectMode::PlainUnsubscribe,
    )
    .await;
}

#[tokio::test]
async fn subscribed_publisher_tracked_unsubscribe_then_graceful_disconnect_suppresses_will() {
    assert_subscribed_publisher_graceful_disconnect_suppresses_will(
        SubscriptionDisconnectMode::TrackedUnsubscribe,
    )
    .await;
}

#[tokio::test]
async fn mixed_qos_graceful_disconnect_suppresses_will() {
    let Some(mut broker) = MosquittoBroker::start().await else {
        return;
    };
    let port = broker.port;
    let will_topic = format!("rumqttc/smoke/{port}/will");

    let (observer, suback_rx, mut will_rx, observer_task) =
        start_will_observer(port, will_topic.clone()).await;
    observer
        .subscribe(will_topic.clone(), QoS::AtLeastOnce)
        .await
        .unwrap();
    time::timeout(OBSERVER_TIMEOUT, suback_rx)
        .await
        .unwrap()
        .unwrap();

    let will = LastWill::new(will_topic, "will-published", QoS::AtLeastOnce, false, None);
    let mut options = MqttOptions::new(
        format!("rumqttc-smoke-{port}-publisher"),
        ("127.0.0.1", port),
    );
    options.set_keep_alive(5).set_last_will(will);

    let (client, eventloop) = AsyncClient::builder(options).capacity(16).build();
    let publisher_task = tokio::spawn(run_publisher_eventloop_until_disconnect(eventloop));

    client
        .publish(
            "rumqttc/smoke/qos2/one",
            QoS::ExactlyOnce,
            false,
            "qos2-one",
        )
        .await
        .unwrap();
    client
        .publish(
            "rumqttc/smoke/qos1/two",
            QoS::AtLeastOnce,
            false,
            "qos1-two",
        )
        .await
        .unwrap();
    client
        .publish(
            "rumqttc/smoke/qos1/three",
            QoS::AtLeastOnce,
            false,
            "qos1-three",
        )
        .await
        .unwrap();
    time::sleep(Duration::from_millis(200)).await;
    client
        .publish(
            "rumqttc/smoke/qos2/four",
            QoS::ExactlyOnce,
            false,
            "qos2-four",
        )
        .await
        .unwrap();
    let (reason, properties) = normal_disconnect_properties();
    client
        .disconnect_with_properties_timeout(reason, properties, Duration::from_secs(2))
        .await
        .unwrap();

    let saw_disconnect = time::timeout(PUBLISHER_TIMEOUT, publisher_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        saw_disconnect,
        "publisher eventloop did not emit Outgoing(Disconnect)"
    );

    assert_no_will_received(&mut will_rx).await;
    stop_observer(observer, observer_task).await;

    let logs = broker.stop();
    assert!(
        logs.contains(&format!(
            "Received DISCONNECT from rumqttc-smoke-{port}-publisher"
        )),
        "mosquitto did not log publisher DISCONNECT:\n{logs}"
    );
}

#[tokio::test]
async fn graceful_disconnect_with_unsent_backlog_suppresses_will() {
    let Some(mut broker) = MosquittoBroker::start().await else {
        return;
    };
    let port = broker.port;
    let will_topic = format!("rumqttc/smoke/{port}/backlog/will");

    let (observer, suback_rx, mut will_rx, observer_task) =
        start_will_observer(port, will_topic.clone()).await;
    observer
        .subscribe(will_topic.clone(), QoS::AtLeastOnce)
        .await
        .unwrap();
    time::timeout(OBSERVER_TIMEOUT, suback_rx)
        .await
        .unwrap()
        .unwrap();

    let will = LastWill::new(will_topic, "will-published", QoS::AtLeastOnce, false, None);
    let mut options = MqttOptions::new(
        format!("rumqttc-smoke-{port}-backlog-publisher"),
        ("127.0.0.1", port),
    );
    options
        .set_keep_alive(5)
        .set_max_request_batch(1)
        .set_outgoing_inflight_upper_limit(1)
        .set_last_will(will);

    let (client, eventloop) = AsyncClient::builder(options).capacity(16).build();
    let publisher_task = tokio::spawn(run_publisher_eventloop_until_disconnect(eventloop));

    client
        .publish(
            "rumqttc/smoke/backlog/first",
            QoS::AtLeastOnce,
            false,
            "first",
        )
        .await
        .unwrap();
    client
        .publish(
            "rumqttc/smoke/backlog/unsent",
            QoS::AtLeastOnce,
            false,
            "unsent",
        )
        .await
        .unwrap();
    let (reason, properties) = normal_disconnect_properties();
    client
        .disconnect_with_properties_timeout(reason, properties, Duration::from_secs(2))
        .await
        .unwrap();

    let saw_disconnect = time::timeout(PUBLISHER_TIMEOUT, publisher_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        saw_disconnect,
        "publisher eventloop did not emit Outgoing(Disconnect)"
    );

    assert_no_will_received(&mut will_rx).await;
    stop_observer(observer, observer_task).await;

    let logs = broker.stop();
    assert!(
        logs.contains(&format!(
            "Received DISCONNECT from rumqttc-smoke-{port}-backlog-publisher"
        )),
        "mosquitto did not log backlog publisher DISCONNECT:\n{logs}"
    );
}

fn normal_disconnect_properties() -> (DisconnectReasonCode, DisconnectProperties) {
    (
        DisconnectReasonCode::NormalDisconnection,
        DisconnectProperties {
            session_expiry_interval: Some(0),
            reason_string: None,
            user_properties: Vec::new(),
            server_reference: None,
        },
    )
}
