use anyhow::{Context, bail};
use std::sync::{
    Arc,
    atomic::{AtomicU16, AtomicU64, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};

const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    V4,
    V5,
}

#[derive(Clone)]
struct Subscription {
    connection_id: u64,
    protocol: Protocol,
    filter: String,
    qos: u8,
    sender: mpsc::UnboundedSender<Vec<u8>>,
}

struct Router {
    subscriptions: Mutex<Vec<Subscription>>,
    next_connection_id: AtomicU64,
    next_packet_id: AtomicU16,
}

impl Router {
    fn new() -> Self {
        Self {
            subscriptions: Mutex::new(Vec::new()),
            next_connection_id: AtomicU64::new(1),
            next_packet_id: AtomicU16::new(1),
        }
    }

    fn packet_id(&self) -> u16 {
        loop {
            let packet_id = self.next_packet_id.fetch_add(1, Ordering::Relaxed);
            if packet_id != 0 {
                return packet_id;
            }
        }
    }
}

pub async fn run(listener: TcpListener) -> anyhow::Result<()> {
    let router = Arc::new(Router::new());
    loop {
        let (stream, _) = listener.accept().await?;
        let router = Arc::clone(&router);
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, router).await {
                eprintln!("synthetic router connection ended: {error:#}");
            }
        });
    }
}

async fn handle_connection(stream: TcpStream, router: Arc<Router>) -> anyhow::Result<()> {
    let connection_id = router.next_connection_id.fetch_add(1, Ordering::Relaxed);
    let (mut reader, mut writer) = stream.into_split();
    let (sender, mut receiver) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = receiver.recv().await {
            writer.write_all(&frame).await?;
        }
        Ok::<_, std::io::Error>(())
    });

    let result = async {
        let connect = match read_frame(&mut reader).await {
            Ok(frame) => frame,
            Err(error) if is_clean_eof(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        if connect.packet_type() != 1 {
            bail!("first packet was not CONNECT");
        }
        let protocol = connect_protocol(&connect.body)?;
        sender
            .send(match protocol {
                Protocol::V4 => vec![0x20, 0x02, 0x00, 0x00],
                Protocol::V5 => vec![0x20, 0x03, 0x00, 0x00, 0x00],
            })
            .context("connection writer stopped")?;

        loop {
            let frame = match read_frame(&mut reader).await {
                Ok(frame) => frame,
                Err(error) if is_clean_eof(&error) => break,
                Err(error) => return Err(error),
            };
            match frame.packet_type() {
                3 => handle_publish(&router, protocol, &frame, &sender).await?,
                4 => {}
                8 => handle_subscribe(&router, connection_id, protocol, &frame, &sender).await?,
                12 => sender
                    .send(vec![0xd0, 0x00])
                    .context("connection writer stopped")?,
                14 => break,
                packet_type => bail!("unsupported MQTT packet type {packet_type}"),
            }
        }
        Ok(())
    }
    .await;

    router
        .subscriptions
        .lock()
        .await
        .retain(|subscription| subscription.connection_id != connection_id);
    drop(sender);
    match writer_task.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) if result.is_ok() => return Err(error.into()),
        Err(error) if result.is_ok() => return Err(error.into()),
        _ => {}
    }
    result
}

fn is_clean_eof(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
        )
    })
}

async fn handle_subscribe(
    router: &Router,
    connection_id: u64,
    protocol: Protocol,
    frame: &Frame,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
) -> anyhow::Result<()> {
    let (packet_id, filters) = parse_subscribe(protocol, &frame.body)?;
    let mut subscriptions = router.subscriptions.lock().await;
    for (filter, qos) in &filters {
        subscriptions.push(Subscription {
            connection_id,
            protocol,
            filter: filter.clone(),
            qos: *qos,
            sender: sender.clone(),
        });
    }
    drop(subscriptions);

    let mut body = Vec::with_capacity(3 + filters.len());
    body.extend_from_slice(&packet_id.to_be_bytes());
    if protocol == Protocol::V5 {
        body.push(0);
    }
    body.extend(filters.iter().map(|(_, qos)| *qos));
    sender
        .send(frame_with_body(0x90, &body))
        .context("connection writer stopped")
}

async fn handle_publish(
    router: &Router,
    protocol: Protocol,
    frame: &Frame,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
) -> anyhow::Result<()> {
    let incoming_qos = (frame.first_byte >> 1) & 0x03;
    if incoming_qos > 1 {
        bail!("synthetic router supports only QoS 0 and QoS 1");
    }
    let publish = parse_publish(protocol, incoming_qos, &frame.body)?;
    let subscriptions = router.subscriptions.lock().await.clone();
    for subscription in subscriptions {
        if !rumqttc_v4::mqttbytes::matches(&publish.topic, &subscription.filter) {
            continue;
        }
        let qos = incoming_qos.min(subscription.qos);
        let packet_id = (qos != 0).then(|| router.packet_id());
        let outgoing = encode_publish(
            subscription.protocol,
            frame.first_byte & 0x01 != 0,
            qos,
            packet_id,
            &publish.topic,
            &publish.payload,
        );
        let _ = subscription.sender.send(outgoing);
    }

    if let Some(packet_id) = publish.packet_id {
        sender
            .send(frame_with_body(0x40, &packet_id.to_be_bytes()))
            .context("connection writer stopped")?;
    }
    Ok(())
}

struct Frame {
    first_byte: u8,
    body: Vec<u8>,
}

impl Frame {
    const fn packet_type(&self) -> u8 {
        self.first_byte >> 4
    }
}

async fn read_frame(reader: &mut tokio::net::tcp::OwnedReadHalf) -> anyhow::Result<Frame> {
    let first_byte = reader.read_u8().await?;
    let mut remaining_len = 0usize;
    let mut multiplier = 1usize;
    for index in 0..4 {
        let encoded = reader.read_u8().await?;
        remaining_len = remaining_len
            .checked_add(usize::from(encoded & 0x7f) * multiplier)
            .context("MQTT remaining length overflow")?;
        if encoded & 0x80 == 0 {
            if remaining_len > MAX_FRAME_SIZE {
                bail!("MQTT frame exceeds synthetic router limit");
            }
            let mut body = vec![0; remaining_len];
            reader.read_exact(&mut body).await?;
            return Ok(Frame { first_byte, body });
        }
        if index == 3 {
            break;
        }
        multiplier *= 128;
    }
    bail!("malformed MQTT remaining length")
}

fn connect_protocol(body: &[u8]) -> anyhow::Result<Protocol> {
    if body.len() < 7 || &body[..6] != b"\0\x04MQTT" {
        bail!("unsupported MQTT CONNECT protocol name");
    }
    match body[6] {
        4 => Ok(Protocol::V4),
        5 => Ok(Protocol::V5),
        level => bail!("unsupported MQTT protocol level {level}"),
    }
}

fn parse_subscribe(protocol: Protocol, body: &[u8]) -> anyhow::Result<(u16, Vec<(String, u8)>)> {
    let packet_id = read_u16(body, 0)?;
    if packet_id == 0 {
        bail!("SUBSCRIBE packet identifier is zero");
    }
    let mut cursor = 2;
    if protocol == Protocol::V5 {
        let (properties_len, consumed) = read_variable_integer(&body[cursor..])?;
        cursor = cursor
            .checked_add(consumed + properties_len)
            .context("SUBSCRIBE properties overflow")?;
    }
    let mut filters = Vec::new();
    while cursor < body.len() {
        let filter = read_utf8(body, &mut cursor)?.to_owned();
        let options = *body.get(cursor).context("missing SUBSCRIBE options")?;
        cursor += 1;
        let qos = options & 0x03;
        if qos > 1 {
            bail!("synthetic router supports only QoS 0 and QoS 1 subscriptions");
        }
        filters.push((filter, qos));
    }
    if filters.is_empty() {
        bail!("SUBSCRIBE contains no filters");
    }
    Ok((packet_id, filters))
}

struct Publish {
    topic: String,
    packet_id: Option<u16>,
    payload: Vec<u8>,
}

fn parse_publish(protocol: Protocol, qos: u8, body: &[u8]) -> anyhow::Result<Publish> {
    let mut cursor = 0;
    let topic = read_utf8(body, &mut cursor)?.to_owned();
    let packet_id = if qos == 0 {
        None
    } else {
        let packet_id = read_u16(body, cursor)?;
        if packet_id == 0 {
            bail!("PUBLISH packet identifier is zero");
        }
        cursor += 2;
        Some(packet_id)
    };
    if protocol == Protocol::V5 {
        let (properties_len, consumed) = read_variable_integer(&body[cursor..])?;
        cursor = cursor
            .checked_add(consumed + properties_len)
            .context("PUBLISH properties overflow")?;
    }
    let payload = body
        .get(cursor..)
        .context("PUBLISH properties exceed packet body")?
        .to_vec();
    Ok(Publish {
        topic,
        packet_id,
        payload,
    })
}

fn encode_publish(
    protocol: Protocol,
    retain: bool,
    qos: u8,
    packet_id: Option<u16>,
    topic: &str,
    payload: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(topic.len() + payload.len() + 5);
    body.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    if let Some(packet_id) = packet_id {
        body.extend_from_slice(&packet_id.to_be_bytes());
    }
    if protocol == Protocol::V5 {
        body.push(0);
    }
    body.extend_from_slice(payload);
    frame_with_body(0x30 | (qos << 1) | u8::from(retain), &body)
}

fn frame_with_body(first_byte: u8, body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(body.len() + 5);
    frame.push(first_byte);
    write_variable_integer(body.len(), &mut frame);
    frame.extend_from_slice(body);
    frame
}

fn write_variable_integer(mut value: usize, output: &mut Vec<u8>) {
    loop {
        let mut encoded = (value % 128) as u8;
        value /= 128;
        if value != 0 {
            encoded |= 0x80;
        }
        output.push(encoded);
        if value == 0 {
            break;
        }
    }
}

fn read_variable_integer(input: &[u8]) -> anyhow::Result<(usize, usize)> {
    let mut value = 0usize;
    let mut multiplier = 1usize;
    for (index, encoded) in input.iter().copied().take(4).enumerate() {
        value += usize::from(encoded & 0x7f) * multiplier;
        if encoded & 0x80 == 0 {
            return Ok((value, index + 1));
        }
        multiplier *= 128;
    }
    bail!("malformed or incomplete MQTT variable integer")
}

fn read_u16(input: &[u8], cursor: usize) -> anyhow::Result<u16> {
    let bytes: [u8; 2] = input
        .get(cursor..cursor + 2)
        .context("missing MQTT two-byte integer")?
        .try_into()
        .expect("slice length was checked");
    Ok(u16::from_be_bytes(bytes))
}

fn read_utf8<'a>(input: &'a [u8], cursor: &mut usize) -> anyhow::Result<&'a str> {
    let len = usize::from(read_u16(input, *cursor)?);
    *cursor += 2;
    let end = cursor
        .checked_add(len)
        .context("MQTT string length overflow")?;
    let bytes = input.get(*cursor..end).context("incomplete MQTT string")?;
    *cursor = end;
    std::str::from_utf8(bytes).context("MQTT string is not UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v4_and_v5_subscriptions() {
        let v4 = [0, 7, 0, 3, b'a', b'/', b'#', 1];
        assert_eq!(
            parse_subscribe(Protocol::V4, &v4).unwrap(),
            (7, vec![("a/#".to_owned(), 1)])
        );
        let v5 = [0, 8, 0, 0, 3, b'a', b'/', b'+', 0];
        assert_eq!(
            parse_subscribe(Protocol::V5, &v5).unwrap(),
            (8, vec![("a/+".to_owned(), 0)])
        );
    }

    #[test]
    fn encodes_protocol_specific_publish_fields() {
        assert_eq!(
            encode_publish(Protocol::V4, false, 0, None, "a", b"x"),
            [0x30, 4, 0, 1, b'a', b'x']
        );
        assert_eq!(
            encode_publish(Protocol::V5, false, 1, Some(9), "a", b"x"),
            [0x32, 7, 0, 1, b'a', 0, 9, 0, b'x']
        );
    }
}
