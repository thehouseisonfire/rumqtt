#![cfg(feature = "tracing-log-compat")]
mod support;
use rumqttc_wrapper_core::*;
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<String>>>);
impl log::Log for Host {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        self.0
            .lock()
            .unwrap()
            .push(format!("{} {}", record.target(), record.args()));
    }
    fn flush(&self) {}
}
struct Sink;
impl LogSink for Sink {
    fn record(&self, _: &LogRecord) -> bool {
        true
    }
}
#[test]
fn scoped_capture_preserves_the_hosts_process_wide_tracing_log_fallback() {
    let entries = Arc::new(Mutex::new(Vec::new()));
    log::set_boxed_logger(Box::new(Host(entries.clone()))).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    assert!(!tracing::dispatcher::has_been_set());
    tracing::info!(target: "host.fallback", "before");
    let logging = LoggingContext::new(LoggingOptions::default()).unwrap();
    let sink = logging
        .register_with(LogFilter::default(), || Box::new(Sink))
        .unwrap();
    let client = NativeClient::start_with_logging(
        ClientConfig::v4("fallback", "127.0.0.1", 65535),
        Some(sink.clone()),
    )
    .unwrap();
    client.closer().close_now(support::DEADLINE).unwrap();
    client.handle().set_logging(None).unwrap();
    drop(client);
    drop(sink);
    logging.request_shutdown();
    assert!(logging.join(support::DEADLINE).unwrap());
    assert!(
        !tracing::dispatcher::has_been_set(),
        "source routing must not latch a tracing default"
    );
    tracing::info!(target: "host.fallback", "after");
    let entries = entries.lock().unwrap().clone();
    assert!(entries.iter().any(|entry| entry == "host.fallback before"));
    assert!(entries.iter().any(|entry| entry == "host.fallback after"));
}
