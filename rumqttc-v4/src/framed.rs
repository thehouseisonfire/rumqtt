use futures_util::{FutureExt, SinkExt, StreamExt, TryFutureExt};
pub use rumqttc_core::AsyncReadWrite;
use std::{future::Future, io};
use tokio::time::{self, Instant};
use tokio_util::codec::Framed;

use crate::mqttbytes::{
    self,
    v4::{Codec, Packet},
};
use crate::notice::DeferredNotice;
use crate::{Incoming, MqttState, StateError};

/// Network transforms packets <-> frames efficiently. It takes
/// advantage of pre-allocation, buffering and vectorization when
/// appropriate to achieve performance
pub struct Network {
    /// Frame MQTT packets from network connection
    framed: Framed<Box<dyn AsyncReadWrite>, Codec>,
    write_deadline: Option<Instant>,
}

pub type IncomingFrame = Option<Result<Packet, mqttbytes::Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadBatchOutcome {
    NoResponseWritten,
    ResponseWritten,
}

#[derive(Debug)]
pub struct ReadBatch {
    pub(crate) outcome: ReadBatchOutcome,
    pub(crate) notices: Vec<DeferredNotice>,
}

#[derive(Debug)]
pub struct ReadBatchError {
    pub(crate) source: StateError,
    pub(crate) batch: ReadBatch,
}

impl ReadBatchError {
    const fn new(
        source: StateError,
        outcome: ReadBatchOutcome,
        notices: Vec<DeferredNotice>,
    ) -> Self {
        Self {
            source,
            batch: ReadBatch { outcome, notices },
        }
    }
}

impl Network {
    pub fn new(
        socket: impl AsyncReadWrite + 'static,
        max_incoming_size: usize,
        max_outgoing_size: usize,
    ) -> Self {
        let socket: Box<dyn AsyncReadWrite> = Box::new(socket);
        let codec = Codec {
            max_incoming_size,
            max_outgoing_size,
        };
        let framed = Framed::new(socket, codec);

        Self {
            framed,
            write_deadline: None,
        }
    }

    /// Wait for one frame for arbitration. Protocol state and replies are
    /// processed only after this read wins selection.
    pub(crate) fn read_frame(&mut self) -> impl Future<Output = IncomingFrame> + '_ {
        self.framed.next()
    }

    /// Reads and returns a single packet from network
    pub async fn read(&mut self) -> Result<Incoming, StateError> {
        match self.read_frame().await {
            Some(Ok(packet)) => Ok(packet),
            Some(Err(mqttbytes::Error::InsufficientBytes(_))) => unreachable!(),
            Some(Err(e)) => Err(StateError::Deserialization(e)),
            None => Err(StateError::ConnectionAborted),
        }
    }

    /// Read packets in bulk. This allow replies to be in bulk. This method is used
    /// after the connection is established to read a bunch of incoming packets
    #[cfg(test)]
    pub(crate) async fn readb(
        &mut self,
        state: &mut MqttState,
        read_batch_limit: usize,
    ) -> Result<ReadBatch, ReadBatchError> {
        let first = self.read_frame().await;
        self.readb_from(first, state, read_batch_limit).await
    }

    /// Poll only the first frame without waiting. Once a frame is available,
    /// finish its state changes and writes without cancelling batch processing.
    #[cfg(test)]
    pub(crate) async fn try_readb(
        &mut self,
        state: &mut MqttState,
        read_batch_limit: usize,
    ) -> Option<Result<ReadBatch, ReadBatchError>> {
        let first = self.read_frame().now_or_never()?;
        Some(self.readb_from(first, state, read_batch_limit).await)
    }

    pub(crate) async fn readb_from(
        &mut self,
        mut res: IncomingFrame,
        state: &mut MqttState,
        read_batch_limit: usize,
    ) -> Result<ReadBatch, ReadBatchError> {
        let read_batch_limit = read_batch_limit.max(1);
        let mut count = 0;
        let mut outcome = ReadBatchOutcome::NoResponseWritten;
        let mut notices = Vec::new();
        loop {
            match res {
                Some(Ok(packet)) => {
                    let effects = match state.handle_incoming_packet_with_effects(packet) {
                        Ok(effects) => effects,
                        Err(err) => return Err(ReadBatchError::new(err, outcome, notices)),
                    };
                    let has_deferred_notices = !effects.notices.is_empty();
                    notices.extend(effects.notices);
                    if let Some(outgoing) = effects.outgoing {
                        if let Err(err) = self.write(outgoing).await {
                            return Err(ReadBatchError::new(err, outcome, notices));
                        }
                        outcome = ReadBatchOutcome::ResponseWritten;
                    }
                    if has_deferred_notices {
                        break;
                    }

                    count += 1;
                    if count >= read_batch_limit {
                        break;
                    }
                }
                Some(Err(mqttbytes::Error::InsufficientBytes(_))) => unreachable!(),
                Some(Err(e)) => {
                    return Err(ReadBatchError::new(
                        StateError::Deserialization(e),
                        outcome,
                        notices,
                    ));
                }
                None => {
                    return Err(ReadBatchError::new(
                        StateError::ConnectionAborted,
                        outcome,
                        notices,
                    ));
                }
            }
            // do not wait for subsequent reads
            match self.framed.next().now_or_never() {
                Some(r) => res = r,
                _ => break,
            }
        }

        Ok(ReadBatch { outcome, notices })
    }

    /// Replace the response deadline, returning the previous value for restoration.
    pub(crate) const fn replace_write_deadline(
        &mut self,
        deadline: Option<Instant>,
    ) -> Option<Instant> {
        std::mem::replace(&mut self.write_deadline, deadline)
    }

    /// Feed an admitted outbound request outside a protocol response batch.
    pub(crate) fn write_request(
        &mut self,
        packet: Packet,
    ) -> impl Future<Output = Result<(), StateError>> + '_ {
        self.framed
            .feed(packet)
            .map_err(StateError::Deserialization)
    }

    /// Flush outbound requests; the event loop owns their timeout policy.
    pub(crate) fn flush_request(&mut self) -> impl Future<Output = Result<(), StateError>> + '_ {
        self.framed.flush().map_err(StateError::Deserialization)
    }

    async fn await_write(
        deadline: Option<Instant>,
        write: impl Future<Output = Result<(), mqttbytes::Error>>,
    ) -> Result<(), StateError> {
        let result = if let Some(deadline) = deadline {
            tokio::pin!(write);
            // Most response writes and empty flushes are immediately ready.
            // Only construct a timer when the transport actually blocks, keeping
            // the batch's original absolute deadline across every I/O await.
            match std::future::poll_fn(|cx| std::task::Poll::Ready(write.as_mut().poll(cx))).await {
                std::task::Poll::Ready(result) => result,
                std::task::Poll::Pending => {
                    time::timeout_at(deadline, write).await.map_err(|_| {
                        StateError::Io(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "network response write timed out",
                        ))
                    })?
                }
            }
        } else {
            write.await
        };
        result.map_err(StateError::Deserialization)
    }

    /// Serializes packet into write buffer
    pub fn write(&mut self, packet: Packet) -> impl Future<Output = Result<(), StateError>> + '_ {
        Self::await_write(self.write_deadline, self.framed.feed(packet))
    }

    pub fn flush(&mut self) -> impl Future<Output = Result<(), StateError>> + '_ {
        Self::await_write(self.write_deadline, self.framed.flush())
    }

    #[cfg(test)]
    pub(crate) fn set_backpressure_boundary(&mut self, boundary: usize) {
        self.framed.set_backpressure_boundary(boundary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, duplex};

    #[tokio::test]
    async fn cancelled_flush_retries_the_transport_after_the_frame_buffer_drains() {
        use std::{
            pin::Pin,
            sync::{
                Arc,
                atomic::{AtomicBool, AtomicUsize, Ordering},
            },
            task::{Context, Poll},
        };
        use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, DuplexStream, ReadBuf};

        struct FlushGate {
            socket: DuplexStream,
            ready: Arc<AtomicBool>,
            polls: Arc<AtomicUsize>,
        }
        impl AsyncRead for FlushGate {
            fn poll_read(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(&mut self.get_mut().socket).poll_read(cx, buf)
            }
        }
        impl AsyncWrite for FlushGate {
            fn poll_write(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                Pin::new(&mut self.get_mut().socket).poll_write(cx, buf)
            }
            fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                self.polls.fetch_add(1, Ordering::Relaxed);
                if self.ready.load(Ordering::Relaxed) {
                    Poll::Ready(Ok(()))
                } else {
                    Poll::Pending
                }
            }
            fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Pin::new(&mut self.get_mut().socket).poll_shutdown(cx)
            }
        }

        for request_flush in [false, true] {
            let (socket, mut peer) = duplex(64);
            let ready = Arc::new(AtomicBool::new(false));
            let polls = Arc::new(AtomicUsize::new(0));
            let socket = FlushGate {
                socket,
                ready: Arc::clone(&ready),
                polls: Arc::clone(&polls),
            };
            let mut network = Network::new(socket, 1024, 1024);
            network.write_request(Packet::PingReq).await.unwrap();
            if request_flush {
                assert!(network.flush_request().now_or_never().is_none());
            } else {
                assert!(network.flush().now_or_never().is_none());
            }
            let mut written = [0; 2];
            peer.read_exact(&mut written).await.unwrap();
            assert_eq!(written, [0xc0, 0]);
            let before_retry = polls.load(Ordering::Relaxed);
            ready.store(true, Ordering::Relaxed);
            network.flush().await.unwrap();
            assert!(polls.load(Ordering::Relaxed) > before_retry);
        }
    }

    #[tokio::test]
    async fn try_readb_finishes_started_writes_under_backpressure() {
        use tokio::io::AsyncReadExt;

        let (socket, mut peer) = duplex(8);
        let mut network = Network::new(socket, 1024, 1024);
        let mut state = MqttState::builder(10).build();
        // Fill the transport, then leave one packet buffered in the sink. Feeding
        // the automatic PUBACK must suspend while flushing that buffered packet.
        for _ in 0..4 {
            network.write(Packet::PingReq).await.unwrap();
            network.flush().await.unwrap();
        }
        network.write(Packet::PingReq).await.unwrap();
        network.set_backpressure_boundary(1);
        peer.write_all(&[0x32, 5, 0, 1, b'a', 0, 7]).await.unwrap();

        {
            let read = network.try_readb(&mut state, 1);
            tokio::pin!(read);
            assert!(read.as_mut().now_or_never().is_none());
            let mut buffered = [0; 8];
            peer.read_exact(&mut buffered).await.unwrap();
            assert_eq!(buffered, [0xc0, 0, 0xc0, 0, 0xc0, 0, 0xc0, 0]);
            let batch = read.await.unwrap().unwrap();
            assert_eq!(batch.outcome, ReadBatchOutcome::ResponseWritten);
        }
        network.flush().await.unwrap();
        let mut written = [0; 6];
        peer.read_exact(&mut written).await.unwrap();
        assert_eq!(written, [0xc0, 0, 0x40, 2, 0, 7]);
        assert_eq!(state.events.len(), 2);
    }

    #[tokio::test]
    async fn readb_processes_exactly_two_packets_when_limit_is_two() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, 1024, 1024);
        let mut state = MqttState::builder(10).build();

        peer.write_all(&[0xD0, 0x00, 0xD0, 0x00]).await.unwrap();

        let outcome = network.readb(&mut state, 2).await.unwrap().outcome;

        assert_eq!(state.events.len(), 2);
        assert_eq!(outcome, ReadBatchOutcome::NoResponseWritten);
    }

    #[tokio::test]
    async fn readb_processes_one_packet_when_limit_is_one() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, 1024, 1024);
        let mut state = MqttState::builder(10).build();

        peer.write_all(&[0xD0, 0x00, 0xD0, 0x00]).await.unwrap();

        let outcome = network.readb(&mut state, 1).await.unwrap().outcome;

        assert_eq!(state.events.len(), 1);
        assert_eq!(outcome, ReadBatchOutcome::NoResponseWritten);
    }
}
