#![allow(dead_code)]

use bytes::BytesMut;
use rumqttc::v5::mqttbytes::Error as MqttError;
use rumqttc::v5::mqttbytes::v5::{ConnAck, ConnectReturnCode, Packet, PingReq, PingResp, PubAck};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{self, Duration, Instant};

const MAX_PACKET_SIZE: u32 = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Broker {
    socket: TcpStream,
    read: BytesMut,
}

impl Broker {
    pub async fn from_listener(
        listener: TcpListener,
        connack_code: ConnectReturnCode,
        session_present: bool,
    ) -> Self {
        let (socket, _) = listener.accept().await.unwrap();
        let mut broker = Self {
            socket,
            read: BytesMut::with_capacity(10 * 1024),
        };

        let packet = broker
            .read_packet_with_timeout(CONNECT_TIMEOUT)
            .await
            .expect("expected initial CONNECT packet");
        match packet {
            Packet::Connect(_, _, _) => {}
            packet => panic!("expected CONNECT packet, got {packet:?}"),
        }

        let connack = ConnAck {
            session_present,
            code: connack_code,
            properties: None,
        };
        broker.write(Packet::ConnAck(connack)).await;

        broker
    }

    pub async fn read_packet(&mut self) -> Option<Packet> {
        self.read_packet_with_timeout(Duration::from_secs(2)).await
    }

    pub async fn read_packet_with_timeout(&mut self, timeout: Duration) -> Option<Packet> {
        let started = Instant::now();

        loop {
            match Packet::read(&mut self.read, Some(MAX_PACKET_SIZE)) {
                Ok(packet) => return Some(packet),
                Err(MqttError::InsufficientBytes(_)) => {}
                Err(error) => panic!("failed to decode v5 packet: {error:?}"),
            }

            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return None;
            }
            let remaining = timeout.saturating_sub(elapsed);

            let read = match time::timeout(remaining, self.socket.read_buf(&mut self.read)).await {
                Ok(Ok(n)) => n,
                Ok(Err(error)) => panic!("failed to read packet bytes: {error}"),
                Err(_) => return None,
            };

            if read == 0 {
                return None;
            }
        }
    }

    pub async fn ack(&mut self, pkid: u16) {
        self.write(Packet::PubAck(PubAck::new(pkid, None))).await;
    }

    pub async fn pingresp(&mut self) {
        self.write(Packet::PingResp(PingResp)).await;
    }

    pub async fn tick(&mut self) -> Option<Packet> {
        let packet = self.read_packet().await?;
        if matches!(packet, Packet::PingReq(PingReq)) {
            self.pingresp().await;
            return self.read_packet().await;
        }

        Some(packet)
    }

    async fn write(&mut self, packet: Packet) {
        let mut write = BytesMut::new();
        packet.write(&mut write, Some(MAX_PACKET_SIZE)).unwrap();
        self.socket.write_all(&write).await.unwrap();
    }
}
