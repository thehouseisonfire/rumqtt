#![cfg(feature = "tracing")]
mod support;
use rumqttc_wrapper_core::*;
use std::{
    io::Write,
    net::TcpListener,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

struct Capture {
    records: Arc<Mutex<Vec<LogRecord>>>,
    gate: Option<(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>)>,
    first: std::sync::atomic::AtomicBool,
}
impl LogSink for Capture {
    fn record(&self, record: &LogRecord) -> bool {
        if let Some((entered, release)) = &self.gate
            && !self.first.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            entered.send(()).unwrap();
            release
                .lock()
                .unwrap()
                .recv_timeout(support::DEADLINE)
                .unwrap();
        }
        self.records.lock().unwrap().push(record.clone());
        true
    }
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Exercise a stalled logging worker through the full MQTT lifecycle"
)]
fn stalled_logging_does_not_stop_qos_keepalive_reconnect_or_mqtt_shutdown() {
    for mqtt5 in [false, true] {
        for shared in [false, true] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker = thread::spawn(move || {
                let mut first = support::accept(&listener);
                support::connect(&mut first, mqtt5);
                loop {
                    let frame = support::frame(&mut first);
                    if frame[0] == 0xc0 {
                        first.write_all(&[0xd0, 0]).unwrap();
                        break;
                    }
                    assert_eq!(frame[0] >> 4, 3);
                    let topic_length = usize::from(u16::from_be_bytes([frame[2], frame[3]]));
                    let id = &frame[4 + topic_length..6 + topic_length];
                    first.write_all(&[0x40, 2, id[0], id[1]]).unwrap();
                }
                drop(first);
                let mut second = support::accept(&listener);
                support::connect(&mut second, mqtt5);
                loop {
                    let frame = support::frame(&mut second);
                    if frame[0] == 0xe0 {
                        break;
                    }
                    assert_eq!(frame[0], 0xc0);
                    second.write_all(&[0xd0, 0]).unwrap();
                }
            });
            let records = Arc::new(Mutex::new(Vec::new()));
            let (entered, seen) = mpsc::channel();
            let (release, wait) = mpsc::channel();
            let logging = LoggingContext::new(LoggingOptions {
                record_capacity: 2,
                ..LoggingOptions::default()
            })
            .unwrap();
            let sink = logging
                .register_with(
                    LogFilter {
                        level: LogLevel::Trace,
                        targets: vec![],
                    },
                    || {
                        Box::new(Capture {
                            records: records.clone(),
                            gate: Some((entered, Mutex::new(wait))),
                            first: false.into(),
                        })
                    },
                )
                .unwrap();
            let execution = shared.then(|| {
                ExecutionContext::new(ExecutionOptions {
                    worker_threads: 2,
                    ..ExecutionOptions::default()
                })
                .unwrap()
            });
            let mut config = support::config(mqtt5, port);
            config.common.keep_alive = Duration::from_secs(1);
            config.common.username = Some("private-username".into());
            config.common.password = Some(b"private-password".as_slice().into());
            let mut client = match &execution {
                Some(execution) => {
                    NativeClient::start_in_with_logging(config, execution, Some(sink.clone()))
                }
                None => NativeClient::start_with_logging(config, Some(sink.clone())),
            }
            .unwrap();
            seen.recv_timeout(support::DEADLINE).unwrap();
            let mut events = support::connected(&mut client);
            let handle = client.handle();
            let completion = handle
                .try_admit(Command::Publish(PublishCommand {
                    topic: "private-topic".into(),
                    payload: b"private-payload".as_slice().into(),
                    qos: QoS::AtLeastOnce,
                    retain: false,
                    protocol: PublishProtocolOptions::VersionNeutral,
                }))
                .unwrap()
                .completion;
            completion.wait_timeout(support::DEADLINE).unwrap();
            support::until(&mut events, |event| {
                matches!(event, WrapperEvent::Disconnected { .. })
            });
            support::until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            assert!(sink.statistics().overload > 0);
            client.closer().close(support::DEADLINE).unwrap();
            broker.join().unwrap();
            let flush = logging.flush();
            assert!(!flush.wait(Duration::ZERO).unwrap());
            release.send(()).unwrap();
            assert!(flush.wait(support::DEADLINE).unwrap());
            handle.set_logging(None).unwrap();
            drop(handle);
            drop(client);
            drop(sink);
            logging.request_shutdown();
            assert!(logging.join(support::DEADLINE).unwrap());
            if let Some(execution) = execution {
                execution.request_shutdown();
                execution.join(support::DEADLINE).unwrap();
            }
            let records = records.lock().unwrap();
            assert!(!records.is_empty());
            let formatted = format!("{records:?}");
            drop(records);
            for secret in [
                "private-username",
                "private-password",
                "private-topic",
                "private-payload",
            ] {
                assert!(!formatted.contains(secret));
            }
        }
    }
}
fn capture_lifecycle_with_replacement(
    mqtt5: bool,
) -> (Vec<LogRecord>, Vec<LogRecord>, LoggingStatistics) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (replace, replaced) = mpsc::channel();
    let broker = thread::spawn(move || {
        let mut first = support::accept(&listener);
        support::connect(&mut first, mqtt5);
        replaced.recv_timeout(support::DEADLINE).unwrap();
        drop(first);
        let mut second = support::accept(&listener);
        support::connect(&mut second, mqtt5);
        assert_eq!(support::frame(&mut second)[0], 0xe0);
    });
    let context = LoggingContext::new(LoggingOptions::default()).unwrap();
    let first = Arc::new(Mutex::new(Vec::new()));
    let second = Arc::new(Mutex::new(Vec::new()));
    let registration = |records: Arc<Mutex<Vec<LogRecord>>>| {
        context
            .register_with(
                LogFilter {
                    level: LogLevel::Trace,
                    targets: vec![],
                },
                || {
                    Box::new(Capture {
                        records,
                        gate: None,
                        first: false.into(),
                    })
                },
            )
            .unwrap()
    };
    let a = registration(first.clone());
    let b = registration(second.clone());
    let execution = ExecutionContext::new(ExecutionOptions {
        worker_threads: 2,
        ..ExecutionOptions::default()
    })
    .unwrap();
    let mut client = NativeClient::start_in_with_logging(
        support::config(mqtt5, port),
        &execution,
        Some(a.clone()),
    )
    .unwrap();
    let mut events = support::connected(&mut client);
    assert!(context.flush().wait(support::DEADLINE).unwrap());
    client.handle().set_logging(Some(b.clone())).unwrap();
    replace.send(()).unwrap();
    support::until(&mut events, |e| matches!(e, WrapperEvent::Connected { .. }));
    client.closer().close(support::DEADLINE).unwrap();
    assert!(context.flush().wait(support::DEADLINE).unwrap());
    broker.join().unwrap();
    client.handle().set_logging(None).unwrap();
    drop(client);
    drop(a);
    drop(b);
    context.request_shutdown();
    assert!(context.join(support::DEADLINE).unwrap());
    execution.request_shutdown();
    execution.join(support::DEADLINE).unwrap();
    let stats = context.statistics();
    let first = first.lock().unwrap().clone();
    let second = second.lock().unwrap().clone();
    (first, second, stats)
}

#[test]
fn lifecycle_records_keep_typed_fields_and_identity_across_reconnect_and_replacement() {
    for mqtt5 in [false, true] {
        let mut captured = false;
        for _ in 0..16 {
            let (first, second, stats) = capture_lifecycle_with_replacement(mqtt5);
            assert_eq!(stats.failures, 0);
            assert_eq!(stats.rejected, 0);
            assert_eq!(stats.discarded, 0);
            assert_eq!(stats.delivered, stats.accepted);
            assert_eq!(
                stats.delivered,
                u64::try_from(first.len() + second.len()).unwrap()
            );
            if let Some(anchor) = first.first().or_else(|| second.first()) {
                assert_ne!(anchor.client_id, 0);
                assert!(
                    first
                        .iter()
                        .chain(&second)
                        .all(|record| record.client_id == anchor.client_id)
                );
            }
            let initial = first
                .iter()
                .find(|r| r.name == "mqtt.connection_established");
            let reconnected = second
                .iter()
                .find(|r| r.name == "mqtt.connection_established");
            let shutdown = second
                .iter()
                .any(|r| r.name == "mqtt.wrapper.shutdown_completed");
            let (Some(initial), Some(reconnected), true) = (initial, reconnected, shutdown) else {
                let missing = [initial.is_none(), reconnected.is_none(), !shutdown]
                    .into_iter()
                    .filter(|missing| *missing)
                    .count();
                assert!(
                    stats.overload >= u64::try_from(missing).unwrap(),
                    "required lifecycle records missing without admission loss"
                );
                continue;
            };
            assert_ne!(initial.client_id, 0);
            assert_eq!(initial.client_id, reconnected.client_id);
            assert!(
                first
                    .iter()
                    .chain(&second)
                    .all(|record| record.client_id == initial.client_id)
            );
            assert!(
                reconnected
                    .fields
                    .iter()
                    .any(|f| f.name == "reconnect" && f.value == LogValue::Bool(true))
            );
            assert!(
                reconnected
                    .fields
                    .iter()
                    .any(|f| f.name == "attempt_id" && matches!(f.value, LogValue::Unsigned(_)))
            );
            captured = true;
            break;
        }
        assert!(
            captured,
            "could not capture all lifecycle records in 16 overload-accounted attempts"
        );
    }
}

fn capture_accepted_session_present_mismatch() -> (Vec<LogRecord>, LoggingStatistics) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let mut socket = support::accept(&listener);
        let connect = support::frame(&mut socket);
        assert_eq!(connect[0], 0x10);
        assert_ne!(connect[9] & 2, 0, "fixture requires a clean session");
        socket.write_all(&[0x20, 2, 1, 0]).unwrap();
        assert_eq!(support::frame(&mut socket)[0], 0xe0);
    });
    let context = LoggingContext::new(LoggingOptions::default()).unwrap();
    let records = Arc::new(Mutex::new(Vec::new()));
    let registration = context
        .register_with(
            LogFilter {
                level: LogLevel::Warn,
                targets: vec!["rumqttc::lifecycle".into()],
            },
            || {
                Box::new(Capture {
                    records: records.clone(),
                    gate: None,
                    first: false.into(),
                })
            },
        )
        .unwrap();
    let mut config = support::config(false, port);
    let ProtocolConfig::V4(v4) = &mut config.protocol else {
        unreachable!();
    };
    v4.session_present_mismatch_policy = SessionPresentMismatchPolicy::AcceptAsClean;
    let mut client = NativeClient::start_with_logging(config, Some(registration.clone())).unwrap();
    let _events = support::connected(&mut client);
    client.closer().close(support::DEADLINE).unwrap();
    broker.join().unwrap();
    drop(client);
    drop(registration);
    context.request_shutdown();
    assert!(context.join(support::DEADLINE).unwrap());
    let stats = context.statistics();
    let records = records.lock().unwrap().clone();
    (records, stats)
}

#[test]
fn accepted_v4_session_mismatch_records_preserve_the_brokers_raw_flag() {
    for _ in 0..16 {
        let (records, stats) = capture_accepted_session_present_mismatch();
        assert_eq!(stats.failures, 0);
        assert_eq!(stats.rejected, 0);
        assert_eq!(stats.discarded, 0);
        assert_eq!(stats.accepted, stats.delivered);
        assert_eq!(stats.delivered, u64::try_from(records.len()).unwrap());
        let Some(record) = records
            .iter()
            .find(|record| record.name == "mqtt.protocol_compatibility")
        else {
            assert!(
                stats.overload > 0,
                "compatibility record missing without admission loss"
            );
            continue;
        };
        assert!(!record.truncated);
        for (name, value) in [
            ("raw_session_present", LogValue::Bool(true)),
            ("session_resumed", LogValue::Bool(false)),
            (
                "compatibility",
                LogValue::Text("session_present_mismatch_accepted_as_clean".into()),
            ),
        ] {
            assert!(
                record
                    .fields
                    .iter()
                    .any(|field| field.name == name && field.value == value),
                "missing or mistyped compatibility field {name}: {record:?}"
            );
        }
        return;
    }
    panic!("could not capture the compatibility record in 16 overload-accounted attempts");
}
