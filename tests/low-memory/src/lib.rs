#![allow(async_fn_in_trait)]

use std::{
    env,
    error::Error,
    fmt,
    io::{self, Write},
    time::Duration,
};
use tokio::time::timeout;

pub mod stability;

pub const REQUEST_CAPACITY: usize = 4;
pub const MAX_REQUEST_BATCH: usize = 4;
pub const READ_BATCH_SIZE: usize = 4;
pub const INFLIGHT: u16 = 4;
pub const MAX_PACKET_SIZE: usize = 1_024;
pub const SOCKET_BUFFER_SIZE: u32 = 4_096;
pub const KEEP_ALIVE_SECS: u16 = 3;
pub const CONNECTION_TIMEOUT_SECS: u64 = 5;
pub const PAYLOAD_SIZE: usize = 128;
const INITIAL_MESSAGES: u32 = 32;
const RECONNECT_MESSAGES: u32 = 8;
const PHASE_TIMEOUT: Duration = Duration::from_secs(15);
const RECONNECT_BACKOFF: Duration = Duration::from_millis(100);

pub type BoxError = Box<dyn Error + Send + Sync>;

#[derive(Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub topic: String,
}

impl Config {
    pub fn from_env(protocol: &str) -> Result<Self, BoxError> {
        let host = env::var("MQTT_HOST").unwrap_or_else(|_| "broker".to_owned());
        let port = env::var("MQTT_PORT")
            .unwrap_or_else(|_| "1883".to_owned())
            .parse()?;
        let run_id = env::var("RUN_ID").unwrap_or_else(|_| "manual".to_owned());
        if !run_id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(failure("RUN_ID must contain only ASCII letters and digits"));
        }

        let client_id = format!("lm{protocol}{run_id}");
        if client_id.len() > 23 {
            return Err(failure("generated MQTT 3.1.1 client ID exceeds 23 bytes"));
        }

        Ok(Self {
            host,
            port,
            topic: format!("rumqtt/low-memory/{protocol}/{run_id}"),
            client_id,
        })
    }
}

#[derive(Debug)]
pub enum Observation {
    Connected,
    Subscribed,
    OutgoingPublish,
    PublishAck,
    IncomingPublish(Vec<u8>),
    PingRequest,
    PingResponse,
    Disconnected(String),
    GracefulDisconnect,
    Other,
}

pub trait Harness {
    fn topic(&self) -> &str;
    fn subscribe(&self) -> Result<(), BoxError>;
    fn publish(&self, payload: Vec<u8>) -> Result<(), BoxError>;
    fn disconnect(&self) -> Result<(), BoxError>;
    async fn poll(&mut self) -> Result<Observation, BoxError>;
}

pub async fn run<H: Harness>(harness: &mut H, protocol: &str) -> Result<(), BoxError> {
    wait_for_connection(harness, false).await?;
    subscribe(harness).await?;
    exchange(harness, 1, INITIAL_MESSAGES).await?;
    exercise_keep_alive(harness).await?;

    println!("phase=ready-for-network-loss protocol={protocol}");
    io::stdout().flush()?;

    wait_for_connection(harness, true).await?;
    subscribe(harness).await?;
    exchange(harness, 2, RECONNECT_MESSAGES).await?;
    graceful_disconnect(harness).await?;

    println!("result=pass protocol={protocol}");
    io::stdout().flush()?;
    tokio::time::sleep(Duration::from_millis(750)).await;
    Ok(())
}

async fn wait_for_connection<H: Harness>(
    harness: &mut H,
    reconnecting: bool,
) -> Result<(), BoxError> {
    timeout(PHASE_TIMEOUT, async {
        let mut saw_loss = !reconnecting;
        loop {
            match harness.poll().await? {
                Observation::Connected if saw_loss => return Ok(()),
                Observation::Connected => {
                    return Err(failure("reconnected before the forced connection loss"));
                }
                Observation::Disconnected(error) if reconnecting => {
                    saw_loss = true;
                    eprintln!("expected connection loss: {error}");
                    tokio::time::sleep(RECONNECT_BACKOFF).await;
                }
                Observation::Disconnected(error) => {
                    return Err(failure(format!("unexpected connection loss: {error}")));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| failure("timed out waiting for connection"))?
}

async fn subscribe<H: Harness>(harness: &mut H) -> Result<(), BoxError> {
    harness.subscribe()?;
    timeout(PHASE_TIMEOUT, async {
        loop {
            match harness.poll().await? {
                Observation::Subscribed => return Ok(()),
                Observation::Disconnected(error) => {
                    return Err(failure(format!(
                        "connection lost while subscribing: {error}"
                    )));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| failure("timed out waiting for SUBACK"))?
}

async fn exchange<H: Harness>(
    harness: &mut H,
    phase: u8,
    message_count: u32,
) -> Result<(), BoxError> {
    let mut next = 0;
    while next < message_count {
        let wave_end = (next + REQUEST_CAPACITY as u32).min(message_count);
        for index in next..wave_end {
            harness.publish(payload(phase, index))?;
        }
        receive_wave(harness, phase, next, wave_end).await?;
        next = wave_end;
    }
    Ok(())
}

async fn receive_wave<H: Harness>(
    harness: &mut H,
    phase: u8,
    start: u32,
    end: u32,
) -> Result<(), BoxError> {
    timeout(PHASE_TIMEOUT, async {
        let count = (end - start) as usize;
        let mut outgoing = 0;
        let mut acknowledgements = 0;
        let mut received = vec![false; count];

        while outgoing < count || acknowledgements < count || received.iter().any(|seen| !seen) {
            match harness.poll().await? {
                Observation::OutgoingPublish => outgoing += 1,
                Observation::PublishAck => acknowledgements += 1,
                Observation::IncomingPublish(bytes) => {
                    let index = validate_payload(&bytes, phase)?;
                    if !(start..end).contains(&index) {
                        return Err(failure(format!(
                            "received message index {index} outside expected wave {start}..{end}"
                        )));
                    }
                    let slot = &mut received[(index - start) as usize];
                    if *slot {
                        return Err(failure(format!("received duplicate message index {index}")));
                    }
                    *slot = true;
                }
                Observation::Disconnected(error) => {
                    return Err(failure(format!(
                        "connection lost during message exchange: {error}"
                    )));
                }
                _ => {}
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| failure(format!("timed out receiving message wave {start}..{end}")))?
}

async fn exercise_keep_alive<H: Harness>(harness: &mut H) -> Result<(), BoxError> {
    timeout(PHASE_TIMEOUT, async {
        let mut request = false;
        loop {
            match harness.poll().await? {
                Observation::PingRequest => request = true,
                Observation::PingResponse if request => return Ok(()),
                Observation::Disconnected(error) => {
                    return Err(failure(format!(
                        "connection lost while waiting for keep-alive: {error}"
                    )));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| failure("timed out waiting for PINGREQ/PINGRESP"))?
}

async fn graceful_disconnect<H: Harness>(harness: &mut H) -> Result<(), BoxError> {
    harness.disconnect()?;
    timeout(PHASE_TIMEOUT, async {
        loop {
            match harness.poll().await? {
                Observation::GracefulDisconnect => return Ok(()),
                Observation::Disconnected(error) => {
                    return Err(failure(format!(
                        "connection lost before graceful DISCONNECT: {error}"
                    )));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| failure("timed out waiting for graceful DISCONNECT"))?
}

fn payload(phase: u8, index: u32) -> Vec<u8> {
    let mut bytes = vec![0; PAYLOAD_SIZE];
    bytes[0] = phase;
    bytes[1..5].copy_from_slice(&index.to_be_bytes());
    for (offset, byte) in bytes[5..].iter_mut().enumerate() {
        *byte = phase ^ (index as u8) ^ (offset as u8);
    }
    bytes
}

fn validate_payload(bytes: &[u8], phase: u8) -> Result<u32, BoxError> {
    if bytes.len() != PAYLOAD_SIZE || bytes[0] != phase {
        return Err(failure("received payload has an unexpected size or phase"));
    }
    let index = u32::from_be_bytes(bytes[1..5].try_into()?);
    if bytes[5..]
        .iter()
        .enumerate()
        .any(|(offset, byte)| *byte != phase ^ (index as u8) ^ (offset as u8))
    {
        return Err(failure("received payload contents failed validation"));
    }
    Ok(index)
}

#[derive(Debug)]
struct Failure(String);

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for Failure {}

fn failure(message: impl Into<String>) -> BoxError {
    Box::new(Failure(message.into()))
}
