#![cfg(any(unix, windows))]

use std::env;
use std::future::Future;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rumqttc_session_store_file::{v4, v5};

const CHILD_PROTOCOL: &str = "RUMQTTC_RESTART_TEST_PROTOCOL";
const CHILD_PHASE: &str = "RUMQTTC_RESTART_TEST_PHASE";
const CHILD_ROOT: &str = "RUMQTTC_RESTART_TEST_ROOT";
const CHILD_PORT: &str = "RUMQTTC_RESTART_TEST_PORT";
const CHILD_SCENARIO: &str = "RUMQTTC_RESTART_TEST_SCENARIO";
const CHILD_FAULT: &str = "RUMQTTC_RESTART_TEST_FAULT";
const CLIENT_ID: &str = "file-store-process-restart";
const CHANGED_CLIENT_ID: &str = "file-store-process-restart-changed";
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
    Seed,
    Initial,
    Restore,
}

impl Phase {
    const fn name(self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::Initial => "initial",
            Self::Restore => "restore",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum V5Scenario {
    Qos1Publish,
    Qos2Publish,
    PubRel,
    IncomingQos2,
    Subscribe,
    Unsubscribe,
    MixedReplay,
    MissingBrokerSession,
    ChangedClientId,
    ConnackZeroAbrupt,
    DisconnectZeroGraceful,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoreFault {
    Load,
    Save,
    Clear,
}

impl StoreFault {
    const fn name(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::Save => "save",
            Self::Clear => "clear",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "load" => Self::Load,
            "save" => Self::Save,
            "clear" => Self::Clear,
            other => panic!("unexpected restart store fault {other}"),
        }
    }
}

impl V5Scenario {
    const REPLAY_CASES: [Self; 7] = [
        Self::Qos1Publish,
        Self::Qos2Publish,
        Self::PubRel,
        Self::IncomingQos2,
        Self::Subscribe,
        Self::Unsubscribe,
        Self::MixedReplay,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Qos1Publish => "qos1-publish",
            Self::Qos2Publish => "qos2-publish",
            Self::PubRel => "pubrel",
            Self::IncomingQos2 => "incoming-qos2",
            Self::Subscribe => "subscribe",
            Self::Unsubscribe => "unsubscribe",
            Self::MixedReplay => "mixed-replay",
            Self::MissingBrokerSession => "missing-broker-session",
            Self::ChangedClientId => "changed-client-id",
            Self::ConnackZeroAbrupt => "connack-zero-abrupt",
            Self::DisconnectZeroGraceful => "disconnect-zero-graceful",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "qos1-publish" => Self::Qos1Publish,
            "qos2-publish" => Self::Qos2Publish,
            "pubrel" => Self::PubRel,
            "incoming-qos2" => Self::IncomingQos2,
            "subscribe" => Self::Subscribe,
            "unsubscribe" => Self::Unsubscribe,
            "mixed-replay" => Self::MixedReplay,
            "missing-broker-session" => Self::MissingBrokerSession,
            "changed-client-id" => Self::ChangedClientId,
            "connack-zero-abrupt" => Self::ConnackZeroAbrupt,
            "disconnect-zero-graceful" => Self::DisconnectZeroGraceful,
            other => panic!("unexpected MQTT 5 restart scenario {other}"),
        }
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn spawn(
        protocol: Protocol,
        phase: Phase,
        root: &Path,
        port: u16,
        scenario: Option<V5Scenario>,
    ) -> Self {
        Self::spawn_with_fault(protocol, phase, root, port, scenario, None)
    }

    fn spawn_with_fault(
        protocol: Protocol,
        phase: Phase,
        root: &Path,
        port: u16,
        scenario: Option<V5Scenario>,
        fault: Option<StoreFault>,
    ) -> Self {
        let mut command = Command::new(env::current_exe().expect("test executable path"));
        command
            .args(["--exact", "restart_child_entrypoint", "--nocapture"])
            .env(CHILD_PROTOCOL, protocol.name())
            .env(CHILD_PHASE, phase.name())
            .env(CHILD_ROOT, root)
            .env(CHILD_PORT, port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if let Some(scenario) = scenario {
            command.env(CHILD_SCENARIO, scenario.name());
        }
        if let Some(fault) = fault {
            command.env(CHILD_FAULT, fault.name());
        }
        let child = command.spawn().expect("spawn restart-test client process");
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

#[derive(Clone, Debug)]
struct FaultInjectingV5Store {
    inner: v5::SessionFileStore,
    fault: StoreFault,
}

impl FaultInjectingV5Store {
    fn error(operation: StoreFault) -> rumqttc_v5::SessionStoreError {
        io::Error::other(format!("injected file-store {operation:?} failure")).into()
    }
}

impl rumqttc_v5::SessionStore for FaultInjectingV5Store {
    fn load<'a>(
        &'a self,
        key: &'a rumqttc_v5::SessionStoreKey,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        Option<rumqttc_v5::PersistedSession>,
                        rumqttc_v5::SessionStoreError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if self.fault == StoreFault::Load {
                Err(Self::error(self.fault))
            } else {
                rumqttc_v5::SessionStore::load(&self.inner, key).await
            }
        })
    }

    fn save<'a>(
        &'a self,
        key: &'a rumqttc_v5::SessionStoreKey,
        session: &'a rumqttc_v5::PersistedSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), rumqttc_v5::SessionStoreError>> + Send + 'a>> {
        Box::pin(async move {
            if self.fault == StoreFault::Save {
                Err(Self::error(self.fault))
            } else {
                rumqttc_v5::SessionStore::save(&self.inner, key, session).await
            }
        })
    }

    fn clear<'a>(
        &'a self,
        key: &'a rumqttc_v5::SessionStoreKey,
    ) -> Pin<Box<dyn Future<Output = Result<(), rumqttc_v5::SessionStoreError>> + Send + 'a>> {
        Box::pin(async move {
            if self.fault == StoreFault::Clear {
                Err(Self::error(self.fault))
            } else {
                rumqttc_v5::SessionStore::clear(&self.inner, key).await
            }
        })
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

#[test]
fn v5_file_backed_recovery_state_matrix_survives_process_restart() {
    for scenario in V5Scenario::REPLAY_CASES {
        exercise_v5_seeded_restart(scenario, true);
    }
}

#[test]
fn v5_missing_broker_session_discards_checkpoint_before_fresh_work() {
    exercise_v5_seeded_restart(V5Scenario::MissingBrokerSession, false);
}

#[test]
fn v5_changed_client_identifier_does_not_restore_the_old_checkpoint() {
    exercise_v5_seeded_restart(V5Scenario::ChangedClientId, false);
}

#[test]
fn v5_connack_zero_expiry_abrupt_restart_never_replays_the_checkpoint() {
    exercise_v5_zero_expiry_restart(V5Scenario::ConnackZeroAbrupt);
}

#[test]
fn v5_disconnect_zero_expiry_gracefully_clears_before_restart() {
    exercise_v5_zero_expiry_restart(V5Scenario::DisconnectZeroGraceful);
}

#[test]
fn v5_file_store_load_save_and_clear_faults_fail_closed_across_processes() {
    for fault in [StoreFault::Load, StoreFault::Save, StoreFault::Clear] {
        exercise_v5_file_store_fault(fault);
    }
}

fn exercise_v5_file_store_fault(fault: StoreFault) {
    let root = tempfile::tempdir().expect("session-store root");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test broker");
    listener
        .set_nonblocking(true)
        .expect("configure nonblocking accept");
    let port = listener.local_addr().expect("broker address").port();

    if fault != StoreFault::Save {
        ChildGuard::spawn(
            Protocol::V5,
            Phase::Seed,
            root.path(),
            port,
            Some(V5Scenario::Qos1Publish),
        )
        .wait_for_success();
    }

    let phase = if fault == StoreFault::Save {
        Phase::Initial
    } else {
        Phase::Restore
    };
    let child = ChildGuard::spawn_with_fault(
        Protocol::V5,
        phase,
        root.path(),
        port,
        Some(V5Scenario::Qos1Publish),
        Some(fault),
    );

    if fault == StoreFault::Load {
        child.wait_for_success();
        assert_matches_would_block(listener.accept(), "load failure must precede CONNECT");
    } else {
        let mut connection = accept_before(&listener, Instant::now() + IO_TIMEOUT);
        assert_eq!(read_frame(&mut connection).0 & 0xf0, 0x10);
        connection
            .write_all(Protocol::V5.connack(false))
            .expect("send fault-test CONNACK");
        child.wait_for_success();
        let mut byte = [0_u8; 1];
        match connection.read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                ) => {}
            result => {
                panic!("{fault:?}: stale packet escaped the failed durability barrier: {result:?}")
            }
        }
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fault checkpoint inspection runtime");
    runtime.block_on(async {
        use rumqttc_v5::{SessionStore, SessionStoreKey};
        let store = v5::SessionFileStore::open(root.path()).await.unwrap();
        let checkpoint = store
            .load(&SessionStoreKey::new(SCOPE, CLIENT_ID))
            .await
            .unwrap();
        match fault {
            StoreFault::Load | StoreFault::Clear => {
                assert!(matches!(
                    checkpoint.as_ref().map(|checkpoint| checkpoint.replay.as_slice()),
                    Some([rumqttc_v5::PersistedRequest::Publish(publish)]) if publish.pkid == 7
                ));
            }
            StoreFault::Save => assert!(checkpoint.is_none()),
        }
    });
}

fn assert_matches_would_block(
    result: io::Result<(TcpStream, std::net::SocketAddr)>,
    message: &str,
) {
    assert!(
        matches!(result, Err(error) if error.kind() == io::ErrorKind::WouldBlock),
        "{message}"
    );
}

fn exercise_v5_zero_expiry_restart(scenario: V5Scenario) {
    let root = tempfile::tempdir().expect("session-store root");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test broker");
    listener
        .set_nonblocking(true)
        .expect("configure nonblocking accept");
    let port = listener.local_addr().expect("broker address").port();

    let initial = ChildGuard::spawn(
        Protocol::V5,
        Phase::Initial,
        root.path(),
        port,
        Some(scenario),
    );
    let mut connection = accept_before(&listener, Instant::now() + IO_TIMEOUT);
    assert_eq!(read_frame(&mut connection).0 & 0xf0, 0x10);
    let connack = if scenario == V5Scenario::ConnackZeroAbrupt {
        v5_connack_with_expiry(false, 0)
    } else {
        Protocol::V5.connack(false).to_vec()
    };
    connection
        .write_all(&connack)
        .expect("send initial CONNACK");
    let publish = read_publish(&mut connection, Protocol::V5);
    assert_eq!(publish.packet_id, 1);
    assert!(!publish.duplicate);

    match scenario {
        V5Scenario::ConnackZeroAbrupt => initial.terminate(),
        V5Scenario::DisconnectZeroGraceful => {
            connection
                .write_all(&[0x40, 0x04, 0x00, 0x01, 0x00, 0x00])
                .expect("ack publish before graceful disconnect");
            let (header, _) = read_frame(&mut connection);
            assert_eq!(header & 0xf0, 0xe0, "expected graceful DISCONNECT");
            initial.wait_for_success();
        }
        _ => unreachable!(),
    }
    drop(connection);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("checkpoint inspection runtime");
    runtime.block_on(async {
        use rumqttc_v5::{SessionStore, SessionStoreKey};
        let store = v5::SessionFileStore::open(root.path()).await.unwrap();
        let checkpoint = store
            .load(&SessionStoreKey::new(SCOPE, CLIENT_ID))
            .await
            .unwrap();
        match scenario {
            V5Scenario::ConnackZeroAbrupt => assert_eq!(
                checkpoint
                    .expect("crash can leave the last durable admission checkpoint")
                    .session_expiry_interval,
                Some(0)
            ),
            V5Scenario::DisconnectZeroGraceful => {
                assert!(
                    checkpoint.is_none(),
                    "graceful zero expiry must clear the store"
                );
            }
            _ => unreachable!(),
        }
    });

    let restored = ChildGuard::spawn(
        Protocol::V5,
        Phase::Restore,
        root.path(),
        port,
        Some(scenario),
    );
    let mut connection = accept_before(&listener, Instant::now() + IO_TIMEOUT);
    assert_eq!(read_frame(&mut connection).0 & 0xf0, 0x10);
    connection
        .write_all(Protocol::V5.connack(false))
        .expect("send fresh-session CONNACK after zero expiry");
    let fresh = read_publish(&mut connection, Protocol::V5);
    assert_eq!(fresh.packet_id, 1);
    assert!(
        !fresh.duplicate,
        "zero-expiry restart must admit only fresh work"
    );
    assert_no_trailing_v5_packet_after_fresh_ack(&mut connection, fresh.packet_id);
    drop(connection);
    restored.wait_for_success();
}

fn v5_connack_with_expiry(session_present: bool, expiry: u32) -> Vec<u8> {
    let mut bytes = vec![0x20, 0x08, u8::from(session_present), 0x00, 0x05, 0x11];
    bytes.extend_from_slice(&expiry.to_be_bytes());
    bytes
}

fn exercise_v5_seeded_restart(scenario: V5Scenario, session_present: bool) {
    let root = tempfile::tempdir().expect("session-store root");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test broker");
    listener
        .set_nonblocking(true)
        .expect("configure nonblocking accept");
    let port = listener.local_addr().expect("broker address").port();

    ChildGuard::spawn(Protocol::V5, Phase::Seed, root.path(), port, Some(scenario))
        .wait_for_success();

    let restored = ChildGuard::spawn(
        Protocol::V5,
        Phase::Restore,
        root.path(),
        port,
        Some(scenario),
    );
    let mut connection = accept_before(&listener, Instant::now() + IO_TIMEOUT);
    assert_eq!(
        read_frame(&mut connection).0 & 0xf0,
        0x10,
        "{scenario:?}: expected CONNECT"
    );
    connection
        .write_all(Protocol::V5.connack(session_present))
        .expect("send restart-matrix CONNACK");

    if session_present {
        assert_v5_recovery_frames(&mut connection, scenario);
    } else {
        let publish = read_publish(&mut connection, Protocol::V5);
        assert_eq!(publish.packet_id, 1, "{scenario:?}");
        assert!(
            !publish.duplicate,
            "{scenario:?}: fresh work must use DUP=0"
        );
        assert_eq!(publish.qos, 1, "{scenario:?}");
        assert_no_trailing_v5_packet_after_fresh_ack(&mut connection, publish.packet_id);
    }

    drop(connection);
    restored.wait_for_success();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("checkpoint inspection runtime");
    runtime.block_on(async {
        use rumqttc_v5::{SessionStore, SessionStoreKey};

        let store = v5::SessionFileStore::open(root.path())
            .await
            .expect("open store for inspection");
        let old = store
            .load(&SessionStoreKey::new(SCOPE, CLIENT_ID))
            .await
            .expect("load old restart checkpoint");
        match scenario {
            V5Scenario::MissingBrokerSession => {
                let checkpoint = old.expect("fresh work must create a replacement checkpoint");
                assert!(
                    checkpoint.replay.is_empty(),
                    "acknowledged fresh work must not leave old or fresh replay entries"
                );
                assert_eq!(checkpoint.client_id, CLIENT_ID);
            }
            V5Scenario::ChangedClientId => {
                assert!(
                    old.is_some(),
                    "changed identity must retain the old key untouched"
                );
                assert!(
                    store
                        .load(&SessionStoreKey::new(SCOPE, CHANGED_CLIENT_ID))
                        .await
                        .expect("load changed-client checkpoint")
                        .is_some(),
                    "fresh changed-client work must be checkpointed under its own key"
                );
            }
            _ => {
                let checkpoint = old.expect("resumed checkpoint must remain available");
                assert_eq!(checkpoint.client_id, CLIENT_ID);
                assert_eq!(checkpoint.session_expiry_interval, Some(60));
            }
        }
    });
}

fn assert_v5_recovery_frames(stream: &mut TcpStream, scenario: V5Scenario) {
    match scenario {
        V5Scenario::Qos1Publish => assert_replayed_publish(stream, 7, 1),
        V5Scenario::Qos2Publish => assert_replayed_publish(stream, 8, 2),
        V5Scenario::PubRel => assert_packet_identifier_frame(stream, 0x62, 9),
        V5Scenario::IncomingQos2 => {
            stream
                .write_all(&[0x62, 0x04, 0x00, 0x0a, 0x00, 0x00])
                .expect("send PUBREL for restored incoming QoS 2 state");
            assert_packet_identifier_frame(stream, 0x70, 10);
        }
        V5Scenario::Subscribe => assert_packet_identifier_frame(stream, 0x82, 11),
        V5Scenario::Unsubscribe => assert_packet_identifier_frame(stream, 0xa2, 12),
        V5Scenario::MixedReplay => {
            assert_replayed_publish(stream, 7, 1);
            assert_packet_identifier_frame(stream, 0x62, 9);
            assert_packet_identifier_frame(stream, 0x82, 11);
            assert_packet_identifier_frame(stream, 0xa2, 12);
        }
        V5Scenario::MissingBrokerSession | V5Scenario::ChangedClientId => {
            panic!("fresh-session scenario cannot assert resumed frames")
        }
        V5Scenario::ConnackZeroAbrupt | V5Scenario::DisconnectZeroGraceful => {
            panic!("zero-expiry scenario cannot assert resumed frames")
        }
    }
}

fn assert_replayed_publish(stream: &mut TcpStream, packet_id: u16, qos: u8) {
    let publish = read_publish(stream, Protocol::V5);
    assert_eq!(publish.packet_id, packet_id);
    assert_eq!(publish.qos, qos);
    assert!(publish.duplicate, "restored PUBLISH must set DUP=1");
    assert_eq!(publish.payload, PAYLOAD);
}

fn assert_packet_identifier_frame(stream: &mut TcpStream, header: u8, packet_id: u16) {
    let (actual_header, body) = read_frame(stream);
    assert_eq!(actual_header, header);
    assert_eq!(
        u16::from_be_bytes(body[..2].try_into().expect("packet identifier bytes")),
        packet_id
    );
}

fn assert_no_trailing_v5_packet_after_fresh_ack(stream: &mut TcpStream, packet_id: u16) {
    let [high, low] = packet_id.to_be_bytes();
    stream
        .write_all(&[0x40, 0x04, high, low, 0x00, 0x00])
        .expect("ack fresh QoS 1 publish");
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .expect("set stale-replay observation timeout");

    let mut header = [0_u8; 1];
    match stream.read(&mut header) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) => {}
        Ok(0) => panic!("restart client closed before stale-replay absence was observed"),
        Ok(_) => panic!(
            "unexpected trailing MQTT packet after fresh work: fixed header {:#04x}",
            header[0]
        ),
        Err(error) => panic!("observe trailing stale replay: {error}"),
    }
}

fn exercise_process_restart(protocol: Protocol) {
    let root = tempfile::tempdir().expect("session-store root");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test broker");
    listener
        .set_nonblocking(true)
        .expect("configure nonblocking accept");
    let port = listener.local_addr().expect("broker address").port();

    let initial = ChildGuard::spawn(protocol, Phase::Initial, root.path(), port, None);
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

    let restored = ChildGuard::spawn(protocol, Phase::Restore, root.path(), port, None);
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
                    .set_nonblocking(false)
                    .expect("configure blocking broker connection");
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
    qos: u8,
    payload: Vec<u8>,
}

fn read_publish(stream: &mut TcpStream, protocol: Protocol) -> Publish {
    let (header, body) = read_frame(stream);
    assert_eq!(header & 0xf0, 0x30, "expected PUBLISH, got {header:#04x}");
    let qos = (header & 0x06) >> 1;
    assert!(matches!(qos, 1 | 2), "expected QoS 1 or QoS 2 PUBLISH");

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
        qos,
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
        "seed" => Phase::Seed,
        "initial" => Phase::Initial,
        "restore" => Phase::Restore,
        other => panic!("unexpected child phase {other}"),
    };
    let scenario = env::var(CHILD_SCENARIO)
        .ok()
        .map(|value| V5Scenario::parse(&value));
    let fault = env::var(CHILD_FAULT)
        .ok()
        .map(|value| StoreFault::parse(&value));
    let root = PathBuf::from(env::var_os(CHILD_ROOT).expect("child session-store root"));
    let port = env::var(CHILD_PORT)
        .expect("child broker port")
        .parse()
        .expect("numeric child broker port");

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child Tokio runtime")
        .block_on(run_client(protocol, phase, scenario, fault, root, port));
}

async fn run_client(
    protocol: Protocol,
    phase: Phase,
    scenario: Option<V5Scenario>,
    fault: Option<StoreFault>,
    root: PathBuf,
    port: u16,
) {
    match protocol {
        Protocol::V4 => {
            assert!(scenario.is_none(), "MQTT 4 restart does not use scenarios");
            assert!(
                fault.is_none(),
                "MQTT 4 restart does not inject store faults"
            );
            run_v4_client(phase, root, port).await;
        }
        Protocol::V5 => run_v5_client(phase, scenario, fault, root, port).await,
    }
}

async fn run_v4_client(phase: Phase, root: PathBuf, port: u16) {
    use rumqttc_v4::{AsyncClient, Event, MqttOptions, Outgoing, PublishOptions, QoS};

    let store = v4::SessionFileStore::open(root)
        .await
        .expect("open v4 store");
    assert!(!matches!(phase, Phase::Seed));
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

async fn run_v5_client(
    phase: Phase,
    scenario: Option<V5Scenario>,
    fault: Option<StoreFault>,
    root: PathBuf,
    port: u16,
) {
    use rumqttc_v5::mqttbytes::QoS;
    use rumqttc_v5::{
        AsyncClient, ConnectionError, Event, MqttOptions, Outgoing, PublishOptions, SessionStore,
        SessionStoreKey, StateError,
    };

    let store = v5::SessionFileStore::open(root)
        .await
        .expect("open v5 store");
    if matches!(phase, Phase::Seed) {
        assert!(
            fault.is_none(),
            "seed phase must write through the real file store"
        );
        let scenario = scenario.expect("seed phase requires a scenario");
        store
            .save(
                &SessionStoreKey::new(SCOPE, CLIENT_ID),
                &seeded_v5_session(scenario),
            )
            .await
            .expect("seed MQTT 5 restart checkpoint");
        return;
    }

    let client_id = if scenario == Some(V5Scenario::ChangedClientId) {
        CHANGED_CLIENT_ID
    } else {
        CLIENT_ID
    };
    let mut options = MqttOptions::new(client_id, ("127.0.0.1", port));
    options
        .set_clean_start(false)
        .set_session_expiry_interval(Some(60))
        .set_session_store_scope(SCOPE)
        .set_keep_alive(30);
    let configured_store: Arc<dyn SessionStore> = match fault {
        Some(fault) => Arc::new(FaultInjectingV5Store {
            inner: store,
            fault,
        }),
        None => Arc::new(store),
    };
    options.set_session_store_arc(configured_store);
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(4).build();
    if matches!(phase, Phase::Initial) {
        client
            .publish(TOPIC, PAYLOAD, PublishOptions::new(QoS::AtLeastOnce))
            .await
            .expect("queue initial v5 publish");
        if scenario == Some(V5Scenario::DisconnectZeroGraceful) {
            client
                .disconnect_with_properties(
                    rumqttc_v5::DisconnectReasonCode::NormalDisconnection,
                    rumqttc_v5::DisconnectProperties {
                        session_expiry_interval: Some(0),
                        reason_string: None,
                        user_properties: Vec::new(),
                        server_reference: None,
                    },
                )
                .await
                .expect("queue zero-expiry graceful disconnect");
        }
    }

    let mut fresh_publish_sent = false;
    loop {
        let event = match eventloop.poll().await {
            Ok(event) => event,
            Err(ConnectionError::SessionStore(_)) if fault.is_some() => return,
            Err(ConnectionError::MqttState(StateError::ConnectionAborted))
                if fresh_publish_sent =>
            {
                return;
            }
            Err(ConnectionError::Io(_)) if fresh_publish_sent => return,
            Err(error) => panic!("poll v5 client: {error}"),
        };
        if matches!(phase, Phase::Initial)
            && scenario == Some(V5Scenario::DisconnectZeroGraceful)
            && matches!(event, Event::Outgoing(Outgoing::Disconnect))
        {
            return;
        }
        if !matches!(phase, Phase::Restore) {
            continue;
        }

        if matches!(
            scenario,
            Some(
                V5Scenario::MissingBrokerSession
                    | V5Scenario::ChangedClientId
                    | V5Scenario::ConnackZeroAbrupt
                    | V5Scenario::DisconnectZeroGraceful
            )
        ) && matches!(
            event,
            Event::Incoming(rumqttc_v5::mqttbytes::v5::Packet::ConnAck(_))
        ) {
            client
                .publish(TOPIC, PAYLOAD, PublishOptions::new(QoS::AtLeastOnce))
                .await
                .expect("queue fresh MQTT 5 work after reset");
            continue;
        }

        let complete = match scenario {
            None => matches!(event, Event::Outgoing(Outgoing::Publish(1))),
            Some(V5Scenario::Qos1Publish) => {
                matches!(event, Event::Outgoing(Outgoing::Publish(7)))
            }
            Some(V5Scenario::Qos2Publish) => {
                matches!(event, Event::Outgoing(Outgoing::Publish(8)))
            }
            Some(V5Scenario::PubRel) => matches!(event, Event::Outgoing(Outgoing::PubRel(9))),
            Some(V5Scenario::IncomingQos2) => {
                matches!(event, Event::Outgoing(Outgoing::PubComp(10)))
            }
            Some(V5Scenario::Subscribe) => {
                matches!(event, Event::Outgoing(Outgoing::Subscribe(11)))
            }
            Some(V5Scenario::Unsubscribe | V5Scenario::MixedReplay) => {
                matches!(event, Event::Outgoing(Outgoing::Unsubscribe(12)))
            }
            Some(
                V5Scenario::MissingBrokerSession
                | V5Scenario::ChangedClientId
                | V5Scenario::ConnackZeroAbrupt
                | V5Scenario::DisconnectZeroGraceful,
            ) => {
                if matches!(event, Event::Outgoing(Outgoing::Publish(1))) {
                    fresh_publish_sent = true;
                }
                false
            }
        };
        if complete {
            return;
        }
    }
}

fn seeded_v5_session(scenario: V5Scenario) -> rumqttc_v5::PersistedSession {
    use rumqttc_v5::{
        PersistedAckMode, PersistedFilter, PersistedIncomingQos2, PersistedPubRel,
        PersistedPublish, PersistedQoS, PersistedRequest, PersistedRetainForwardRule,
        PersistedSubscribe, PersistedUnsubscribe,
    };

    let publish = |pkid, qos| {
        PersistedRequest::Publish(PersistedPublish {
            dup: true,
            qos,
            retain: false,
            topic: TOPIC.as_bytes().to_vec(),
            pkid,
            payload: PAYLOAD.to_vec(),
            properties: None,
        })
    };
    let subscribe = || {
        PersistedRequest::Subscribe(PersistedSubscribe {
            pkid: 11,
            filters: vec![PersistedFilter {
                path: "restart/subscription".to_owned(),
                qos: PersistedQoS::AtLeastOnce,
                nolocal: false,
                preserve_retain: false,
                retain_forward_rule: PersistedRetainForwardRule::OnEverySubscribe,
            }],
            properties: None,
        })
    };
    let unsubscribe = || {
        PersistedRequest::Unsubscribe(PersistedUnsubscribe {
            pkid: 12,
            filters: vec!["restart/subscription".to_owned()],
            properties: None,
        })
    };

    let replay = match scenario {
        V5Scenario::Qos1Publish
        | V5Scenario::MissingBrokerSession
        | V5Scenario::ChangedClientId => vec![publish(7, PersistedQoS::AtLeastOnce)],
        V5Scenario::Qos2Publish => vec![publish(8, PersistedQoS::ExactlyOnce)],
        V5Scenario::PubRel => vec![PersistedRequest::PubRel(PersistedPubRel { pkid: 9 })],
        V5Scenario::IncomingQos2 => Vec::new(),
        V5Scenario::Subscribe => vec![subscribe()],
        V5Scenario::Unsubscribe => vec![unsubscribe()],
        V5Scenario::MixedReplay => vec![
            publish(7, PersistedQoS::AtLeastOnce),
            PersistedRequest::PubRel(PersistedPubRel { pkid: 9 }),
            subscribe(),
            unsubscribe(),
        ],
        V5Scenario::ConnackZeroAbrupt | V5Scenario::DisconnectZeroGraceful => {
            panic!("zero-expiry scenarios are created through the MQTT client")
        }
    };
    let incoming_qos2 = if matches!(scenario, V5Scenario::IncomingQos2 | V5Scenario::MixedReplay) {
        vec![PersistedIncomingQos2 { pkid: 10 }]
    } else {
        Vec::new()
    };

    rumqttc_v5::PersistedSession {
        format_version: 2,
        client_id: CLIENT_ID.to_owned(),
        clean_start: false,
        session_expiry_interval: Some(60),
        outgoing_inflight_upper_limit: None,
        ack_mode: PersistedAckMode::Automatic,
        replay,
        incoming_qos2,
    }
}
