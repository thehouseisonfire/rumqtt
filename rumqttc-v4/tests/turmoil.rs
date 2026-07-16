//! Exercises the custom socket connector against Turmoil's simulated network.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use bytes::BytesMut;
use rumqttc::mqttbytes::v4::{
    ConnAck, ConnectReturnCode, Packet, PubAck, Publish, SubAck, SubscribeReasonCode,
};
use rumqttc::mqttbytes::{Error, QoS};
use rumqttc::{AsyncClient, Event, MqttOptions, PublishOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil::net::{TcpListener, TcpStream};

const MAX_PACKET_SIZE: usize = 10 * 1024;
const PORT: u16 = 1883;
const TOPIC: &str = "hello/world";
const PAYLOAD: &[u8] = b"simulated payload";

fn mqtt_options(client_id: &str) -> MqttOptions {
    let mut options = MqttOptions::new(client_id, ("broker", PORT));
    options.set_socket_connector(
        |addr, _network_options| async move { TcpStream::connect(addr).await },
    );
    options
}

async fn read_packet(stream: &mut TcpStream, buffer: &mut BytesMut) -> turmoil::Result<Packet> {
    loop {
        match Packet::read(buffer, MAX_PACKET_SIZE) {
            Ok(packet) => return Ok(packet),
            Err(Error::InsufficientBytes(_)) => {
                if stream.read_buf(buffer).await? == 0 {
                    return Err("connection closed before the next MQTT packet".into());
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn write_packet(stream: &mut TcpStream, packet: Packet) -> turmoil::Result {
    let mut buffer = BytesMut::new();
    packet.write(&mut buffer, MAX_PACKET_SIZE)?;
    stream.write_all(&buffer).await?;
    Ok(())
}

async fn happy_path_broker() -> turmoil::Result {
    let listener = TcpListener::bind(("0.0.0.0", PORT)).await?;
    let (mut stream, _) = listener.accept().await?;
    let mut buffer = BytesMut::new();

    loop {
        match read_packet(&mut stream, &mut buffer).await? {
            Packet::Connect(_) => {
                write_packet(
                    &mut stream,
                    Packet::ConnAck(ConnAck::new(ConnectReturnCode::Success, false)),
                )
                .await?;
            }
            Packet::Subscribe(subscribe) => {
                write_packet(
                    &mut stream,
                    Packet::SubAck(SubAck::new(
                        subscribe.pkid,
                        vec![SubscribeReasonCode::Success(QoS::AtLeastOnce)],
                    )),
                )
                .await?;
                write_packet(
                    &mut stream,
                    Packet::Publish(Publish::new(TOPIC, QoS::AtMostOnce, PAYLOAD)),
                )
                .await?;
            }
            Packet::PingReq => write_packet(&mut stream, Packet::PingResp).await?,
            _ => {}
        }
    }
}

async fn happy_path_client() -> turmoil::Result {
    let options = mqtt_options("turmoil-v4-happy-path");
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
    client.subscribe(TOPIC, QoS::AtLeastOnce).await?;

    loop {
        if let Event::Incoming(Packet::Publish(publish)) = eventloop.poll().await? {
            assert_eq!(&publish.topic[..], TOPIC.as_bytes());
            assert_eq!(&publish.payload[..], PAYLOAD);
            return Ok(());
        }
    }
}

#[test]
fn connect_subscribe_and_receive_over_turmoil() {
    let mut builder = turmoil::Builder::new();
    builder
        .rng_seed(1)
        .simulation_duration(Duration::from_secs(30));
    let mut sim = builder.build();
    sim.host("broker", happy_path_broker);
    sim.client("client", happy_path_client());
    sim.run().unwrap();
}

fn step_until(sim: &mut turmoil::Sim<'_>, condition: impl Fn() -> bool, description: &str) {
    while !condition() {
        assert!(
            !sim.step().unwrap(),
            "simulation completed before {description}"
        );
    }
}

#[test]
fn broker_crash_replays_unacknowledged_qos1_publish() {
    let first_publish_received = Rc::new(Cell::new(false));
    let connection_lost = Rc::new(Cell::new(false));
    let connection_count = Rc::new(Cell::new(0_u8));
    let original_pkid = Rc::new(Cell::new(0_u16));
    let replay_received = Rc::new(Cell::new(false));

    let mut builder = turmoil::Builder::new();
    builder
        .rng_seed(2)
        .simulation_duration(Duration::from_secs(30));
    let mut sim = builder.build();

    let broker_first_publish = Rc::clone(&first_publish_received);
    let broker_connection_count = Rc::clone(&connection_count);
    let broker_original_pkid = Rc::clone(&original_pkid);
    let broker_replay_received = Rc::clone(&replay_received);
    sim.host("broker", move || {
        let first_publish_received = Rc::clone(&broker_first_publish);
        let connection_count = Rc::clone(&broker_connection_count);
        let original_pkid = Rc::clone(&broker_original_pkid);
        let replay_received = Rc::clone(&broker_replay_received);
        async move {
            let attempt = connection_count.get() + 1;
            connection_count.set(attempt);

            let listener = TcpListener::bind(("0.0.0.0", PORT)).await?;
            let (mut stream, _) = listener.accept().await?;
            let mut buffer = BytesMut::new();

            let Packet::Connect(connect) = read_packet(&mut stream, &mut buffer).await? else {
                return Err("broker expected CONNECT".into());
            };
            assert!(!connect.clean_session);
            write_packet(
                &mut stream,
                Packet::ConnAck(ConnAck::new(ConnectReturnCode::Success, attempt > 1)),
            )
            .await?;

            loop {
                match read_packet(&mut stream, &mut buffer).await? {
                    Packet::Publish(publish) => {
                        assert_eq!(&publish.topic[..], TOPIC.as_bytes());
                        assert_eq!(&publish.payload[..], PAYLOAD);
                        assert_eq!(publish.qos, QoS::AtLeastOnce);

                        if attempt == 1 {
                            assert!(!publish.dup);
                            original_pkid.set(publish.pkid);
                            first_publish_received.set(true);
                            std::future::pending::<()>().await;
                        }

                        assert!(publish.dup);
                        assert_eq!(publish.pkid, original_pkid.get());
                        replay_received.set(true);
                        write_packet(&mut stream, Packet::PubAck(PubAck::new(publish.pkid)))
                            .await?;
                        // A Turmoil host may outlive the client that determines simulation
                        // completion. Keep the connection open so the acknowledgement is not
                        // discarded with the host future on the same simulation tick.
                        std::future::pending::<()>().await;
                    }
                    Packet::PingReq => write_packet(&mut stream, Packet::PingResp).await?,
                    _ => {}
                }
            }
        }
    });

    let client_connection_lost = Rc::clone(&connection_lost);
    let client_original_pkid = Rc::clone(&original_pkid);
    sim.client("client", async move {
        let mut options = mqtt_options("turmoil-v4-reconnect");
        options.set_clean_session(false).set_keep_alive(1);
        let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
        client
            .publish(TOPIC, PAYLOAD, PublishOptions::at_least_once())
            .await?;

        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::PubAck(puback))) => {
                    assert_eq!(puback.pkid, client_original_pkid.get());
                    return Ok(());
                }
                Ok(_) => {}
                Err(_) => client_connection_lost.set(true),
            }
        }
    });

    step_until(
        &mut sim,
        || first_publish_received.get(),
        "the broker received the first publish",
    );
    sim.crash("broker");
    step_until(
        &mut sim,
        || connection_lost.get(),
        "the client observed the crashed connection",
    );
    sim.bounce("broker");
    sim.run().unwrap_or_else(|error| {
        panic!(
            "simulation failed: {error}; connections={}, replay_received={}, connection_lost={}",
            connection_count.get(),
            replay_received.get(),
            connection_lost.get()
        )
    });

    assert_eq!(connection_count.get(), 2);
}
