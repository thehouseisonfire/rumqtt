use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::FutureExt;

use crate::transport::{HostFuture, OwnedStream};
use crate::{
    CommonConfig, NetworkConfig, NetworkHandling, ProtocolVersion, TransportConnectorConfig,
    TransportFailure, TransportRequest,
};

pub fn io_error(kind: std::io::ErrorKind, failure: TransportFailure) -> std::io::Error {
    let error = std::io::Error::new(kind, failure);
    if failure.retryable() {
        error
    } else {
        rumqttc_v5::TerminalTransportError::new(error).into_io()
    }
}

pub fn io_failure(error: &std::io::Error) -> Option<TransportFailure> {
    let inner = error.get_ref()?;
    if let Some(failure) = inner.downcast_ref::<TransportFailure>() {
        return Some(*failure);
    }
    // Inspect only our known containers while sanitizing host I/O errors.
    // Calling arbitrary host Error::source() methods here could panic/block.
    inner
        .downcast_ref::<rumqttc_v5::TerminalTransportError>()
        .and_then(|terminal| io_failure(terminal.get_ref()))
}

fn network(options: &rumqttc_v4::NetworkOptions) -> NetworkConfig {
    NetworkConfig {
        tcp_send_buffer_size: options.tcp_send_buffer_size(),
        tcp_receive_buffer_size: options.tcp_recv_buffer_size(),
        tcp_nodelay: options.tcp_nodelay(),
        local_address: options.bind_addr(),
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        bind_device: options.bind_device().map(str::to_owned),
        #[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
        bind_device: None,
        #[cfg(target_os = "linux")]
        mptcp: options.mptcp(),
        #[cfg(not(target_os = "linux"))]
        mptcp: false,
    }
}

async fn connect(
    config: TransportConnectorConfig,
    request: TransportRequest,
) -> std::io::Result<OwnedStream> {
    if std::time::Instant::now() >= request.deadline {
        return Err(TransportFailure::Timeout.into_io());
    }
    let deadline = tokio::time::Instant::from_std(request.deadline);
    let mode = request.mode;
    let network = request.network.clone();
    let work = async {
        let future = crate::runtime::with_host_callback(|| config.connector.connect(request));
        HostFuture::new(future, std::convert::identity).await
    };
    let result = tokio::time::timeout_at(deadline, AssertUnwindSafe(work).catch_unwind()).await;
    let connection = match result {
        Err(_) => return Err(TransportFailure::Timeout.into_io()),
        Ok(Err(payload)) => {
            std::mem::forget(payload);
            return Err(TransportFailure::Panic.into_io());
        }
        Ok(Ok(result)) => result.map_err(TransportFailure::into_io)?,
    };
    // A host can return Ready after spending the budget inside its callback.
    // Tokio's timeout polls ready work first, so check the boundary explicitly.
    if tokio::time::Instant::now() >= deadline {
        return Err(TransportFailure::Timeout.into_io());
    }
    if connection.mode != mode {
        return Err(TransportFailure::Composition.into_io());
    }
    if connection.network_handling == NetworkHandling::NotApplicable
        && network != NetworkConfig::default()
    {
        return Err(TransportFailure::NetworkOptions.into_io());
    }
    Ok(OwnedStream::new(connection.io))
}

#[allow(
    deprecated,
    reason = "Atomic fetch_update is available at the Rust 1.88 MSRV"
)]
fn request(
    common: &CommonConfig,
    protocol: ProtocolVersion,
    target: String,
    options: &rumqttc_v4::NetworkOptions,
    generation: &AtomicU64,
) -> std::io::Result<TransportRequest> {
    let generation = generation
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| TransportFailure::ResourceLimit.into_io())?
        + 1;
    Ok(TransportRequest {
        protocol,
        client_id: common.client_id.clone(),
        target,
        generation,
        deadline: options
            .connection_deadline()
            .ok_or_else(|| TransportFailure::InvalidResult.into_io())?,
        network: network(options),
        mode: common
            .connector
            .as_ref()
            .expect("configured connector")
            .mode,
    })
}

pub(super) fn configure_v4(options: &mut rumqttc_v4::MqttOptions, common: &CommonConfig) {
    let Some(config) = common.connector.clone() else {
        return;
    };
    let common = common.clone();
    let generation = Arc::new(AtomicU64::new(0));
    options.set_socket_connector(move |target, options| {
        let config = config.clone();
        let request = request(&common, ProtocolVersion::V4, target, &options, &generation);
        async move { connect(config, request?).await }
    });
}

pub(super) fn configure_v5(options: &mut rumqttc_v5::MqttOptions, common: &CommonConfig) {
    let Some(config) = common.connector.clone() else {
        return;
    };
    let common = common.clone();
    let generation = Arc::new(AtomicU64::new(0));
    options.set_socket_connector(move |target, options| {
        let config = config.clone();
        let request = request(&common, ProtocolVersion::V5, target, &options, &generation);
        async move { connect(config, request?).await }
    });
}

pub(super) fn failure(error: &(dyn std::error::Error + 'static)) -> Option<TransportFailure> {
    // Traverse typed sources only; never format a host-provided error.
    if let Some(failure) = error.downcast_ref::<TransportFailure>() {
        return Some(*failure);
    }
    if let Some(error) = error.downcast_ref::<std::io::Error>()
        && let Some(inner) = error.get_ref()
        && let Some(failure) = failure(inner)
    {
        return Some(failure);
    }
    error.source().and_then(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        TransportConnection, TransportFuture, TransportIo, TransportIoFuture, TransportMode,
    };
    use bytes::Bytes;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    struct Io;
    impl TransportIo for Io {
        fn read(&self, _: usize) -> TransportIoFuture<Bytes> {
            Box::pin(async { Ok(Bytes::new()) })
        }
        fn write(&self, bytes: Bytes) -> TransportIoFuture<usize> {
            Box::pin(async move { Ok(bytes.len()) })
        }
        fn flush(&self) -> TransportIoFuture<()> {
            Box::pin(async { Ok(()) })
        }
        fn shutdown(&self) -> TransportIoFuture<()> {
            self.flush()
        }
    }
    struct Host {
        calls: Arc<AtomicUsize>,
        blocking: bool,
    }
    impl crate::TransportConnector for Host {
        fn connect(&self, _: TransportRequest) -> TransportFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.blocking {
                std::thread::sleep(Duration::from_millis(30));
            }
            Box::pin(async {
                Ok(TransportConnection {
                    io: Arc::new(Io),
                    mode: TransportMode::Base,
                    network_handling: NetworkHandling::NotApplicable,
                })
            })
        }
    }
    #[tokio::test]
    async fn expired_attempts_do_not_invoke_hosts_or_accept_immediately_ready_late_results() {
        for blocking in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let request = TransportRequest {
                protocol: ProtocolVersion::V4,
                client_id: "deadline".into(),
                target: "memory:1883".into(),
                generation: 1,
                deadline: if blocking {
                    Instant::now() + Duration::from_millis(10)
                } else {
                    Instant::now()
                },
                network: NetworkConfig::default(),
                mode: TransportMode::Base,
            };
            let result = connect(
                TransportConnectorConfig {
                    connector: Arc::new(Host {
                        calls: calls.clone(),
                        blocking,
                    }),
                    mode: TransportMode::Base,
                },
                request,
            )
            .await;
            let Err(error) = result else {
                panic!("late connection accepted")
            };
            assert_eq!(failure(&error), Some(TransportFailure::Timeout));
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(blocking));
        }
    }
}
