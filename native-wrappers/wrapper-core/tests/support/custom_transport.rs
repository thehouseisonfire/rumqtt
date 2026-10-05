use bytes::Bytes;
use rumqttc_wrapper_core::{
    NetworkHandling, TransportConnection, TransportConnector, TransportConnectorConfig,
    TransportFailure, TransportFuture, TransportIo, TransportIoFuture, TransportMode,
    TransportRequest,
};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct TcpConnector {
    pub requests: Arc<Mutex<Vec<TransportRequest>>>,
    pub target_override: Option<String>,
    pub write_chunk: usize,
}
impl TransportConnector for TcpConnector {
    fn connect(&self, request: TransportRequest) -> TransportFuture {
        self.requests.lock().unwrap().push(request.clone());
        let target = self.target_override.clone().unwrap_or(request.target);
        let network = request.network;
        let write_chunk = self.write_chunk;
        Box::pin(async move {
            // This fixture deliberately rejects settings instead of pretending
            // that Tokio's simple dialer applied them.
            if network != rumqttc_wrapper_core::NetworkConfig::default() {
                return Err(TransportFailure::NetworkOptions);
            }
            let socket = tokio::net::TcpStream::connect(target)
                .await
                .map_err(|_| TransportFailure::Connect)?;
            let (read, write) = socket.into_split();
            Ok(TransportConnection {
                io: Arc::new(TcpIo {
                    read: Arc::new(tokio::sync::Mutex::new(read)),
                    write: Arc::new(tokio::sync::Mutex::new(write)),
                    write_chunk,
                }),
                mode: TransportMode::Base,
                network_handling: NetworkHandling::NotApplicable,
            })
        })
    }
}
struct TcpIo {
    write_chunk: usize,
    read: Arc<tokio::sync::Mutex<tokio::net::tcp::OwnedReadHalf>>,
    write: Arc<tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>>,
}
impl TransportIo for TcpIo {
    fn read(&self, max: usize) -> TransportIoFuture<Bytes> {
        let read = self.read.clone();
        Box::pin(async move {
            let mut bytes = vec![0; max.min(3)];
            let count = read.lock().await.read(&mut bytes).await?;
            bytes.truncate(count);
            Ok(Bytes::from(bytes))
        })
    }
    fn write(&self, bytes: Bytes) -> TransportIoFuture<usize> {
        let write = self.write.clone();
        let write_chunk = self.write_chunk;
        Box::pin(async move {
            write
                .lock()
                .await
                .write(&bytes[..bytes.len().min(write_chunk)])
                .await
        })
    }
    fn flush(&self) -> TransportIoFuture<()> {
        let write = self.write.clone();
        Box::pin(async move { write.lock().await.flush().await })
    }
    fn shutdown(&self) -> TransportIoFuture<()> {
        let write = self.write.clone();
        Box::pin(async move { write.lock().await.shutdown().await })
    }
}

#[allow(dead_code)]
pub fn configured() -> (TransportConnectorConfig, Arc<Mutex<Vec<TransportRequest>>>) {
    configured_with_write_chunk(3)
}

#[allow(dead_code)]
pub fn configured_with_write_chunk(
    write_chunk: usize,
) -> (TransportConnectorConfig, Arc<Mutex<Vec<TransportRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    (
        TransportConnectorConfig {
            connector: Arc::new(TcpConnector {
                requests: requests.clone(),
                target_override: None,
                write_chunk,
            }),
            mode: TransportMode::Base,
        },
        requests,
    )
}
