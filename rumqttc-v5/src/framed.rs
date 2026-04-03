use futures_util::{FutureExt, SinkExt, StreamExt, future::poll_fn};
pub use rumqttc_core::AsyncReadWrite;
use tokio_util::codec::Framed;

use super::mqttbytes::v5::{Disconnect, DisconnectReasonCode, Packet};
use super::{Codec, Incoming, MqttState, StateError, mqttbytes};
use std::task::Poll;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InboundDisconnect {
    reason: DisconnectReasonCode,
}

impl InboundDisconnect {
    const fn classify(error: &mqttbytes::Error) -> Option<Self> {
        let reason = match error {
            mqttbytes::Error::MalformedPacket | mqttbytes::Error::MalformedRemainingLength => {
                DisconnectReasonCode::MalformedPacket
            }
            mqttbytes::Error::EmptySubscription | mqttbytes::Error::ProtocolError => {
                DisconnectReasonCode::ProtocolError
            }
            mqttbytes::Error::PayloadSizeLimitExceeded { .. } => {
                DisconnectReasonCode::PacketTooLarge
            }
            _ => return None,
        };

        Some(Self { reason })
    }
}

/// Network transforms packets <-> frames efficiently. It takes
/// advantage of pre-allocation, buffering and vectorization when
/// appropriate to achieve performance
pub struct Network {
    /// Frame MQTT packets from network connection
    framed: Framed<Box<dyn AsyncReadWrite>, Codec>,
}

impl Network {
    pub fn new(socket: impl AsyncReadWrite + 'static, max_incoming_size: Option<u32>) -> Self {
        let socket = Box::new(socket) as Box<dyn AsyncReadWrite>;
        let codec = Codec {
            max_incoming_size,
            max_outgoing_size: None,
        };
        let framed = Framed::new(socket, codec);

        Self { framed }
    }

    pub fn set_max_outgoing_size(&mut self, max_outgoing_size: Option<u32>) {
        self.framed.codec_mut().max_outgoing_size = max_outgoing_size;
    }

    async fn try_send_inbound_disconnect(&mut self, disconnect: InboundDisconnect) {
        let packet = Packet::Disconnect(Disconnect::new(disconnect.reason));

        poll_fn(|cx| match self.framed.poll_ready_unpin(cx) {
            Poll::Ready(Ok(())) => {
                if let Err(err) = self.framed.start_send_unpin(packet.clone()) {
                    trace!("dropping best-effort inbound disconnect after sink error: {err}");
                    return Poll::Ready(());
                }

                match self.framed.poll_flush_unpin(cx) {
                    Poll::Ready(Err(err)) => {
                        trace!("dropping best-effort inbound disconnect after flush error: {err}");
                        Poll::Ready(())
                    }
                    Poll::Ready(Ok(())) | Poll::Pending => Poll::Ready(()),
                }
            }
            Poll::Ready(Err(err)) => {
                trace!("dropping best-effort inbound disconnect after readiness error: {err}");
                Poll::Ready(())
            }
            Poll::Pending => Poll::Ready(()),
        })
        .await;
    }

    async fn handle_incoming_decode_error(&mut self, error: mqttbytes::Error) -> StateError {
        if let Some(disconnect) = InboundDisconnect::classify(&error) {
            self.try_send_inbound_disconnect(disconnect).await;
        }

        StateError::Deserialization(error)
    }

    /// Reads and returns a single packet from network
    pub async fn read(&mut self) -> Result<Incoming, StateError> {
        match self.framed.next().await {
            Some(Ok(packet)) => Ok(packet),
            Some(Err(mqttbytes::Error::InsufficientBytes(_))) => unreachable!(),
            Some(Err(e)) => Err(self.handle_incoming_decode_error(e).await),
            None => Err(StateError::ConnectionAborted),
        }
    }

    /// Read packets in bulk. This allow replies to be in bulk. This method is used
    /// after the connection is established to read a bunch of incoming packets
    pub async fn readb(
        &mut self,
        state: &mut MqttState,
        read_batch_limit: usize,
    ) -> Result<(), StateError> {
        // wait for the first read
        let mut res = self.framed.next().await;
        let read_batch_limit = read_batch_limit.max(1);
        let mut count = 0;
        loop {
            match res {
                Some(Ok(packet)) => {
                    if let Some(outgoing) = state.handle_incoming_packet(packet)? {
                        self.write(outgoing).await?;
                    }

                    count += 1;
                    if count >= read_batch_limit {
                        break;
                    }
                }
                Some(Err(mqttbytes::Error::InsufficientBytes(_))) => unreachable!(),
                Some(Err(e)) => return Err(self.handle_incoming_decode_error(e).await),
                None => return Err(StateError::ConnectionAborted),
            }
            // do not wait for subsequent reads
            match self.framed.next().now_or_never() {
                Some(r) => res = r,
                _ => break,
            }
        }

        Ok(())
    }

    /// Serializes packet into write buffer
    pub async fn write(&mut self, packet: Packet) -> Result<(), StateError> {
        self.framed
            .feed(packet)
            .await
            .map_err(StateError::Deserialization)
    }

    pub async fn flush(&mut self) -> Result<(), StateError> {
        self.framed
            .flush()
            .await
            .map_err(StateError::Deserialization)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io,
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf, duplex};

    #[derive(Debug)]
    struct BackpressuredIo {
        read: std::io::Cursor<Vec<u8>>,
    }

    impl BackpressuredIo {
        fn new(read: &[u8]) -> Self {
            Self {
                read: std::io::Cursor::new(read.to_vec()),
            }
        }
    }

    impl AsyncRead for BackpressuredIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let position = self.read.position() as usize;
            let remaining = &self.read.get_ref()[position..];
            if remaining.is_empty() {
                return Poll::Ready(Ok(()));
            }

            let to_copy = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            self.read.set_position((position + to_copy) as u64);
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for BackpressuredIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn readb_processes_exactly_two_packets_when_limit_is_two() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new(10, false, None);

        peer.write_all(&[0xD0, 0x00, 0xD0, 0x00]).await.unwrap();

        network.readb(&mut state, 2).await.unwrap();

        assert_eq!(state.events.len(), 2);
    }

    #[tokio::test]
    async fn readb_processes_one_packet_when_limit_is_one() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new(10, false, None);

        peer.write_all(&[0xD0, 0x00, 0xD0, 0x00]).await.unwrap();

        network.readb(&mut state, 1).await.unwrap();

        assert_eq!(state.events.len(), 1);
    }

    #[tokio::test]
    async fn read_sends_malformed_packet_disconnect_before_returning_error() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));

        // DISCONNECT with reserved flags set is a malformed packet.
        peer.write_all(&[0xE1, 0x01, 0x00]).await.unwrap();

        let err = network.read().await.unwrap_err();
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::MalformedPacket)
        ));

        let mut response = [0; 3];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [0xE0, 0x01, DisconnectReasonCode::MalformedPacket as u8]
        );
    }

    #[tokio::test]
    async fn readb_sends_packet_too_large_disconnect_before_returning_error() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(10));
        let mut state = MqttState::new(10, false, None);

        peer.write_all(&[0x30, 0x14]).await.unwrap();

        let err = network.readb(&mut state, 1).await.unwrap_err();
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::PayloadSizeLimitExceeded {
                pkt_size: 20,
                max: 10,
            })
        ));

        let mut response = [0; 3];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [0xE0, 0x01, DisconnectReasonCode::PacketTooLarge as u8]
        );
    }

    #[tokio::test]
    async fn read_sends_protocol_error_disconnect_for_empty_subscribe() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));

        peer.write_all(&[0x82, 0x03, 0x00, 0x01, 0x00])
            .await
            .unwrap();

        let err = network.read().await.unwrap_err();
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::EmptySubscription)
        ));

        let mut response = [0; 3];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [0xE0, 0x01, DisconnectReasonCode::ProtocolError as u8]
        );
    }

    #[tokio::test]
    async fn read_sends_protocol_error_disconnect_for_zero_subscription_identifier() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));

        peer.write_all(&[
            0x82, 0x09, 0x00, 0x01, 0x02, 0x0B, 0x00, 0x00, 0x01, b'a', 0x00,
        ])
        .await
        .unwrap();

        let err = network.read().await.unwrap_err();
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::ProtocolError)
        ));

        let mut response = [0; 3];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [0xE0, 0x01, DisconnectReasonCode::ProtocolError as u8]
        );
    }

    #[tokio::test]
    async fn read_sends_malformed_packet_disconnect_for_invalid_publish_qos_bits() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));

        peer.write_all(&[0x36, 0x02, 0x00, 0x00]).await.unwrap();

        let err = network.read().await.unwrap_err();
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::MalformedPacket)
        ));

        let mut response = [0; 3];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [0xE0, 0x01, DisconnectReasonCode::MalformedPacket as u8]
        );
    }

    #[tokio::test]
    async fn read_returns_decode_error_promptly_under_write_backpressure() {
        let io = BackpressuredIo::new(&[0xE1, 0x01, 0x00]);
        let mut network = Network::new(io, Some(1024));

        let err = tokio::time::timeout(std::time::Duration::from_millis(50), network.read())
            .await
            .expect("read should not wait for best-effort disconnect")
            .unwrap_err();

        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::MalformedPacket)
        ));
    }

    #[tokio::test]
    async fn readb_returns_decode_error_promptly_under_write_backpressure() {
        let io = BackpressuredIo::new(&[0x30, 0x14]);
        let mut network = Network::new(io, Some(10));
        let mut state = MqttState::new(10, false, None);

        let err = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            network.readb(&mut state, 1),
        )
        .await
        .expect("readb should not wait for best-effort disconnect")
        .unwrap_err();

        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::PayloadSizeLimitExceeded {
                pkt_size: 20,
                max: 10,
            })
        ));
    }
}
