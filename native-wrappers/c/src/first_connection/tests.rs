use super::*;
use rumqttc_wrapper_core as core;
use std::sync::Arc;

struct PendingConnector;
impl core::TransportConnector for PendingConnector {
    fn connect(&self, _: core::TransportRequest) -> core::TransportFuture {
        Box::pin(std::future::pending())
    }
}

#[test]
fn observation_validates_outputs_and_distinguishes_pending_from_terminal_failure() {
    unsafe {
        let mut out = ptr::dangling_mut();
        let mut error = ptr::dangling_mut();
        assert_eq!(
            rumqttc_client_first_connection(ptr::null(), &raw mut out, &raw mut error),
            1
        );
        assert!(out.is_null());
        assert!(!error.is_null());
        super::super::rumqttc_error_destroy(error);
        assert_eq!(
            rumqttc_first_connection_retain(ptr::null(), &raw mut out, ptr::null_mut()),
            1
        );
        assert!(out.is_null());
        let (mut state, mut protocol, mut session) = (99, 99, 99);
        assert_eq!(
            rumqttc_first_connection_poll(
                ptr::null(),
                &raw mut state,
                &raw mut protocol,
                &raw mut session,
                ptr::null_mut()
            ),
            1
        );
        assert_eq!((state, protocol, session), (0, 0, 0));
        rumqttc_first_connection_release(ptr::null_mut());
    }
    for mqtt5 in [false, true] {
        let mut config = if mqtt5 {
            core::ClientConfig::v5("observer", "localhost", 1883)
        } else {
            core::ClientConfig::v4("observer", "localhost", 1883)
        };
        config.common.connector = Some(core::TransportConnectorConfig {
            connector: Arc::new(PendingConnector),
            mode: core::TransportMode::Base,
        });
        let native = core::NativeClient::start(config).unwrap();
        let observer = rumqttc_first_connection {
            inner: native.connection(),
        };
        for finished in [false, true] {
            if finished {
                native.closer().close_now(Duration::from_secs(5)).unwrap();
            }
            let mut state = 99;
            assert_eq!(
                unsafe {
                    rumqttc_first_connection_poll(
                        &raw const observer,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                    )
                },
                1
            );
            assert_eq!(
                unsafe {
                    rumqttc_first_connection_poll(
                        &raw const observer,
                        &raw mut state,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                    )
                },
                if finished { 6 } else { 11 }
            );
            assert_eq!(state, if finished { 3 } else { 1 });
            let (mut state, mut protocol, mut session) = (99, 99, 99);
            assert_eq!(
                unsafe {
                    rumqttc_first_connection_wait_timeout_ms(
                        &raw const observer,
                        0,
                        &raw mut state,
                        &raw mut protocol,
                        &raw mut session,
                        ptr::null_mut(),
                    )
                },
                if finished { 6 } else { 5 }
            );
            assert_eq!(
                (state, protocol, session),
                (if finished { 3 } else { 1 }, 0, 0)
            );
        }
    }
}

struct ProbeConnector {
    observer: ConnectionHandle,
    checked: std::sync::mpsc::Sender<()>,
    finished: bool,
}
impl core::TransportConnector for ProbeConnector {
    fn connect(&self, _: core::TransportRequest) -> core::TransportFuture {
        let observer = rumqttc_first_connection {
            inner: self.observer.clone(),
        };
        let (mut state, mut protocol, mut session) = (99, 99, 99);
        assert!(!core::blocking_wait_allowed());
        assert_eq!(
            unsafe {
                rumqttc_first_connection_wait_timeout_ms(
                    &raw const observer,
                    1,
                    &raw mut state,
                    &raw mut protocol,
                    &raw mut session,
                    ptr::null_mut(),
                )
            },
            2
        );
        assert_eq!((state, protocol, session), (0, 0, 0));
        assert_eq!(
            unsafe {
                rumqttc_first_connection_wait_timeout_ms(
                    &raw const observer,
                    0,
                    &raw mut state,
                    &raw mut protocol,
                    &raw mut session,
                    ptr::null_mut(),
                )
            },
            if self.finished { 6 } else { 5 }
        );
        assert_eq!(
            (state, protocol, session),
            (if self.finished { 3 } else { 1 }, 0, 0)
        );
        self.checked.send(()).unwrap();
        Box::pin(std::future::pending())
    }
}

#[test]
fn native_transport_callbacks_cannot_block_on_pending_or_completed_observers() {
    let mut config = core::ClientConfig::v4("pending", "localhost", 1883);
    config.common.connector = Some(core::TransportConnectorConfig {
        connector: Arc::new(PendingConnector),
        mode: core::TransportMode::Base,
    });
    let target = core::NativeClient::start(config).unwrap();
    for finished in [false, true] {
        if finished {
            target.closer().close_now(Duration::from_secs(5)).unwrap();
        }
        let (checked, received) = std::sync::mpsc::channel();
        let mut config = core::ClientConfig::v4("probe", "localhost", 1883);
        config.common.connector = Some(core::TransportConnectorConfig {
            connector: Arc::new(ProbeConnector {
                observer: target.connection(),
                checked,
                finished,
            }),
            mode: core::TransportMode::Base,
        });
        let probe = core::NativeClient::start(config).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        probe.closer().close_now(Duration::from_secs(5)).unwrap();
    }
}
