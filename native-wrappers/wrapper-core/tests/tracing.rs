#![cfg(feature = "tracing")]

use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rumqttc_wrapper_core::*;
use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};

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
        let mut client = NativeClient::start(config).unwrap();
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
    ] {
        assert!(!output.contains(secret), "trace leaked {secret}");
    }
}
