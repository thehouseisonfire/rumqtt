mod support;

use std::io::Write;
use std::net::TcpListener;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use rumqttc_wrapper_core::*;
use support::*;

// Occupy the sole shared worker so admission deterministically precedes the
// target driver's first poll. Blocking here is only a scheduling test fixture.
struct WorkerGate {
    entered: mpsc::Sender<()>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl TransportConnector for WorkerGate {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        let release = self.release.lock().unwrap().take();
        let entered = self.entered.clone();
        Box::pin(async move {
            if let Some(release) = release {
                entered.send(()).unwrap();
                release.recv_timeout(DEADLINE).unwrap();
            }
            Err(TransportFailure::Connect)
        })
    }
}

fn no_retries(config: &mut ClientConfig) {
    config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
        initial_delay: Duration::ZERO,
        maximum_delay: Duration::ZERO,
        budget: RetryBudget::Limited(0),
        ..ReconnectConfig::default()
    });
}

#[test]
fn shutdown_admitted_before_first_poll_drains_graceful_work_but_immediate_close_does_not_dial() {
    for mqtt5 in [false, true] {
        for classified in [false, true] {
            for graceful in [false, true] {
                let execution = ExecutionContext::new(ExecutionOptions {
                    client_capacity: 2,
                    worker_threads: 1,
                    max_blocking_threads: 2,
                })
                .unwrap();
                let (entered_tx, entered) = mpsc::channel();
                let (release, release_rx) = mpsc::channel();
                let mut blocker_config = config(mqtt5, 1883);
                no_retries(&mut blocker_config);
                blocker_config.common.connector = Some(TransportConnectorConfig {
                    connector: Arc::new(WorkerGate {
                        entered: entered_tx,
                        release: Mutex::new(Some(release_rx)),
                    }),
                    mode: TransportMode::Base,
                });
                let blocker = NativeClient::start_in(blocker_config, &execution).unwrap();
                entered.recv_timeout(DEADLINE).unwrap();

                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let mut target_config = config(mqtt5, listener.local_addr().unwrap().port());
                if classified {
                    no_retries(&mut target_config);
                }
                let target = NativeClient::start_in(target_config, &execution).unwrap();
                assert_eq!(target.handle().reconnect_diagnostics().cycles_started, 0);
                let subscribe = target
                    .handle()
                    .try_admit(Command::Subscribe(SubscribeCommand {
                        filters: vec![Subscription {
                            filter: "queued".into(),
                            qos: QoS::AtLeastOnce,
                            protocol: SubscriptionProtocolOptions::VersionNeutral,
                        }],
                        protocol: SubscribeProtocolOptions::VersionNeutral,
                    }))
                    .unwrap();
                let close = target
                    .handle()
                    .try_admit(if graceful {
                        Command::GracefulDisconnect {
                            timeout: Some(DEADLINE),
                        }
                    } else {
                        Command::ImmediateDisconnect
                    })
                    .unwrap();
                assert_eq!(target.handle().reconnect_diagnostics().cycles_started, 0);

                let broker = graceful.then(|| {
                    let listener = listener.try_clone().unwrap();
                    Broker::spawn(move || {
                        let mut socket = accept(&listener);
                        connect(&mut socket, mqtt5);
                        let packet = frame(&mut socket);
                        assert_eq!(packet[0], 0x82);
                        let [high, low] = [packet[2], packet[3]];
                        if mqtt5 {
                            socket.write_all(&[0x90, 4, high, low, 0, 1]).unwrap();
                        } else {
                            socket.write_all(&[0x90, 3, high, low, 1]).unwrap();
                        }
                        assert_eq!(frame(&mut socket)[0], 0xe0);
                    })
                });
                release.send(()).unwrap();
                if graceful {
                    assert!(matches!(terminal(&subscribe), Ok(Completion::Subscribe(_))));
                    assert_eq!(terminal(&close).unwrap(), Completion::GracefulShutdown);
                } else {
                    assert!(terminal(&subscribe).is_err());
                    assert_eq!(terminal(&close).unwrap(), Completion::ImmediateShutdown);
                }
                target.join(DEADLINE).unwrap();
                let snapshot = target.handle().reconnect_diagnostics();
                assert_eq!(snapshot.cycles_started, u64::from(graceful));
                assert_eq!(snapshot.retries_since_reset, 0);
                assert_eq!(snapshot.stop_reason, ReconnectStopReason::Shutdown);
                if let Some(broker) = broker {
                    broker.join();
                } else {
                    listener.set_nonblocking(true).unwrap();
                    assert_eq!(
                        listener.accept().unwrap_err().kind(),
                        std::io::ErrorKind::WouldBlock
                    );
                }
                blocker.join(DEADLINE).unwrap();
                execution.request_shutdown();
                execution.join(DEADLINE).unwrap();
            }
        }
    }
}
