#![cfg(any(unix, windows))]

use std::env;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rumqttc_session_store_file::{v4, v5};

const CHILD_PROTOCOL: &str = "RUMQTTC_RESTART_TEST_PROTOCOL";
const CHILD_PHASE: &str = "RUMQTTC_RESTART_TEST_PHASE";
const CHILD_ROOT: &str = "RUMQTTC_RESTART_TEST_ROOT";
const CHILD_PORT: &str = "RUMQTTC_RESTART_TEST_PORT";
const CLIENT_ID: &str = "file-store-process-restart";
const SCOPE: &str = "process-restart-test";
const TOPIC: &str = "restart/qos1";
const PAYLOAD: &[u8] = b"survives-process-restart";
const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
enum Protocol {
    V4,
    V5,
}

impl Protocol {
    const fn name(self) -> &'static str {
        match self {
            Self::V4 => "v4",
            Self::V5 => "v5",
        }
    }

    const fn connack(self, session_present: bool) -> &'static [u8] {
        match (self, session_present) {
            (Self::V4, false) => &[0x20, 0x02, 0x00, 0x00],
            (Self::V4, true) => &[0x20, 0x02, 0x01, 0x00],
            (Self::V5, false) => &[0x20, 0x03, 0x00, 0x00, 0x00],
            (Self::V5, true) => &[0x20, 0x03, 0x01, 0x00, 0x00],
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Initial,
    Restore,
}

impl Phase {
    const fn name(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Restore => "restore",
        }
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn spawn(protocol: Protocol, phase: Phase, root: &Path, port: u16) -> Self {
        let child = Command::new(env::current_exe().expect("test executable path"))
            .args(["--exact", "restart_child_entrypoint", "--nocapture"])
            .env(CHILD_PROTOCOL, protocol.name())
            .env(CHILD_PHASE, phase.name())
            .env(CHILD_ROOT, root)
            .env(CHILD_PORT, port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn restart-test client process");
        Self(Some(child))
    }

    fn terminate(mut self) {
        let mut child = self.0.take().expect("child is present");
        child.kill().expect("terminate initial client process");
        let status = child.wait().expect("reap initial client process");
        assert!(
            !status.success(),
            "terminated client unexpectedly exited cleanly"
        );
    }

    fn wait_for_success(mut self) {
        let deadline = Instant::now() + IO_TIMEOUT;
        let mut child = self.0.take().expect("child is present");
        loop {
            if let Some(status) = child.try_wait().expect("query child status") {
                assert!(status.success(), "restored client failed with {status}");
                return;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("restored client did not finish within {IO_TIMEOUT:?}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn v4_file_backed_session_replays_after_process_restart() {
    exercise_process_restart(Protocol::V4);
}

#[test]
fn v5_file_backed_session_replays_after_process_restart() {
    exercise_process_restart(Protocol::V5);
}

fn exercise_process_restart(protocol: Protocol) {
    let root = tempfile::tempdir().expect("session-store root");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test broker");
    listener
        .set_nonblocking(true)
        .expect("configure nonblocking accept");
    let port = listener.local_addr().expect("broker address").port();

    let initial = ChildGuard::spawn(protocol, Phase::Initial, root.path(), port);
    let mut connection = accept_before(&listener, Instant::now() + IO_TIMEOUT);
    assert_eq!(
        read_frame(&mut connection).0 & 0xf0,
        0x10,
        "expected CONNECT"
    );
    connection
        .write_all(protocol.connack(false))
        .expect("send initial CONNACK");
    let initial_publish = read_publish(&mut connection, protocol);
    assert_eq!(initial_publish.packet_id, 1);
    assert!(!initial_publish.duplicate);
    assert_eq!(initial_publish.payload, PAYLOAD);
    initial.terminate();
    drop(connection);

    let restored = ChildGuard::spawn(protocol, Phase::Restore, root.path(), port);
    let mut connection = accept_before(&listener, Instant::now() + IO_TIMEOUT);
    assert_eq!(
        read_frame(&mut connection).0 & 0xf0,
        0x10,
        "expected CONNECT"
    );
    connection
        .write_all(protocol.connack(true))
        .expect("send resumed-session CONNACK");
    let replay = read_publish(&mut connection, protocol);
    assert_eq!(replay.packet_id, initial_publish.packet_id);
    assert!(replay.duplicate, "restored QoS 1 publish must set DUP=1");
    assert_eq!(replay.payload, PAYLOAD);
    restored.wait_for_success();
}

fn accept_before(listener: &TcpListener, deadline: Instant) -> TcpStream {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(IO_TIMEOUT))
                    .expect("set broker read timeout");
                stream
                    .set_write_timeout(Some(IO_TIMEOUT))
                    .expect("set broker write timeout");
                return stream;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "client did not connect in time");
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept test client: {error}"),
        }
    }
}

struct Publish {
    packet_id: u16,
    duplicate: bool,
    payload: Vec<u8>,
}

fn read_publish(stream: &mut TcpStream, protocol: Protocol) -> Publish {
    let (header, body) = read_frame(stream);
    assert_eq!(header & 0xf0, 0x30, "expected PUBLISH, got {header:#04x}");
    assert_eq!(header & 0x06, 0x02, "expected QoS 1 PUBLISH");

    let topic_length = usize::from(u16::from_be_bytes(
        body.get(..2)
            .expect("PUBLISH topic length")
            .try_into()
            .expect("two-byte topic length"),
    ));
    let topic_end = 2 + topic_length;
    assert_eq!(
        body.get(2..topic_end).expect("PUBLISH topic"),
        TOPIC.as_bytes()
    );
    let packet_id = u16::from_be_bytes(
        body.get(topic_end..topic_end + 2)
            .expect("PUBLISH packet identifier")
            .try_into()
            .expect("two-byte packet identifier"),
    );
    let mut payload_start = topic_end + 2;
    if matches!(protocol, Protocol::V5) {
        let (properties_length, encoded_length) = decode_variable_integer(&body[payload_start..]);
        payload_start += encoded_length + properties_length;
    }

    Publish {
        packet_id,
        duplicate: header & 0x08 != 0,
        payload: body.get(payload_start..).expect("PUBLISH payload").to_vec(),
    }
}

fn read_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut header = [0_u8; 1];
    stream.read_exact(&mut header).expect("read fixed header");

    let mut multiplier = 1_usize;
    let mut remaining = 0_usize;
    for _ in 0..4 {
        let mut encoded = [0_u8; 1];
        stream
            .read_exact(&mut encoded)
            .expect("read remaining length");
        remaining += usize::from(encoded[0] & 0x7f) * multiplier;
        if encoded[0] & 0x80 == 0 {
            let mut body = vec![0; remaining];
            stream.read_exact(&mut body).expect("read packet body");
            return (header[0], body);
        }
        multiplier *= 128;
    }
    panic!("malformed MQTT remaining length")
}

fn decode_variable_integer(bytes: &[u8]) -> (usize, usize) {
    let mut value = 0_usize;
    let mut multiplier = 1_usize;
    for (index, byte) in bytes.iter().copied().take(4).enumerate() {
        value += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            return (value, index + 1);
        }
        multiplier *= 128;
    }
    panic!("malformed MQTT variable byte integer")
}

#[test]
fn restart_child_entrypoint() {
    let Ok(protocol) = env::var(CHILD_PROTOCOL) else {
        return;
    };
    let protocol = match protocol.as_str() {
        "v4" => Protocol::V4,
        "v5" => Protocol::V5,
        other => panic!("unexpected child protocol {other}"),
    };
    let phase = match env::var(CHILD_PHASE).expect("child phase").as_str() {
        "initial" => Phase::Initial,
        "restore" => Phase::Restore,
        other => panic!("unexpected child phase {other}"),
    };
    let root = PathBuf::from(env::var_os(CHILD_ROOT).expect("child session-store root"));
    let port = env::var(CHILD_PORT)
        .expect("child broker port")
        .parse()
        .expect("numeric child broker port");

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child Tokio runtime")
        .block_on(run_client(protocol, phase, root, port));
}

async fn run_client(protocol: Protocol, phase: Phase, root: PathBuf, port: u16) {
    match protocol {
        Protocol::V4 => run_v4_client(phase, root, port).await,
        Protocol::V5 => run_v5_client(phase, root, port).await,
    }
}

async fn run_v4_client(phase: Phase, root: PathBuf, port: u16) {
    use rumqttc_v4::{AsyncClient, Event, MqttOptions, Outgoing, PublishOptions, QoS};

    let store = v4::SessionFileStore::open(root)
        .await
        .expect("open v4 store");
    let mut options = MqttOptions::new(CLIENT_ID, ("127.0.0.1", port));
    options
        .set_clean_session(false)
        .set_session_store_scope(SCOPE)
        .set_keep_alive(30)
        .set_session_store(store);
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(4).build();
    if matches!(phase, Phase::Initial) {
        client
            .publish(TOPIC, PAYLOAD, PublishOptions::new(QoS::AtLeastOnce))
            .await
            .expect("queue initial v4 publish");
    }

    loop {
        let event = eventloop.poll().await.expect("poll v4 client");
        if matches!(phase, Phase::Restore) && matches!(event, Event::Outgoing(Outgoing::Publish(1)))
        {
            return;
        }
    }
}

async fn run_v5_client(phase: Phase, root: PathBuf, port: u16) {
    use rumqttc_v5::mqttbytes::QoS;
    use rumqttc_v5::{AsyncClient, Event, MqttOptions, Outgoing, PublishOptions};

    let store = v5::SessionFileStore::open(root)
        .await
        .expect("open v5 store");
    let mut options = MqttOptions::new(CLIENT_ID, ("127.0.0.1", port));
    options
        .set_clean_start(false)
        .set_session_expiry_interval(Some(60))
        .set_session_store_scope(SCOPE)
        .set_keep_alive(30)
        .set_session_store(store);
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(4).build();
    if matches!(phase, Phase::Initial) {
        client
            .publish(TOPIC, PAYLOAD, PublishOptions::new(QoS::AtLeastOnce))
            .await
            .expect("queue initial v5 publish");
    }

    loop {
        let event = eventloop.poll().await.expect("poll v5 client");
        if matches!(phase, Phase::Restore) && matches!(event, Event::Outgoing(Outgoing::Publish(1)))
        {
            return;
        }
    }
}
