#![cfg(feature = "tracing")]
mod support;

use rumqttc_wrapper_core::*;
use std::{cell::RefCell, net::TcpListener, sync::mpsc, thread};

struct HostClient(Option<NativeClient>);
impl Drop for HostClient {
    fn drop(&mut self) {
        // Both routing slots were initialized after this host slot and have already
        // been destroyed. Lookup, scope entry/restoration and client cleanup must work.
        assert!(rumqttc_core::native_tracing::current().is_none());
        rumqttc_core::native_tracing::with_dispatch(None, || drop(self.0.take()));
    }
}
thread_local! {
    static CLIENT: RefCell<Option<HostClient>> = const { RefCell::new(None) };
}
struct Sink;
impl LogSink for Sink {
    fn record(&self, _: &LogRecord) -> bool {
        true
    }
}

#[test]
fn clients_in_host_thread_local_storage_finalize_after_logging_tls() {
    let output = support::process_output(
        std::process::Command::new(std::env::current_exe().unwrap()).args([
            "--exact",
            "host_thread_local_finalization_child",
            "--ignored",
            "--nocapture",
        ]),
    );
    assert!(
        output.status.success(),
        "host TLS finalization subprocess failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "runs in a subprocess because a TLS destructor panic aborts the process"]
fn host_thread_local_finalization_child() {
    for mqtt5 in [false, true] {
        for shared in [false, true] {
            for attached in [false, true] {
                let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
                let config = support::config(mqtt5, listener.local_addr().unwrap().port());
                let execution =
                    shared.then(|| ExecutionContext::new(ExecutionOptions::default()).unwrap());
                let context =
                    attached.then(|| LoggingContext::new(LoggingOptions::default()).unwrap());
                let registration = context.as_ref().map(|context| {
                    context
                        .register_with(LogFilter::default(), || Box::new(Sink))
                        .unwrap()
                });
                let (send, receive) = mpsc::channel();
                let placement = execution.clone();
                thread::spawn(move || {
                    CLIENT.with(|slot| {
                        let client = if let Some(execution) = &placement {
                            NativeClient::start_in_with_logging(config, execution, registration)
                        } else {
                            NativeClient::start_with_logging(config, registration)
                        }
                        .unwrap();
                        send.send(client.closer()).unwrap();
                        *slot.borrow_mut() = Some(HostClient(Some(client)));
                    });
                })
                .join()
                .unwrap();
                let closer = receive.recv_timeout(support::DEADLINE).unwrap();
                closer.close_now(support::DEADLINE).unwrap();
                drop(closer);
                if let Some(execution) = execution {
                    execution.request_shutdown();
                    execution.join(support::DEADLINE).unwrap();
                }
                if let Some(context) = context {
                    context.request_shutdown();
                    assert!(context.join(support::DEADLINE).unwrap());
                }
            }
        }
    }
}
