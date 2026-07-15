use log::{Log, Metadata, Record};
use rumqttc::{EventLoop, MqttOptions, NetworkOptions};
use std::sync::Mutex;

#[derive(Default)]
struct CaptureLogger {
    records: Mutex<Vec<(String, String)>>,
}

impl Log for CaptureLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == "rumqttc::lifecycle"
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            self.records
                .lock()
                .unwrap()
                .push((record.target().to_owned(), record.args().to_string()));
        }
    }

    fn flush(&self) {}
}

static LOGGER: CaptureLogger = CaptureLogger {
    records: Mutex::new(Vec::new()),
};

#[tokio::test]
async fn tracing_events_fall_back_to_one_log_record_each() {
    log::set_logger(&LOGGER).expect("integration-test process should not have a logger");
    log::set_max_level(log::LevelFilter::Trace);

    let mut eventloop = EventLoop::new(MqttOptions::new("log-compat", "localhost"), 1);
    let mut network_options = NetworkOptions::new();
    network_options.set_connection_timeout(0);
    eventloop.set_network_options(network_options);

    assert!(eventloop.poll().await.is_err());

    let records = LOGGER.records.lock().unwrap();
    assert_eq!(
        records.len(),
        2,
        "expected attempt and attempt-failed records"
    );
    assert!(
        records
            .iter()
            .all(|(target, _)| target == "rumqttc::lifecycle")
    );
    assert!(
        records
            .iter()
            .any(|(_, message)| message.contains("attempt_id=1"))
    );
    assert!(records.iter().any(|(_, message)| {
        message.contains("phase=\"connection\"") && message.contains("error_kind=\"timeout\"")
    }));
}
