use std::{io, net::SocketAddr};

use bytes::BytesMut;
use log::*;
use tokio::net::TcpListener;

mod network;
mod protocol;
use network::Network;
use protocol::{v4, v5};

pub struct Config {
    pub addr: SocketAddr,
}

pub async fn run(config: Config) -> Result<(), Error> {
    let listener = TcpListener::bind(config.addr).await?;
    info!("router: listening on {}", config.addr);

    loop {
        let (stream, addr) = listener.accept().await?;
        info!("router: accepted connection from {}", addr);
        let (network, _) = match Network::read_connect(stream).await {
            Ok(v) => v,
            Err(e) => {
                error!("router: unable to read connect : {}", e);
                continue;
            }
        };
        info!("connection: sent connack");
        tokio::spawn(publisher_handle(network));
    }
}

async fn publisher_handle(mut network: Network) {
    let mut payload = BytesMut::with_capacity(2);
    v4::pingresp::write(&mut payload).unwrap();
    let pingresp_bytes = payload.split().freeze();

    loop {
        let packet = match network.poll().await {
            Ok(packet) => packet,
            Err(e) => {
                error!("connection: unable to read packet: {}", e);
                return;
            }
        };
        match packet {
            protocol::Packet::V4(packet) => match packet {
                v4::Packet::Disconnect => {
                    info!("connection: received disconnect, exiting");
                    return;
                }
                v4::Packet::PingReq => {
                    if let Err(e) = network.send_data(&pingresp_bytes).await {
                        error!("unable to send pingresp, exiting : {}", e);
                        return;
                    };
                }
                v4::Packet::Publish(publish) => {
                    let pkid = match publish.view_meta() {
                        Ok(v) => v.2,
                        Err(e) => {
                            error!("connection: malformed publish packet : {}", e);
                            continue;
                        }
                    };
                    payload.reserve(2);
                    v4::puback::write(pkid, &mut payload).unwrap();
                    if let Err(e) = network.send_data(&payload.split().freeze()).await {
                        error!("unable to send puback pkid = {}, exiting : {}", pkid, e);
                        return;
                    };
                }
                p => {
                    error!("connection: invalid packet {:?}", p);
                    continue;
                }
            },
            protocol::Packet::V5(packet) => match packet {
                v5::Packet::Disconnect => {
                    info!("connection: received disconnect, exiting");
                    return;
                }
                v5::Packet::PingReq => {
                    if let Err(e) = network.send_data(&pingresp_bytes).await {
                        error!("unable to send pingresp, exiting : {}", e);
                        return;
                    };
                }
                v5::Packet::Publish(publish) => {
                    let pkid = match publish.view_meta() {
                        Ok(v) => v.2,
                        Err(e) => {
                            error!("connection: malformed publish packet : {}", e);
                            continue;
                        }
                    };
                    payload.reserve(8);
                    v5::puback::write(pkid, v5::puback::PubAckReason::Success, None, &mut payload)
                        .unwrap();
                    if let Err(e) = network.send_data(&payload.split().freeze()).await {
                        error!("unable to send puback pkid = {}, exiting : {}", pkid, e);
                        return;
                    };
                }
                p => {
                    error!("connection: invalid packet {:?}", p);
                    continue;
                }
            },
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("MQTT : {0}")]
    MQTT(#[from] crate::protocol::Error),
    #[error("i/O : {0}")]
    IO(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use crate::protocol::{self, FixedHeader};

    #[test]
    fn v4_publish_view_topic_returns_malformed_packet_for_out_of_bounds_fixed_header() {
        let publish = protocol::v4::publish::Publish {
            fixed_header: FixedHeader::new(0x30, 0, 0),
            raw: Bytes::from_static(b""),
        };
        assert_eq!(publish.view_topic(), Err(protocol::Error::MalformedPacket));
    }

    #[test]
    fn v5_publish_view_meta_returns_malformed_packet_for_missing_pkid() {
        let publish = protocol::v5::publish::Publish {
            fixed_header: FixedHeader::new(0x32, 0, 0),
            raw: Bytes::from_static(b"\x32\x00\x01a"),
        };
        assert_eq!(publish.view_meta(), Err(protocol::Error::MalformedPacket));
    }

    #[test]
    fn v4_publish_view_meta_parses_valid_minimal_qos1_publish() {
        let publish = protocol::v4::publish::Publish {
            fixed_header: FixedHeader::new(0x32, 0, 0),
            raw: Bytes::from_static(b"\x32\x00\x01a\x00\x07"),
        };
        assert_eq!(publish.view_meta(), Ok(("a", 1, 7, false, false)));
    }
}
