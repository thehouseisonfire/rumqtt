use futures_util::{FutureExt, SinkExt, StreamExt};
pub use rumqttc_core::AsyncReadWrite;
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

        Self { framed }
    }

    /// Reads and returns a single packet from network
    pub async fn read(&mut self) -> Result<Incoming, StateError> {
        match self.framed.next().await {
            Some(Ok(packet)) => Ok(packet),
            Some(Err(mqttbytes::Error::InsufficientBytes(_))) => unreachable!(),
            Some(Err(e)) => Err(StateError::Deserialization(e)),
            None => Err(StateError::ConnectionAborted),
        }
    }

    /// Read packets in bulk. This allow replies to be in bulk. This method is used
    /// after the connection is established to read a bunch of incoming packets
    pub(crate) async fn readb(
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

    /// Serializes packet into write buffer
    pub async fn write(&mut self, packet: Packet) -> Result<(), StateError> {
        self.framed
            .feed(packet)
            .await
            .map_err(StateError::Deserialization)
    }

    pub async fn flush(&mut self) -> Result<(), crate::state::StateError> {
        self.framed
            .flush()
            .await
            .map_err(StateError::Deserialization)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, duplex};

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
