use log::{Log, Metadata, Record};
use rumqttc::{ConnectionError, EventLoop, MqttOptions};
use std::sync::Mutex;
use std::time::Duration;

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

    let mut options = MqttOptions::new("log-compat", "localhost");
    // A zero timeout can still succeed against a local broker. Keep the connector pending
    // so the timeout and its lifecycle records are independent of the host environment.
    options.set_socket_connector(|_, _| {
        std::future::pending::<std::io::Result<tokio::io::DuplexStream>>()
    });
    options.set_connect_timeout(Duration::ZERO);
    let mut eventloop = EventLoop::new(options, 1);

    assert!(matches!(
        eventloop.poll().await,
        Err(ConnectionError::Timeout(_))
    ));

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
        message.contains("phase=\"transport\"") && message.contains("error_kind=\"timeout\"")
    }));
}
