#![allow(dead_code)]
#![allow(clippy::missing_errors_doc)]

use bytes::BytesMut;
use flume::{Receiver, Sender, bounded};
use rumqttc::mqttbytes::v5::{
    ConnAck, ConnectReturnCode, Packet, PingReq, PingResp, PubAck, Publish,
};
use rumqttc::mqttbytes::{self, QoS};
use rumqttc::{Event, Incoming, Outgoing};
use std::collections::VecDeque;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::select;
use tokio::{task, time};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectBehavior {
    Accept { session_saved: bool },
    RefuseBadUserNamePassword,
    StallAfterConnect,
}

pub struct Broker {
    pub(crate) framed: Network,
    pub(crate) incoming: VecDeque<Packet>,
    outgoing_tx: Sender<Packet>,
    outgoing_rx: Receiver<Packet>,
}

impl Broker {
    /// Creates a new broker which accepts one MQTT connection.
    ///
    /// # Panics
    ///
    /// Panics if the test listener cannot be bound or if the initial CONNECT
    /// handshake cannot be completed.
    #[allow(dead_code)]
    pub async fn new(port: u16, behavior: ConnectBehavior) -> Self {
        let addr = format!("127.0.0.1:{port}");
        let listener = TcpListener::bind(&addr).await.unwrap();
        Self::from_listener(listener, behavior).await
    }

    /// Creates a new broker from an existing listener.
    ///
    /// # Panics
    ///
    /// Panics if accepting the test connection, reading the initial packet, or
    /// writing the `CONNACK` fails unexpectedly.
    pub async fn from_listener(listener: TcpListener, behavior: ConnectBehavior) -> Self {
        let (stream, _) = listener.accept().await.unwrap();
        let mut framed = Network::new(stream, 10 * 1024);
        let mut incoming = VecDeque::new();
        let (outgoing_tx, outgoing_rx) = bounded(10);
        framed.readb(&mut incoming).await.unwrap();

        match incoming.pop_front().unwrap() {
            Packet::Connect(connect, _, _) => match behavior {
                ConnectBehavior::Accept { session_saved } => {
                    let connack = ConnAck {
                        code: ConnectReturnCode::Success,
                        session_present: !connect.clean_start && session_saved,
                        properties: None,
                    };
                    framed.connack(connack).await.unwrap();
                }
                ConnectBehavior::RefuseBadUserNamePassword => {
                    let connack = ConnAck {
                        code: ConnectReturnCode::BadUserNamePassword,
                        session_present: false,
                        properties: None,
                    };
                    framed.connack(connack).await.unwrap();
                }
                ConnectBehavior::StallAfterConnect => {
                    return Self {
                        framed,
                        incoming,
                        outgoing_tx,
                        outgoing_rx,
                    };
                }
            },
            packet => panic!("Expecting connect packet, received {packet:?}"),
        }

        Self {
            framed,
            incoming: VecDeque::new(),
            outgoing_tx,
            outgoing_rx,
        }
    }

    pub async fn read_publish(&mut self) -> Option<Publish> {
        self.read_publish_with_timeout(Duration::from_secs(2)).await
    }

    /// Reads a publish packet from the stream with a caller-provided timeout.
    ///
    /// # Panics
    ///
    /// Panics if reading from the test transport fails unexpectedly after the
    /// timeout has started.
    pub async fn read_publish_with_timeout(&mut self, timeout: Duration) -> Option<Publish> {
        loop {
            let packet = if let Some(packet) = self.incoming.pop_front() {
                packet
            } else {
                let packet = time::timeout(timeout, async {
                    self.framed.readb(&mut self.incoming).await.unwrap();
                    self.incoming.pop_front().unwrap()
                })
                .await;

                match packet {
                    Ok(packet) => packet,
                    Err(_) => return None,
                }
            };

            match packet {
                Packet::Publish(publish) => return Some(publish),
                Packet::PingReq(_) => {
                    self.framed.write(Packet::PingResp(PingResp)).await.unwrap();
                }
                packet => panic!("Expecting a publish. Received = {packet:?}"),
            }
        }
    }

    pub async fn wait_for_n_publishes(
        &mut self,
        count: usize,
        timeout: Duration,
    ) -> Option<Vec<Publish>> {
        let mut publishes = Vec::with_capacity(count);
        for _ in 0..count {
            let publish = self.read_publish_with_timeout(timeout).await?;
            publishes.push(publish);
        }

        Some(publishes)
    }

    pub async fn read_packet(&mut self) -> Option<Packet> {
        let _ = time::timeout(Duration::from_secs(30), async {
            let result = self.framed.readb(&mut self.incoming).await;
            if result.is_err() {
                println!("Broker read = {result:?}");
            }
        })
        .await;

        self.incoming.pop_front()
    }

    /// Reads packets forever until the caller cancels the task.
    ///
    /// # Panics
    ///
    /// Panics if reading from the test transport fails unexpectedly.
    pub async fn blackhole(&mut self) -> Packet {
        loop {
            self.framed.readb(&mut self.incoming).await.unwrap();
        }
    }

    /// Sends a `PUBACK`.
    ///
    /// # Panics
    ///
    /// Panics if writing to the test transport fails unexpectedly.
    pub async fn ack(&mut self, pkid: u16) {
        let packet = Packet::PubAck(PubAck::new(pkid, None));
        self.framed.write(packet).await.unwrap();
    }

    pub async fn ack_many(&mut self, packet_ids: &[u16]) {
        for pkid in packet_ids {
            self.ack(*pkid).await;
        }
    }

    /// Sends a `PINGRESP`.
    ///
    /// # Panics
    ///
    /// Panics if writing to the test transport fails unexpectedly.
    pub async fn pingresp(&mut self) {
        let packet = Packet::PingResp(PingResp);
        self.framed.write(packet).await.unwrap();
    }

    /// Spawns a task that emits a fixed number of test publishes.
    ///
    /// # Panics
    ///
    /// Panics if the internal test channel is closed while the spawned task is
    /// sending publishes.
    pub fn spawn_publishes(&self, count: u8, qos: QoS, delay: u64) {
        let tx = self.outgoing_tx.clone();

        task::spawn(async move {
            for i in 1..=count {
                let topic = "hello/world".to_owned();
                let payload = vec![1, 2, 3, i];
                let mut publish = Publish::new(topic, qos, payload, None);

                if !matches!(qos, QoS::AtMostOnce) {
                    publish.pkid = u16::from(i);
                }

                tx.send_async(Packet::Publish(publish)).await.unwrap();
                time::sleep(Duration::from_secs(delay)).await;
            }
        });
    }

    /// Selects between outgoing and incoming packets.
    ///
    /// # Panics
    ///
    /// Panics if the internal test channel closes or if network I/O fails
    /// unexpectedly.
    pub async fn tick(&mut self) -> Event {
        if let Some(incoming) = self.incoming.pop_front() {
            return Event::Incoming(incoming);
        }

        select! {
            request = self.outgoing_rx.recv_async() => {
                let request = request.unwrap();
                let outgoing = self.framed.write(request).await.unwrap();
                Event::Outgoing(outgoing)
            }
            packet = self.framed.readb(&mut self.incoming) => {
                packet.unwrap();
                let incoming = self.incoming.pop_front().unwrap();
                Event::Incoming(incoming)
            }
        }
    }
}

pub struct Network {
    socket: Box<dyn N>,
    read: BytesMut,
    write: BytesMut,
    max_incoming_size: u32,
    max_readb_count: usize,
}

impl Network {
    pub fn new(socket: impl N + 'static, max_incoming_size: u32) -> Self {
        let socket = Box::new(socket) as Box<dyn N>;
        Self {
            socket,
            read: BytesMut::with_capacity(10 * 1024),
            write: BytesMut::with_capacity(10 * 1024),
            max_incoming_size,
            max_readb_count: 10,
        }
    }

    async fn read_bytes(&mut self, required: usize) -> io::Result<usize> {
        let mut total_read = 0;
        loop {
            let read = self.socket.read_buf(&mut self.read).await?;
            if read == 0 {
                return if self.read.is_empty() {
                    Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "connection closed by peer",
                    ))
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "connection reset by peer",
                    ))
                };
            }

            total_read += read;
            if total_read >= required {
                return Ok(total_read);
            }
        }
    }

    pub async fn connack(&mut self, connack: ConnAck) -> io::Result<usize> {
        let mut write = BytesMut::new();
        let len = connack
            .write(&mut write)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;

        self.socket.write_all(&write[..]).await?;
        Ok(len)
    }

    pub async fn readb(&mut self, incoming: &mut VecDeque<Incoming>) -> io::Result<()> {
        let mut count = 0;
        loop {
            match Packet::read(&mut self.read, Some(self.max_incoming_size)) {
                Ok(packet) => {
                    incoming.push_back(packet);
                    count += 1;
                    if count >= self.max_readb_count {
                        return Ok(());
                    }
                }
                Err(mqttbytes::Error::InsufficientBytes(_)) if count > 0 => return Ok(()),
                Err(mqttbytes::Error::InsufficientBytes(required)) => {
                    self.read_bytes(required).await?;
                }
                Err(error) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        error.to_string(),
                    ));
                }
            }
        }
    }

    async fn write(&mut self, packet: Packet) -> Result<Outgoing, mqttbytes::Error> {
        let outgoing = outgoing(&packet);
        match packet {
            Packet::Publish(packet) => packet.write(&mut self.write)?,
            Packet::PingReq(_) => PingReq::write(&mut self.write)?,
            Packet::PingResp(_) => PingResp::write(&mut self.write)?,
            Packet::Disconnect(packet) => packet.write(&mut self.write)?,
            Packet::PubAck(packet) => packet.write(&mut self.write)?,
            Packet::PubRec(packet) => packet.write(&mut self.write)?,
            Packet::PubRel(packet) => packet.write(&mut self.write)?,
            Packet::PubComp(packet) => packet.write(&mut self.write)?,
            Packet::Subscribe(packet) => packet.write(&mut self.write)?,
            Packet::SubAck(packet) => packet.write(&mut self.write)?,
            Packet::Unsubscribe(packet) => packet.write(&mut self.write)?,
            Packet::UnsubAck(packet) => packet.write(&mut self.write)?,
            Packet::Auth(packet) => packet.write(&mut self.write)?,
            Packet::Connect(_, _, _) | Packet::ConnAck(_) => unimplemented!(),
        };

        self.socket.write_all(&self.write[..]).await.unwrap();
        self.write.clear();

        Ok(outgoing)
    }
}

fn outgoing(packet: &Packet) -> Outgoing {
    match packet {
        Packet::Publish(publish) => Outgoing::Publish(publish.pkid),
        Packet::PubAck(puback) => Outgoing::PubAck(puback.pkid),
        Packet::PubRec(pubrec) => Outgoing::PubRec(pubrec.pkid),
        Packet::PubRel(pubrel) => Outgoing::PubRel(pubrel.pkid),
        Packet::PubComp(pubcomp) => Outgoing::PubComp(pubcomp.pkid),
        Packet::Subscribe(subscribe) => Outgoing::Subscribe(subscribe.pkid),
        Packet::Unsubscribe(unsubscribe) => Outgoing::Unsubscribe(unsubscribe.pkid),
        Packet::PingReq(_) => Outgoing::PingReq,
        Packet::PingResp(_) => Outgoing::PingResp,
        Packet::Disconnect(_) => Outgoing::Disconnect,
        Packet::Auth(_) => Outgoing::Auth,
        packet => panic!("Invalid outgoing packet = {packet:?}"),
    }
}

pub trait N: AsyncRead + AsyncWrite + Send + Sync + Unpin {}
impl<T> N for T where T: AsyncRead + AsyncWrite + Unpin + Send + Sync {}
