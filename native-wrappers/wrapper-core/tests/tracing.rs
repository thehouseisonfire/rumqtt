#![cfg(feature = "tracing")]

mod support;

use std::fmt::Write;
use std::io::Write as IoWrite;
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rumqttc_wrapper_core::*;
use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};

fn observe_private_terminal_ack() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = std::thread::spawn(move || {
        let mut stream = support::accept(&listener);
        support::connect(&mut stream, true);
        let publish = support::frame(&mut stream);
        let topic_len = usize::from(u16::from_be_bytes([publish[2], publish[3]]));
        let id = &publish[4 + topic_len..6 + topic_len];
        let properties = b"\x1f\x00\x0eprivate-reason\x26\x00\x0bprivate-key\x00\x0dprivate-value";
        let mut ack = vec![
            0x40,
            u8::try_from(4 + properties.len()).unwrap(),
            id[0],
            id[1],
            0x87,
            u8::try_from(properties.len()).unwrap(),
        ];
        ack.extend_from_slice(properties);
        stream.write_all(&ack).unwrap();
        assert_eq!(support::frame(&mut stream)[0], 0xe0);
    });
    let mut client = support::start(support::config(true, port)).unwrap();
    let mut events = client.take_events().unwrap();
    assert!(matches!(
        events.recv_timeout(support::DEADLINE).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));
    let admission = client
        .handle()
        .try_admit(Command::Publish(PublishCommand {
            topic: "a".into(),
            payload: bytes::Bytes::new(),
            qos: QoS::AtLeastOnce,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap();
    assert_eq!(
        admission
            .completion
            .wait_timeout(support::DEADLINE)
            .unwrap_err()
            .broker_reason(),
        Some(0x87)
    );
    let outcome = admission.completion.try_outcome().unwrap();
    assert_eq!(
        outcome
            .acknowledgement()
            .unwrap()
            .properties()
            .unwrap()
            .reason_string
            .as_deref(),
        Some("private-reason")
    );
    client.closer().close_now(support::DEADLINE).unwrap();
    broker.join().unwrap();
}

#[derive(Default)]
struct Capture {
    output: Arc<Mutex<String>>,
    next: AtomicU64,
}

struct Fields<'a>(&'a mut String);
impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        write!(self.0, " {}={value:?}", field.name()).unwrap();
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        let mut output = self.output.lock().unwrap();
        output.push_str(attributes.metadata().name());
        attributes.record(&mut Fields(&mut output));
        drop(output);
        Id::from_u64(self.next.fetch_add(1, Ordering::Relaxed) + 1)
    }
    fn record(&self, _: &Id, values: &Record<'_>) {
        values.record(&mut Fields(&mut self.output.lock().unwrap()));
    }
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        event.record(&mut Fields(&mut self.output.lock().unwrap()));
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[test]
fn lifecycle_and_admission_traces_do_not_capture_credentials_or_commands() {
    let capture = Capture::default();
    let output = capture.output.clone();
    // This integration-test process has one test; the global subscriber also
    // captures the dedicated driver thread, unlike a thread-local dispatcher.
    tracing::subscriber::set_global_default(capture).unwrap();
    for mqtt5 in [false, true] {
        let mut config = if mqtt5 {
            ClientConfig::v5("private-client-id", "127.0.0.1", 1)
        } else {
            ClientConfig::v4("private-client-id", "127.0.0.1", 1)
        };
        config.common.username = Some("private-username".into());
        config.common.password = Some(b"private-password".as_slice().into());
        let mut client = support::start(config).unwrap();
        let mut events = client.take_events().unwrap();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Some(WrapperEvent::Disconnected { .. })
        ));
        let _admission = client.handle().try_admit(Command::Publish(PublishCommand {
            topic: "private-topic".into(),
            payload: b"private-payload".as_slice().into(),
            qos: QoS::AtLeastOnce,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }));
        client.closer().close_now(Duration::from_secs(3)).unwrap();
    }
    observe_private_terminal_ack();
    let output = output.lock().unwrap();
    assert!(output.contains("start"));
    assert!(output.contains("drive"));
    assert!(output.contains("try_admit"));
    for secret in [
        "private-client-id",
        "private-username",
        "private-password",
        "private-topic",
        "private-payload",
        "private-reason",
        "private-key",
        "private-value",
    ] {
        assert!(!output.contains(secret), "trace leaked {secret}");
    }
    drop(output);
}
