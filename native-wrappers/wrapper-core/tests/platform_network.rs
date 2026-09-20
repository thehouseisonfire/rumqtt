mod support;

use rumqttc_wrapper_core::*;
use support::*;

#[test]
fn bind_address_reaches_broker_for_both_protocols() {
    for mqtt5 in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.network.local_address = Some("127.0.0.1:0".parse().unwrap());
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            assert_eq!(
                socket.peer_addr().unwrap().ip(),
                std::net::Ipv4Addr::LOCALHOST
            );
            connect(&mut socket, mqtt5);
            assert_eq!(frame(&mut socket)[0], 0xe0);
        });
        let mut client = NativeClient::start(config).unwrap();
        let _events = connected(&mut client);
        client.closer().close(DEADLINE).unwrap();
        broker.join();
    }
}

#[test]
fn unsupported_network_controls_fail_before_driver_start() {
    for mqtt5 in [false, true] {
        if !cfg!(target_os = "linux") {
            let mut config = config(mqtt5, 1);
            config.common.network.mptcp = true;
            assert_eq!(
                NativeClient::start(config).unwrap_err().kind(),
                ErrorKind::Configuration
            );
        }
        if !cfg!(any(
            target_os = "linux",
            target_os = "android",
            target_os = "fuchsia"
        )) {
            let mut config = config(mqtt5, 1);
            config.common.network.bind_device = Some("device".into());
            assert_eq!(
                NativeClient::start(config).unwrap_err().kind(),
                ErrorKind::Configuration
            );
        }
        if !cfg!(unix) {
            let mut config = config(mqtt5, 1);
            config.common.broker = BrokerTarget::Unix {
                path: "unsupported.sock".into(),
            };
            config.common.transport = TransportConfig::Unix;
            assert_eq!(
                NativeClient::start(config).unwrap_err().kind(),
                ErrorKind::Configuration
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn bind_device_failure_reaches_socket_and_mptcp_can_connect() {
    for mqtt5 in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut invalid = config(mqtt5, listener.local_addr().unwrap().port());
        invalid.common.network.bind_device = Some("rmq-no-device".into());
        let mut client = NativeClient::start(invalid).unwrap();
        let mut events = client.take_events().unwrap();
        let event = until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        });
        let WrapperEvent::Disconnected { error, .. } = event else {
            unreachable!()
        };
        assert_eq!(error.kind(), ErrorKind::Network);
        client.closer().close_now(DEADLINE).unwrap();
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );

        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.network.mptcp = true;
        config.common.network.local_address = Some("127.0.0.1:0".parse().unwrap());
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            assert_eq!(frame(&mut socket)[0], 0xe0);
        });
        let mut client = NativeClient::start(config).unwrap();
        let _events = connected(&mut client);
        client.closer().close(DEADLINE).unwrap();
        broker.join();
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::time::{Duration, Instant};

    fn accept_unix(listener: &UnixListener) -> UnixStream {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + DEADLINE;
        loop {
            match listener.accept() {
                Ok((socket, _)) => {
                    socket.set_read_timeout(Some(DEADLINE)).unwrap();
                    socket.set_write_timeout(Some(DEADLINE)).unwrap();
                    return socket;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("Unix accept: {error}"),
            }
        }
    }

    fn unix_config(mqtt5: bool, path: &std::path::Path) -> ClientConfig {
        let mut config = config(mqtt5, 1);
        config.common.broker = BrokerTarget::Unix {
            path: path.to_str().unwrap().into(),
        };
        config.common.transport = TransportConfig::Unix;
        config
    }

    #[test]
    fn unix_reconnect_and_both_shutdown_modes() {
        for mqtt5 in [false, true] {
            for immediate in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("mqtt.sock");
                let listener = UnixListener::bind(&path).unwrap();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let broker = Broker::spawn(move || {
                    for generation in 0..2 {
                        let mut socket = accept_unix(&listener);
                        assert_eq!(frame(&mut socket)[0], 0x10);
                        socket
                            .write_all(if mqtt5 {
                                &[0x20, 3, 0, 0, 0]
                            } else {
                                &[0x20, 2, 0, 0]
                            })
                            .unwrap();
                        if generation == 0 {
                            release_rx.recv_timeout(DEADLINE).unwrap();
                        } else {
                            assert_eq!(frame(&mut socket)[0], 0xe0);
                        }
                    }
                });
                let mut client = NativeClient::start(unix_config(mqtt5, &path)).unwrap();
                let mut events = connected(&mut client);
                release_tx.send(()).unwrap();
                until(&mut events, |event| {
                    matches!(event, WrapperEvent::Disconnected { .. })
                });
                until(&mut events, |event| {
                    matches!(event, WrapperEvent::Connected { .. })
                });
                if immediate {
                    client.closer().close_now(DEADLINE).unwrap();
                } else {
                    client.closer().close(DEADLINE).unwrap();
                }
                broker.join();
            }
        }
    }

    #[test]
    fn unix_missing_path_is_a_network_failure() {
        let directory = tempfile::tempdir().unwrap();
        for mqtt5 in [false, true] {
            assert_network_failure(unix_config(mqtt5, &directory.path().join("missing.sock")));
        }
    }

    fn assert_network_failure(config: ClientConfig) {
        let mut client = NativeClient::start(config).unwrap();
        let mut events = client.take_events().unwrap();
        let event = until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        });
        let WrapperEvent::Disconnected { error, .. } = event else {
            unreachable!()
        };
        assert_eq!(error.kind(), ErrorKind::Network);
        client.closer().close_now(DEADLINE).unwrap();
    }

    #[test]
    fn unix_permission_denial_is_a_network_failure() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::process::CommandExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mqtt.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o0)).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "unix::permission_child", "--nocapture"])
            .env("RUMQTTC_PERMISSION_SOCKET", &path);
        let uid = std::process::Command::new("id").arg("-u").output().unwrap();
        if uid.stdout == b"0\n" {
            child.uid(65534).gid(65534);
        }
        let output = process_output(&mut child);
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            output.status.success(),
            "permission subprocess failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn permission_child() {
        let Some(path) = std::env::var_os("RUMQTTC_PERMISSION_SOCKET") else {
            return;
        };
        for mqtt5 in [false, true] {
            assert_network_failure(unix_config(mqtt5, std::path::Path::new(&path)));
        }
    }

    #[test]
    fn unix_connack_timeout_and_immediate_shutdown_are_bounded() {
        for mqtt5 in [false, true] {
            for timeout in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("mqtt.sock");
                let listener = UnixListener::bind(&path).unwrap();
                let (ready_tx, ready_rx) = std::sync::mpsc::channel();
                let broker = Broker::spawn(move || {
                    let mut socket = accept_unix(&listener);
                    assert_eq!(frame(&mut socket)[0], 0x10);
                    ready_tx.send(()).unwrap();
                    assert_eq!(socket.read(&mut [0]).unwrap(), 0);
                });
                let mut config = unix_config(mqtt5, &path);
                config.common.connection_timeout = Duration::from_secs(1);
                let mut client = NativeClient::start(config).unwrap();
                let mut events = client.take_events().unwrap();
                ready_rx.recv_timeout(DEADLINE).unwrap();
                if timeout {
                    let event = until(&mut events, |event| {
                        matches!(event, WrapperEvent::Disconnected { .. })
                    });
                    let WrapperEvent::Disconnected { error, .. } = event else {
                        unreachable!()
                    };
                    assert_eq!(error.kind(), ErrorKind::Timeout);
                }
                client.closer().close_now(DEADLINE).unwrap();
                broker.join();
            }
        }
    }
}
