#![cfg(feature = "tracing")]
mod support;
use rumqttc_wrapper_core::*;
use std::{
    io::Write,
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};
struct Host(Arc<Mutex<Vec<String>>>, AtomicU64);
impl tracing::Subscriber for Host {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        self.0.lock().unwrap().push(attrs.metadata().name().into());
        tracing::span::Id::from_u64(self.1.fetch_add(1, Ordering::Relaxed) + 1)
    }
    fn event(&self, event: &tracing::Event<'_>) {
        self.0.lock().unwrap().push(event.metadata().name().into());
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}
struct Sink(Arc<Mutex<Vec<String>>>);
impl LogSink for Sink {
    fn record(&self, record: &LogRecord) -> bool {
        self.0.lock().unwrap().push(record.name.clone());
        true
    }
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Verify host coexistence and exclusive capture through both protocol lifecycles"
)]
fn scoped_client_logging_coexists_with_host_and_unattached_peers() {
    let host = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::set_global_default(Host(host.clone(), AtomicU64::new(0))).unwrap();
    let logging = LoggingContext::new(LoggingOptions::default()).unwrap();
    let records = Arc::new(Mutex::new(Vec::new()));
    let sink = logging
        .register_with(
            LogFilter {
                level: LogLevel::Trace,
                targets: vec![],
            },
            || Box::new(Sink(records.clone())),
        )
        .unwrap();
    let execution = ExecutionContext::new(ExecutionOptions {
        worker_threads: 2,
        ..ExecutionOptions::default()
    })
    .unwrap();
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = thread::spawn(move || {
            let mut s = support::accept(&listener);
            support::connect(&mut s, mqtt5);
            assert_eq!(support::frame(&mut s)[0], 0xe0);
        });
        let before = host
            .lock()
            .unwrap()
            .iter()
            .filter(|n| *n == "mqtt.connection_established")
            .count();
        let wrapper_before = host
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.starts_with("mqtt.wrapper."))
            .count();
        let mut client = NativeClient::start_in_with_logging(
            support::config(mqtt5, port),
            &execution,
            Some(sink.clone()),
        )
        .unwrap();
        let _events = support::connected(&mut client);
        tracing::event!(name: "host.unrelated", tracing::Level::INFO, "application work");
        client.closer().close(support::DEADLINE).unwrap();
        broker.join().unwrap();
        assert!(logging.flush().wait(support::DEADLINE).unwrap());
        assert_eq!(
            host.lock()
                .unwrap()
                .iter()
                .filter(|n| *n == "mqtt.connection_established")
                .count(),
            before
        );
        assert_eq!(
            host.lock()
                .unwrap()
                .iter()
                .filter(|n| n.starts_with("mqtt.wrapper."))
                .count(),
            wrapper_before,
            "attached wrapper instrumentation must not fan out to the host"
        );
        client.handle().set_logging(None).unwrap();
        drop(client);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = thread::spawn(move || {
            let mut s = support::accept(&listener);
            support::connect(&mut s, mqtt5);
            assert_eq!(support::frame(&mut s)[0], 0xe0);
            s.flush().unwrap();
        });
        let mut peer = NativeClient::start_in(support::config(mqtt5, port), &execution).unwrap();
        let _events = support::connected(&mut peer);
        peer.closer().close(support::DEADLINE).unwrap();
        broker.join().unwrap();
        assert_eq!(
            host.lock()
                .unwrap()
                .iter()
                .filter(|n| *n == "mqtt.connection_established")
                .count(),
            before + 1
        );
    }
    assert!(host.lock().unwrap().iter().any(|n| n == "host.unrelated"));
    assert!(
        records
            .lock()
            .unwrap()
            .iter()
            .any(|n| n == "mqtt.connection_established")
    );
    drop(sink);
    logging.request_shutdown();
    assert!(logging.join(support::DEADLINE).unwrap());
    execution.request_shutdown();
    execution.join(support::DEADLINE).unwrap();
}
