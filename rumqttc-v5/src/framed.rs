use futures_util::{FutureExt, SinkExt, StreamExt, future::poll_fn};
pub use rumqttc_core::AsyncReadWrite;
use tokio_util::codec::Framed;

use crate::notice::DeferredNotice;

use super::mqttbytes::v5::{Disconnect, DisconnectReasonCode, Packet};
use super::{Codec, Incoming, MqttState, StateError, mqttbytes};
use std::task::Poll;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InboundDisconnect {
    reason: DisconnectReasonCode,
}

impl InboundDisconnect {
    const fn classify(error: &mqttbytes::Error) -> Option<Self> {
        let Some(reason) = inbound_disconnect_reason(error) else {
            return None;
        };

        Some(Self { reason })
    }

    const fn error(self) -> mqttbytes::Error {
        match self.reason {
            DisconnectReasonCode::ProtocolError => mqttbytes::Error::ProtocolError,
            // Preserve any more-specific MQTT 5 disconnect reason that came from parsing/state.
            reason => mqttbytes::Error::ProtocolViolation(reason),
        }
    }
}

const fn inbound_disconnect_reason(error: &mqttbytes::Error) -> Option<DisconnectReasonCode> {
    let reason = match error {
        mqttbytes::Error::ProtocolViolation(reason) => *reason,
        mqttbytes::Error::InvalidConnectReturnCode(_)
        | mqttbytes::Error::InvalidReason(_)
        | mqttbytes::Error::InvalidRemainingLength(_)
        | mqttbytes::Error::InvalidProtocol
        | mqttbytes::Error::InvalidProtocolLevel(_)
        | mqttbytes::Error::IncorrectPacketFormat
        | mqttbytes::Error::InvalidPacketType(_)
        | mqttbytes::Error::InvalidRetainForwardRule(_)
        | mqttbytes::Error::InvalidQoS(_)
        | mqttbytes::Error::InvalidSubscribeReasonCode(_)
        | mqttbytes::Error::PayloadSizeIncorrect
        | mqttbytes::Error::PayloadTooLong
        | mqttbytes::Error::PayloadRequired
        | mqttbytes::Error::PayloadNotUtf8(_)
        | mqttbytes::Error::BoundaryCrossed(_)
        | mqttbytes::Error::MalformedPacket
        | mqttbytes::Error::MalformedRemainingLength
        | mqttbytes::Error::InvalidPropertyType(_)
        | mqttbytes::Error::TopicNotUtf8 { .. } => DisconnectReasonCode::MalformedPacket,
        mqttbytes::Error::EmptySubscription
        | mqttbytes::Error::ProtocolError
        | mqttbytes::Error::PacketIdZero => DisconnectReasonCode::ProtocolError,
        mqttbytes::Error::PayloadSizeLimitExceeded { .. } => DisconnectReasonCode::PacketTooLarge,
        // These cases are local transport/incremental framing conditions, not protocol responses.
        mqttbytes::Error::InsufficientBytes(_)
        | mqttbytes::Error::Io(_)
        | mqttbytes::Error::OutgoingPacketTooLarge { .. } => return None,
    };

    Some(reason)
}

/// Network transforms packets <-> frames efficiently. It takes
/// advantage of pre-allocation, buffering and vectorization when
/// appropriate to achieve performance
pub struct Network {
    /// Frame MQTT packets from network connection
    framed: Framed<Box<dyn AsyncReadWrite>, Codec>,
}

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
    pub fn new(socket: impl AsyncReadWrite + 'static, max_incoming_size: Option<u32>) -> Self {
        let socket: Box<dyn AsyncReadWrite> = Box::new(socket);
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
    ) -> Result<ReadBatch, ReadBatchError> {
        // wait for the first read
        let mut res = self.framed.next().await;
        let read_batch_limit = read_batch_limit.max(1);
        let mut count = 0;
        let mut outcome = ReadBatchOutcome::NoResponseWritten;
        let mut notices = Vec::new();
        loop {
            match res {
                Some(Ok(packet)) => {
                    match state.handle_incoming_packet_with_effects(packet) {
                        Ok(super::state::IncomingPacketEffects {
                            outgoing: Some(Packet::Disconnect(disconnect)),
                            notices: packet_notices,
                        }) => {
                            if disconnect.reason_code as u8 >= 0x80 {
                                let disconnect = InboundDisconnect {
                                    reason: disconnect.reason_code,
                                };
                                state.discard_last_outgoing_disconnect_event();
                                self.try_send_inbound_disconnect(disconnect).await;
                                return Err(ReadBatchError::new(
                                    StateError::Deserialization(disconnect.error()),
                                    outcome,
                                    notices,
                                ));
                            }

                            let has_deferred_notices = !packet_notices.is_empty();
                            notices.extend(packet_notices);
                            if let Err(err) = self.write(Packet::Disconnect(disconnect)).await {
                                return Err(ReadBatchError::new(err, outcome, notices));
                            }
                            outcome = ReadBatchOutcome::ResponseWritten;
                            if has_deferred_notices {
                                break;
                            }
                        }
                        Ok(super::state::IncomingPacketEffects {
                            outgoing: Some(outgoing),
                            notices: packet_notices,
                        }) => {
                            let has_deferred_notices = !packet_notices.is_empty();
                            notices.extend(packet_notices);
                            if let Err(err) = self.write(outgoing).await {
                                return Err(ReadBatchError::new(err, outcome, notices));
                            }
                            outcome = ReadBatchOutcome::ResponseWritten;
                            if has_deferred_notices {
                                break;
                            }
                        }
                        Ok(super::state::IncomingPacketEffects {
                            outgoing: None,
                            notices: packet_notices,
                        }) => {
                            let has_deferred_notices = !packet_notices.is_empty();
                            notices.extend(packet_notices);
                            if has_deferred_notices {
                                break;
                            }
                        }
                        Err(
                            err @ (StateError::Deserialization(mqttbytes::Error::ProtocolError)
                            | StateError::ProtocolViolation(_)),
                        ) => {
                            self.try_send_inbound_disconnect(InboundDisconnect {
                                reason: DisconnectReasonCode::ProtocolError,
                            })
                            .await;
                            return Err(ReadBatchError::new(err, outcome, notices));
                        }
                        Err(err) => return Err(ReadBatchError::new(err, outcome, notices)),
                    }

                    count += 1;
                    if count >= read_batch_limit {
                        break;
                    }
                }
                Some(Err(mqttbytes::Error::InsufficientBytes(_))) => unreachable!(),
                Some(Err(e)) => {
                    let err = self.handle_incoming_decode_error(e).await;
                    return Err(ReadBatchError::new(err, outcome, notices));
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

    #[cfg(test)]
    pub(crate) fn set_backpressure_boundary(&mut self, boundary: usize) {
        self.framed.set_backpressure_boundary(boundary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Request;
    use crate::mqttbytes::{
        QoS,
        v5::{Auth, AuthProperties, AuthReasonCode, PubAck, Publish},
    };
    use crate::notice::{PublishNoticeTx, TrackedNoticeTx};
    use bytes::BytesMut;
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
            let position = usize::try_from(self.read.position())
                .expect("test cursor position should fit in usize");
            let remaining = &self.read.get_ref()[position..];
            if remaining.is_empty() {
                return Poll::Ready(Ok(()));
            }

            let to_copy = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            let new_position = position
                .checked_add(to_copy)
                .expect("test cursor position should not overflow");
            self.read.set_position(
                u64::try_from(new_position).expect("test cursor position should fit in u64"),
            );
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
        let mut state = MqttState::builder(10).build();

        peer.write_all(&[0xD0, 0x00, 0xD0, 0x00]).await.unwrap();

        let outcome = network.readb(&mut state, 2).await.unwrap().outcome;

        assert_eq!(state.events.len(), 2);
        assert_eq!(outcome, ReadBatchOutcome::NoResponseWritten);
    }

    #[tokio::test]
    async fn readb_processes_one_packet_when_limit_is_one() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::builder(10).build();

        peer.write_all(&[0xD0, 0x00, 0xD0, 0x00]).await.unwrap();

        let outcome = network.readb(&mut state, 1).await.unwrap().outcome;

        assert_eq!(state.events.len(), 1);
        assert_eq!(outcome, ReadBatchOutcome::NoResponseWritten);
    }

    #[tokio::test]
    async fn readb_reports_response_written_for_automatic_puback() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::builder(10).build();
        let mut publish = Publish::new("hello/world", QoS::AtLeastOnce, vec![1, 2, 3], None);
        publish.pkid = 1;
        let mut bytes = BytesMut::new();
        publish.write(&mut bytes).unwrap();

        peer.write_all(&bytes).await.unwrap();

        let outcome = network.readb(&mut state, 1).await.unwrap().outcome;

        assert_eq!(outcome, ReadBatchOutcome::ResponseWritten);
    }

    #[tokio::test]
    async fn readb_stops_after_deferred_notice_before_buffered_error() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::builder(10).build();
        let (notice_tx, _notice) = PublishNoticeTx::new();
        let mut publish = Publish::new("hello/world", QoS::AtLeastOnce, vec![1, 2, 3], None);
        publish.pkid = 1;
        state
            .handle_outgoing_packet_with_notice(
                Request::Publish(publish),
                Some(TrackedNoticeTx::Publish(notice_tx)),
            )
            .unwrap();
        let mut puback = BytesMut::new();
        PubAck::new(1, None).write(&mut puback).unwrap();

        peer.write_all(&puback).await.unwrap();
        peer.write_all(&[0xE1, 0x01, 0x00]).await.unwrap();

        let batch = network.readb(&mut state, 10).await.unwrap();

        assert_eq!(batch.outcome, ReadBatchOutcome::NoResponseWritten);
        assert_eq!(batch.notices.len(), 1);

        let err = network.readb(&mut state, 10).await.unwrap_err().source;
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::IncorrectPacketFormat)
        ));
    }

    #[tokio::test]
    async fn readb_preserves_deferred_notice_when_collision_replay_write_fails() {
        let (client, mut peer) = duplex(64);
        let (notice_tx, _notice) = PublishNoticeTx::new();
        let mut first = Publish::new("hello/world", QoS::AtLeastOnce, vec![1, 2, 3], None);
        first.pkid = 1;
        let mut collision = Publish::new("hello/world", QoS::AtLeastOnce, vec![4, 5, 6], None);
        collision.pkid = 1;
        let mut puback = BytesMut::new();
        PubAck::new(1, None).write(&mut puback).unwrap();
        let mut network = Network::new(client, Some(1024));
        network.set_max_outgoing_size(Some(1));
        let mut state = MqttState::builder(10).build();

        state
            .handle_outgoing_packet_with_notice(
                Request::Publish(first),
                Some(TrackedNoticeTx::Publish(notice_tx)),
            )
            .unwrap();
        let (packet, flush_notice) = state
            .handle_outgoing_packet_with_notice(Request::Publish(collision), None)
            .unwrap();
        assert!(packet.is_none());
        assert!(flush_notice.is_none());
        assert!(state.collision.is_some());

        peer.write_all(&puback).await.unwrap();
        let err = network.readb(&mut state, 1).await.unwrap_err();

        assert!(matches!(
            err.source,
            StateError::Deserialization(mqttbytes::Error::OutgoingPacketTooLarge { .. })
        ));
        assert_eq!(err.batch.notices.len(), 1);
    }

    #[tokio::test]
    async fn read_sends_malformed_packet_disconnect_before_returning_error() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));

        // DISCONNECT with reserved flags set is a malformed packet (MQTT-2.1.3-1).
        peer.write_all(&[0xE1, 0x01, 0x00]).await.unwrap();

        let err = network.read().await.unwrap_err();
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::IncorrectPacketFormat)
        ));

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [
                0xE0,
                0x02,
                DisconnectReasonCode::MalformedPacket as u8,
                0x00
            ]
        );
    }

    #[tokio::test]
    async fn readb_sends_packet_too_large_disconnect_before_returning_error() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(10));
        let mut state = MqttState::builder(10).build();

        peer.write_all(&[0x30, 0x14]).await.unwrap();

        let err = network.readb(&mut state, 1).await.unwrap_err().source;
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::PayloadSizeLimitExceeded {
                pkt_size: 22,
                max: 10,
            })
        ));

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        // MQTT-3.1.2-24: reason code is the literal 0x95 (Packet too large),
        // pinned on the wire independently of the DisconnectReasonCode enum.
        assert_eq!(response, [0xE0, 0x02, 0x95, 0x00]);
        assert_eq!(response[2], DisconnectReasonCode::PacketTooLarge as u8);
    }

    #[tokio::test]
    async fn read_sends_packet_too_large_disconnect_before_returning_error() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(10));

        peer.write_all(&[0x30, 0x14]).await.unwrap();

        let err = network.read().await.unwrap_err();
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::PayloadSizeLimitExceeded {
                pkt_size: 22,
                max: 10,
            })
        ));

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        // MQTT-3.1.2-24: the single-packet read() path also flushes a DISCONNECT
        // with the literal 0x95 (Packet too large) reason code before erroring.
        assert_eq!(response, [0xE0, 0x02, 0x95, 0x00]);
        assert_eq!(response[2], DisconnectReasonCode::PacketTooLarge as u8);
    }

    #[tokio::test]
    async fn read_sends_protocol_error_disconnect_for_zero_packet_id() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));

        peer.write_all(&[0x82, 0x06, 0x00, 0x00, 0x00, 0x01, b'a', 0x00])
            .await
            .unwrap();

        let err = network.read().await.unwrap_err();
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::PacketIdZero)
        ));

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    #[tokio::test]
    async fn readb_sends_topic_alias_invalid_disconnect_and_returns_error() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::builder(10).client_topic_alias_max(5).build();

        peer.write_all(&[0x30, 0x07, 0x00, 0x01, b'a', 0x03, 0x23, 0x00, 0x06])
            .await
            .unwrap();

        let err = network.readb(&mut state, 1).await.unwrap_err().source;
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::ProtocolViolation(
                DisconnectReasonCode::TopicAliasInvalid
            ))
        ));

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [
                0xE0,
                0x02,
                DisconnectReasonCode::TopicAliasInvalid as u8,
                0x00
            ]
        );
    }

    #[tokio::test]
    async fn readb_sends_protocol_error_disconnect_for_server_reauthenticate() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::builder(10)
            .authentication_method(Some("test-method".to_owned()))
            .build();
        let auth = Auth::new(
            AuthReasonCode::ReAuthenticate,
            Some(AuthProperties {
                method: Some("test-method".to_owned()),
                data: None,
                reason: None,
                user_properties: vec![],
            }),
        );
        let mut encoded = BytesMut::new();
        auth.write(&mut encoded).unwrap();

        peer.write_all(&encoded).await.unwrap();

        let err = network.readb(&mut state, 1).await.unwrap_err().source;
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::ProtocolError)
        ));

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
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

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    #[tokio::test]
    async fn read_sends_malformed_packet_disconnect_for_reserved_subscribe_option_bits() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));

        peer.write_all(&[0x82, 0x07, 0x00, 0x01, 0x00, 0x00, 0x01, b'a', 0x40])
            .await
            .unwrap();

        let err = network.read().await.unwrap_err();
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::IncorrectPacketFormat)
        ));

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [
                0xE0,
                0x02,
                DisconnectReasonCode::MalformedPacket as u8,
                0x00
            ]
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

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    #[tokio::test]
    async fn read_sends_topic_alias_invalid_disconnect_for_zero_topic_alias() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));

        peer.write_all(&[0x30, 0x07, 0x00, 0x01, b'a', 0x03, 0x23, 0x00, 0x00])
            .await
            .unwrap();

        let err = network.read().await.unwrap_err();
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::ProtocolViolation(
                DisconnectReasonCode::TopicAliasInvalid
            ))
        ));

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [
                0xE0,
                0x02,
                DisconnectReasonCode::TopicAliasInvalid as u8,
                0x00
            ]
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

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [
                0xE0,
                0x02,
                DisconnectReasonCode::MalformedPacket as u8,
                0x00
            ]
        );
    }

    #[tokio::test]
    async fn read_sends_malformed_packet_disconnect_for_invalid_utf8() {
        let (client, mut peer) = duplex(64);
        let mut network = Network::new(client, Some(1024));

        peer.write_all(&[
            0x30, 0x04, // PUBLISH, remaining length
            0x00, 0x01, // topic length
            0xff, // invalid UTF-8
            0x00, // properties length
        ])
        .await
        .unwrap();

        let err = network.read().await.unwrap_err();
        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::TopicNotUtf8 { .. })
        ));

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [
                0xE0,
                0x02,
                DisconnectReasonCode::MalformedPacket as u8,
                0x00
            ]
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
            StateError::Deserialization(mqttbytes::Error::IncorrectPacketFormat)
        ));
    }

    #[tokio::test]
    async fn readb_returns_decode_error_promptly_under_write_backpressure() {
        let io = BackpressuredIo::new(&[0x30, 0x14]);
        let mut network = Network::new(io, Some(10));
        let mut state = MqttState::builder(10).build();

        let err = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            network.readb(&mut state, 1),
        )
        .await
        .expect("readb should not wait for best-effort disconnect")
        .unwrap_err()
        .source;

        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::PayloadSizeLimitExceeded {
                pkt_size: 22,
                max: 10,
            })
        ));
    }

    #[tokio::test]
    async fn readb_returns_state_protocol_error_promptly_under_write_backpressure() {
        let auth = Auth::new(
            AuthReasonCode::ReAuthenticate,
            Some(AuthProperties {
                method: Some("test-method".to_owned()),
                data: None,
                reason: None,
                user_properties: vec![],
            }),
        );
        let mut encoded = BytesMut::new();
        auth.write(&mut encoded).unwrap();
        let io = BackpressuredIo::new(&encoded);
        let mut network = Network::new(io, Some(1024));
        let mut state = MqttState::builder(10)
            .authentication_method(Some("test-method".to_owned()))
            .build();

        let err = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            network.readb(&mut state, 1),
        )
        .await
        .expect("readb should not wait for best-effort state protocol-error disconnect")
        .unwrap_err()
        .source;

        assert!(matches!(
            err,
            StateError::Deserialization(mqttbytes::Error::ProtocolError)
        ));
    }
}
