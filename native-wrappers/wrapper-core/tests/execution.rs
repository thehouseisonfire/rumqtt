mod support;

use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use rumqttc_wrapper_core::*;

const DEADLINE: Duration = Duration::from_secs(5);

fn context(capacity: usize) -> ExecutionContext {
    ExecutionContext::new(ExecutionOptions {
        client_capacity: capacity,
        worker_threads: 1,
        max_blocking_threads: 2,
    })
    .unwrap()
}

fn config(mqtt5: bool) -> ClientConfig {
    if mqtt5 {
        ClientConfig::v5("execution-v5", "127.0.0.1", 65535)
    } else {
        ClientConfig::v4("execution-v4", "127.0.0.1", 65535)
    }
}

#[test]
fn context_capacity_is_atomic_and_failed_start_returns_its_reservation() {
    for mqtt5 in [false, true] {
        let execution = context(1);
        let mut invalid = config(mqtt5);
        invalid.common.request_channel_capacity = 0;
        assert_eq!(
            NativeClient::start_in(invalid, &execution)
                .unwrap_err()
                .kind(),
            ErrorKind::Configuration
        );
        let barrier = Arc::new(Barrier::new(9));
        #[allow(
            clippy::needless_collect,
            reason = "Spawn every participant before entering the barrier"
        )]
        let starts = (0..8)
            .map(|_| {
                let execution = execution.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    NativeClient::start_in(config(mqtt5), &execution)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let clients = starts
            .into_iter()
            .filter_map(|start| match start.join().unwrap() {
                Ok(client) => Some(client),
                Err(error) => {
                    assert_eq!(error.kind(), ErrorKind::Backpressure);
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(clients.len(), 1);
        clients[0].closer().close_now(DEADLINE).unwrap();
        let replacement = NativeClient::start_in(config(mqtt5), &execution).unwrap();
        execution.request_shutdown();
        replacement.join(DEADLINE).unwrap();
        execution.join(DEADLINE).unwrap();
    }
}

#[test]
fn shutdown_racing_start_closes_every_accepted_client_and_rejects_later_starts() {
    for mqtt5 in [false, true] {
        for _ in 0..16 {
            let execution = context(8);
            let barrier = Arc::new(Barrier::new(2));
            let starting = {
                let execution = execution.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    NativeClient::start_in(config(mqtt5), &execution)
                })
            };
            barrier.wait();
            execution.request_shutdown();
            match starting.join().unwrap() {
                Ok(client) => client.join(DEADLINE).unwrap(),
                Err(error) => assert_eq!(error.kind(), ErrorKind::Shutdown),
            }
            execution.request_shutdown();
            execution.join(DEADLINE).unwrap();
            assert_eq!(execution.state(), ExecutionState::Quiescent);
            assert!(execution.try_join().unwrap());
            assert_eq!(
                NativeClient::start_in(config(mqtt5), &execution)
                    .unwrap_err()
                    .delivery_status(),
                DeliveryStatus::NotAdmitted
            );
        }
    }
}

#[test]
fn panic_is_terminal_once_and_does_not_stop_a_peer() {
    for mqtt5 in [false, true] {
        let execution = context(2);
        let mut failed = NativeClient::start_in(config(mqtt5), &execution).unwrap();
        let peer = NativeClient::start_in(config(mqtt5), &execution).unwrap();
        let pending = failed
            .handle()
            .try_admit(Command::Diagnostics)
            .unwrap()
            .completion;
        failed.handle().terminate_for_internal_panic();
        failed.join(DEADLINE).unwrap();
        let mut events = failed.take_events().unwrap();
        let mut terminal_count = 0;
        while let Some(event) = events.try_recv().unwrap() {
            if let WrapperEvent::DriverTerminated(error) = event {
                assert_eq!(error.code(), ErrorCode::InternalPanic);
                terminal_count += 1;
            }
        }
        assert_eq!(terminal_count, 1);
        let _ = pending.wait_timeout(DEADLINE);
        assert_eq!(peer.handle().state(), LifecycleState::Running);
        let diagnostic = peer.handle().try_admit(Command::Diagnostics).unwrap();
        assert!(matches!(
            diagnostic.completion.wait_timeout(DEADLINE).unwrap(),
            Completion::Diagnostics(_)
        ));
        peer.closer().close_now(DEADLINE).unwrap();
        assert_eq!(execution.state(), ExecutionState::Open);
        execution.request_shutdown();
        execution.join(DEADLINE).unwrap();
    }
}

#[test]
fn releasing_context_references_does_not_stop_clients() {
    for mqtt5 in [false, true] {
        let execution = context(1);
        let observer = execution.clone();
        let client = NativeClient::start_in(config(mqtt5), &execution).unwrap();
        drop(execution);
        assert_eq!(observer.state(), ExecutionState::Open);
        assert_eq!(client.handle().state(), LifecycleState::Running);
        client.closer().close_now(DEADLINE).unwrap();
        observer.request_shutdown();
        observer.join(DEADLINE).unwrap();
    }
}

#[test]
fn both_protocols_complete_qos_work_and_graceful_close_on_shared_execution() {
    use std::io::Write;
    use std::net::TcpListener;
    use support::{
        Broker, accept, connect, connected, frame, puback, publish, publish_id, terminal,
    };
    for mqtt5 in [false, true] {
        let execution = context(2);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            let id = publish_id(&mut socket, mqtt5);
            puback(&mut socket, id);
            assert_eq!(frame(&mut socket)[0] >> 4, 14);
            socket.flush().unwrap();
        });
        let mut cfg = config(mqtt5);
        cfg.common.broker = BrokerTarget::Tcp {
            host: "127.0.0.1".into(),
            port,
        };
        let mut client = NativeClient::start_in(cfg, &execution).unwrap();
        let _events = connected(&mut client);
        assert_eq!(
            terminal(&publish(&client, b"shared")).unwrap(),
            Completion::Publish(PublishCompletion::Qos1Acknowledged)
        );
        client.closer().close(DEADLINE).unwrap();
        execution.request_shutdown();
        execution.join(DEADLINE).unwrap();
        broker.join();
    }
}

struct SlowRetry {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    peer: NativeClientCloser,
    completion: CompletionHandle,
}

impl TransportConnector for SlowRetry {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        use std::sync::atomic::Ordering;
        // Finite synchronous host work cannot be preempted, but must not starve peers forever.
        thread::sleep(Duration::from_millis(1));
        assert_eq!(
            self.peer
                .close_now(Duration::from_millis(1))
                .unwrap_err()
                .kind(),
            ErrorKind::Shutdown
        );
        assert_eq!(
            self.completion
                .wait_timeout(Duration::from_millis(1))
                .unwrap_err()
                .kind(),
            ErrorKind::Shutdown
        );
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Err(TransportFailure::Connect) })
    }
}

#[test]
fn one_worker_keeps_mqtt_progress_under_control_flood_and_slow_reconnect_callbacks() {
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use support::{
        Broker, accept, connect, connected, frame, puback, publish, publish_id, terminal,
    };
    for mqtt5 in [false, true] {
        let execution = context(6);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            for _ in 0..32 {
                let id = publish_id(&mut socket, mqtt5);
                puback(&mut socket, id);
            }
            assert_eq!(frame(&mut socket)[0] >> 4, 14);
        });
        let mut cfg = support::config(mqtt5, port);
        cfg.common.request_channel_capacity = 64;
        let mut peer = NativeClient::start_in(cfg, &execution).unwrap();
        let _events = connected(&mut peer);
        let peer_diagnostics = peer
            .handle()
            .try_admit(Command::Diagnostics)
            .unwrap()
            .completion;
        let calls = Arc::new(AtomicUsize::new(0));
        let mut storms = Vec::new();
        for _ in 0..4 {
            let mut cfg = config(mqtt5);
            cfg.common.event_buffer_capacity = 4096;
            cfg.common.connector = Some(TransportConnectorConfig {
                mode: TransportMode::Base,
                connector: Arc::new(SlowRetry {
                    calls: calls.clone(),
                    peer: peer.closer(),
                    completion: peer_diagnostics.clone(),
                }),
            });
            storms.push(NativeClient::start_in(cfg, &execution).unwrap());
        }
        let busy = NativeClient::start_in(config(mqtt5), &execution).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let flood = {
            let handle = busy.handle();
            let stop = stop.clone();
            let requests = requests.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let result = handle.try_admit(Command::Diagnostics).unwrap();
                    result.completion.wait_timeout(DEADLINE).unwrap();
                    requests.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        let started = std::time::Instant::now();
        while calls.load(Ordering::Relaxed) < 8 {
            assert!(
                started.elapsed() < DEADLINE,
                "reconnect storm did not make progress"
            );
            thread::sleep(Duration::from_millis(1));
        }
        for _ in 0..32 {
            assert_eq!(
                terminal(&publish(&peer, b"peer")).unwrap(),
                Completion::Publish(PublishCompletion::Qos1Acknowledged)
            );
        }
        stop.store(true, Ordering::Relaxed);
        flood.join().unwrap();
        assert!(requests.load(Ordering::Relaxed) > 0);
        assert!(calls.load(Ordering::Relaxed) >= 8);
        peer.closer().close(DEADLINE).unwrap();
        execution.request_shutdown();
        execution.join(DEADLINE).unwrap();
        drop(storms);
        broker.join();
    }
}
