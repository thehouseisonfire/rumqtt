#![allow(dead_code)]

pub mod capture;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use rumqttc_wrapper_core::*;

pub const DEADLINE: Duration = Duration::from_secs(5);

pub fn process_output(command: &mut std::process::Command) -> std::process::Output {
    use std::process::Stdio;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + DEADLINE * 2;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("fixture subprocess timed out");
        }
        thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

pub fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + DEADLINE;
    loop {
        match listener.accept() {
            Ok((socket, _)) => {
                socket.set_read_timeout(Some(DEADLINE)).unwrap();
                socket.set_write_timeout(Some(DEADLINE)).unwrap();
                return socket;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "broker accept timed out");
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("broker accept failed: {error}"),
        }
    }
}

pub fn frame(stream: &mut impl Read) -> BytesMut {
    let mut byte = [0];
    stream.read_exact(&mut byte).unwrap();
    let mut frame = BytesMut::from(byte.as_slice());
    let mut length = 0;
    for shift in [0, 7, 14, 21] {
        stream.read_exact(&mut byte).unwrap();
        frame.extend_from_slice(&byte);
        length |= usize::from(byte[0] & 127) << shift;
        if byte[0] < 128 {
            assert!(length <= 1024 * 1024, "fixture packet too large");
            let header = frame.len();
            frame.resize(header + length, 0);
            stream.read_exact(&mut frame[header..]).unwrap();
            return frame;
        }
    }
    panic!("invalid MQTT remaining length");
}

pub fn connect(socket: &mut TcpStream, mqtt5: bool) {
    assert_eq!(frame(socket)[0], 0x10);
    socket
        .write_all(if mqtt5 {
            &[0x20, 3, 0, 0, 0]
        } else {
            &[0x20, 2, 0, 0]
        })
        .unwrap();
}

pub fn config(mqtt5: bool, port: u16) -> ClientConfig {
    if mqtt5 {
        ClientConfig::v5("parity", "127.0.0.1", port)
    } else {
        ClientConfig::v4("parity", "127.0.0.1", port)
    }
}

pub fn connected(client: &mut NativeClient) -> EventConsumer {
    let mut events = client.take_events().unwrap();
    until(&mut events, |event| {
        matches!(event, WrapperEvent::Connected { .. })
    });
    events
}

pub fn until(
    events: &mut EventConsumer,
    predicate: impl Fn(&WrapperEvent) -> bool,
) -> WrapperEvent {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap()
            .expect("event stream ended before expected event");
        if predicate(&event) {
            return event;
        }
        assert!(
            !matches!(event, WrapperEvent::DriverTerminated(_)),
            "unexpected termination: {event:?}"
        );
        assert!(Instant::now() < deadline, "expected event timed out");
    }
}

pub fn publish(client: &NativeClient, payload: &'static [u8]) -> Admission {
    client
        .handle()
        .try_admit(Command::Publish(PublishCommand {
            topic: "a".into(),
            payload: payload.into(),
            qos: QoS::AtLeastOnce,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap()
}

pub fn publish_id(socket: &mut TcpStream, mqtt5: bool) -> u16 {
    let mut packet = frame(socket);
    if mqtt5 {
        let rumqttc_v5::Packet::Publish(packet) =
            rumqttc_v5::Packet::read(&mut packet, None).unwrap()
        else {
            panic!("expected publish")
        };
        packet.pkid
    } else {
        let rumqttc_v4::Packet::Publish(packet) =
            rumqttc_v4::Packet::read(&mut packet, 1024 * 1024).unwrap()
        else {
            panic!("expected publish")
        };
        packet.pkid
    }
}

pub fn puback(socket: &mut TcpStream, id: u16) {
    let [high, low] = id.to_be_bytes();
    socket.write_all(&[0x40, 2, high, low]).unwrap();
}

pub fn terminal(admission: &Admission) -> Result<Completion> {
    match admission.completion.wait_timeout_outcome(DEADLINE) {
        CompletionWaitOutcome::Completed(result) => result,
        CompletionWaitOutcome::DeadlineElapsed => panic!("operation remained unresolved"),
    }
}

pub struct Broker {
    done: mpsc::Receiver<()>,
    thread: thread::JoinHandle<()>,
}

impl Broker {
    pub fn spawn(work: impl FnOnce() + Send + 'static) -> Self {
        let (done_tx, done) = mpsc::channel();
        let thread = thread::spawn(move || {
            work();
            let _ = done_tx.send(());
        });
        Self { done, thread }
    }

    pub fn join(self) {
        self.done
            .recv_timeout(DEADLINE)
            .expect("broker did not finish successfully");
        self.thread.join().unwrap();
    }
}
