use std::{
    env,
    io::{self, Write},
    time::{Duration, Instant},
};

use tokio::time::timeout;

use crate::{
    BoxError, CONNECTION_TIMEOUT_SECS, INFLIGHT, KEEP_ALIVE_SECS, MAX_PACKET_SIZE,
    MAX_REQUEST_BATCH, PAYLOAD_SIZE, READ_BATCH_SIZE, REQUEST_CAPACITY, SOCKET_BUFFER_SIZE,
    failure,
};

pub const DEFAULT_WARMUP_ROUNDS: u16 = 5;
pub const DEFAULT_MEASURED_ROUNDS: u16 = 20;
pub const DEFAULT_MESSAGES_PER_ROUND: u32 = 5_000;
pub const RESTART_EVERY_MEASURED_ROUNDS: u16 = 5;
pub const ROUND_BOUNDARY_HOLD: Duration = Duration::from_secs(1);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(15);
const ROUND_TIMEOUT: Duration = Duration::from_secs(60);
const RECONNECT_BACKOFF: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub primary_topic: String,
    pub temporary_topic: String,
    pub warmup_rounds: u16,
    pub measured_rounds: u16,
    pub messages_per_round: u32,
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

        let client_id = format!("ms{protocol}{run_id}");
        if client_id.len() > 23 {
            return Err(failure("generated MQTT 3.1.1 client ID exceeds 23 bytes"));
        }

        let warmup_rounds = parse_positive_env("WARMUP_ROUNDS", DEFAULT_WARMUP_ROUNDS)?;
        let measured_rounds = parse_positive_env("MEASURED_ROUNDS", DEFAULT_MEASURED_ROUNDS)?;
        let messages_per_round =
            parse_positive_env("MESSAGES_PER_ROUND", DEFAULT_MESSAGES_PER_ROUND)?;
        if measured_rounds < 10 {
            return Err(failure("MEASURED_ROUNDS must be at least 10"));
        }

        let primary_topic = format!("rumqtt/memory-stability/{protocol}/{run_id}/primary");
        let temporary_topic = format!("rumqtt/memory-stability/{protocol}/{run_id}/temporary");
        Ok(Self {
            host,
            port,
            client_id,
            primary_topic,
            temporary_topic,
            warmup_rounds,
            measured_rounds,
            messages_per_round,
        })
    }
}

fn parse_positive_env<T>(name: &str, default: T) -> Result<T, BoxError>
where
    T: Copy + std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
    T: PartialEq + From<u8>,
{
    let value = match env::var(name) {
        Ok(value) => value.parse()?,
        Err(_) => default,
    };
    if value == T::from(0) {
        return Err(failure(format!("{name} must be positive")));
    }
    Ok(value)
}

#[derive(Debug)]
pub enum Observation {
    Connected,
    Subscribed,
    Unsubscribed,
    OutgoingPublish,
    OutgoingPublishAck,
    PublishAck,
    IncomingPublish { topic: Vec<u8>, payload: Vec<u8> },
    PingRequest,
    PingResponse,
    Disconnected(String),
    GracefulDisconnect,
    Other,
}

#[derive(Debug)]
pub struct IdleState {
    pub idle: bool,
    pub detail: String,
}

pub trait Harness {
    fn primary_topic(&self) -> &str;
    fn temporary_topic(&self) -> &str;
    fn subscribe(&self, topic: &str) -> Result<(), BoxError>;
    fn unsubscribe(&self, topic: &str) -> Result<(), BoxError>;
    fn publish(&self, payload: Vec<u8>) -> Result<(), BoxError>;
    fn disconnect(&self) -> Result<(), BoxError>;
    fn idle_state(&self) -> IdleState;
    async fn poll(&mut self) -> Result<Observation, BoxError>;
}

#[derive(Default)]
struct Counters {
    published: u64,
    publish_acks: u64,
    echoes: u64,
    echo_acks: u64,
    reconnects: u16,
    subscriptions: u16,
    unsubscriptions: u16,
}

#[allow(clippy::future_not_send)]
pub async fn run<H: Harness>(
    harness: &mut H,
    protocol: &str,
    config: &Config,
) -> Result<(), BoxError> {
    let scenario_started = Instant::now();
    let mut counters = Counters::default();

    wait_for_connection(harness, false).await?;
    subscribe(harness, harness.primary_topic().to_owned()).await?;
    counters.subscriptions += 1;

    for round in 1..=config.warmup_rounds {
        run_round(
            harness,
            "warmup",
            1,
            round,
            config.messages_per_round,
            &mut counters,
        )
        .await?;

        if round == 1 {
            exercise_keep_alive(harness).await?;
        }
        if round == config.warmup_rounds {
            reconnect(harness, protocol, "warmup", round, &mut counters).await?;
        }
        finish_round(harness, "warmup", round, scenario_started).await?;
    }

    for round in 1..=config.measured_rounds {
        run_round(
            harness,
            "measured",
            2,
            round,
            config.messages_per_round,
            &mut counters,
        )
        .await?;

        if round % RESTART_EVERY_MEASURED_ROUNDS == 0 {
            reconnect(harness, protocol, "measured", round, &mut counters).await?;
        }
        finish_round(harness, "measured", round, scenario_started).await?;
    }

    graceful_disconnect(harness).await?;
    println!(
        "result=pass protocol={protocol} published={} pubacks={} echoes={} echo_pubacks={} \
         reconnects={} subscriptions={} unsubscriptions={} duration_ms={}",
        counters.published,
        counters.publish_acks,
        counters.echoes,
        counters.echo_acks,
        counters.reconnects,
        counters.subscriptions,
        counters.unsubscriptions,
        scenario_started.elapsed().as_millis()
    );
    io::stdout().flush()?;
    Ok(())
}

async fn run_round<H: Harness>(
    harness: &mut H,
    kind: &str,
    phase: u8,
    round: u16,
    messages: u32,
    counters: &mut Counters,
) -> Result<(), BoxError> {
    timeout(ROUND_TIMEOUT, async {
        subscribe(harness, harness.temporary_topic().to_owned()).await?;
        counters.subscriptions += 1;
        exchange(harness, phase, round, messages, counters).await?;
        unsubscribe(harness, harness.temporary_topic().to_owned()).await?;
        counters.unsubscriptions += 1;
        Ok::<(), BoxError>(())
    })
    .await
    .map_err(|_| failure(format!("timed out completing {kind} round {round}")))?
}

async fn reconnect<H: Harness>(
    harness: &mut H,
    protocol: &str,
    kind: &str,
    round: u16,
    counters: &mut Counters,
) -> Result<(), BoxError> {
    println!("control=restart-broker protocol={protocol} kind={kind} round={round}");
    io::stdout().flush()?;

    wait_for_connection(harness, true).await?;
    counters.reconnects += 1;
    subscribe(harness, harness.primary_topic().to_owned()).await?;
    counters.subscriptions += 1;
    Ok(())
}

#[allow(clippy::future_not_send)]
async fn finish_round<H: Harness>(
    harness: &H,
    kind: &str,
    round: u16,
    scenario_started: Instant,
) -> Result<(), BoxError> {
    let idle = harness.idle_state();
    if !idle.idle {
        return Err(failure(format!(
            "{kind} round {round} did not reach logical idle state: {}",
            idle.detail
        )));
    }

    println!(
        "phase=round-idle kind={kind} round={round} elapsed_ms={} idle=true {}",
        scenario_started.elapsed().as_millis(),
        idle.detail
    );
    io::stdout().flush()?;
    tokio::time::sleep(ROUND_BOUNDARY_HOLD).await;
    Ok(())
}

async fn wait_for_connection<H: Harness>(
    harness: &mut H,
    reconnecting: bool,
) -> Result<(), BoxError> {
    timeout(OPERATION_TIMEOUT, async {
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

async fn subscribe<H: Harness>(harness: &mut H, topic: String) -> Result<(), BoxError> {
    harness.subscribe(&topic)?;
    timeout(OPERATION_TIMEOUT, async {
        loop {
            match harness.poll().await? {
                Observation::Subscribed => return Ok(()),
                Observation::Disconnected(error) => {
                    return Err(failure(format!(
                        "connection lost while subscribing to {topic}: {error}"
                    )));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| failure(format!("timed out waiting for SUBACK for {topic}")))?
}

async fn unsubscribe<H: Harness>(harness: &mut H, topic: String) -> Result<(), BoxError> {
    harness.unsubscribe(&topic)?;
    timeout(OPERATION_TIMEOUT, async {
        loop {
            match harness.poll().await? {
                Observation::Unsubscribed => return Ok(()),
                Observation::Disconnected(error) => {
                    return Err(failure(format!(
                        "connection lost while unsubscribing from {topic}: {error}"
                    )));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| failure(format!("timed out waiting for UNSUBACK for {topic}")))?
}

async fn exchange<H: Harness>(
    harness: &mut H,
    phase: u8,
    round: u16,
    message_count: u32,
    counters: &mut Counters,
) -> Result<(), BoxError> {
    let mut next = 0;
    while next < message_count {
        let wave_end = (next + REQUEST_CAPACITY as u32).min(message_count);
        for index in next..wave_end {
            harness.publish(payload(phase, round, index))?;
        }
        receive_wave(harness, phase, round, next, wave_end, counters).await?;
        next = wave_end;
    }
    Ok(())
}

async fn receive_wave<H: Harness>(
    harness: &mut H,
    phase: u8,
    round: u16,
    start: u32,
    end: u32,
    counters: &mut Counters,
) -> Result<(), BoxError> {
    timeout(OPERATION_TIMEOUT, async {
        let count = (end - start) as usize;
        let mut outgoing = 0;
        let mut acknowledgements = 0;
        let mut echo_acknowledgements = 0;
        let mut received = vec![false; count];

        while outgoing < count
            || acknowledgements < count
            || echo_acknowledgements < count
            || received.iter().any(|seen| !seen)
        {
            match harness.poll().await? {
                Observation::OutgoingPublish => {
                    outgoing += 1;
                    counters.published += 1;
                }
                Observation::PublishAck => {
                    acknowledgements += 1;
                    counters.publish_acks += 1;
                }
                Observation::OutgoingPublishAck => {
                    echo_acknowledgements += 1;
                    counters.echo_acks += 1;
                }
                Observation::IncomingPublish {
                    topic,
                    payload: bytes,
                } => {
                    if topic != harness.primary_topic().as_bytes() {
                        return Err(failure(format!(
                            "received publish on unexpected topic {:?}",
                            String::from_utf8_lossy(&topic)
                        )));
                    }
                    let index = validate_payload(&bytes, phase, round)?;
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
                    counters.echoes += 1;
                }
                Observation::Disconnected(error) => {
                    return Err(failure(format!(
                        "connection lost during message exchange: {error}"
                    )));
                }
                _ => {}
            }

            if outgoing > count || acknowledgements > count || echo_acknowledgements > count {
                return Err(failure("received more protocol completions than expected"));
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| {
        failure(format!(
            "timed out receiving round {round} message wave {start}..{end}"
        ))
    })?
}

async fn exercise_keep_alive<H: Harness>(harness: &mut H) -> Result<(), BoxError> {
    timeout(OPERATION_TIMEOUT, async {
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
    timeout(OPERATION_TIMEOUT, async {
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

fn payload(phase: u8, round: u16, index: u32) -> Vec<u8> {
    let mut bytes = vec![0; PAYLOAD_SIZE];
    bytes[0] = phase;
    bytes[1..3].copy_from_slice(&round.to_be_bytes());
    bytes[3..7].copy_from_slice(&index.to_be_bytes());
    for (offset, byte) in bytes[7..].iter_mut().enumerate() {
        *byte = phase ^ (round as u8) ^ (index as u8) ^ (offset as u8);
    }
    bytes
}

fn validate_payload(bytes: &[u8], phase: u8, round: u16) -> Result<u32, BoxError> {
    if bytes.len() != PAYLOAD_SIZE || bytes[0] != phase {
        return Err(failure("received payload has an unexpected size or phase"));
    }
    let actual_round = u16::from_be_bytes(bytes[1..3].try_into()?);
    if actual_round != round {
        return Err(failure(format!(
            "received payload for round {actual_round}, expected {round}"
        )));
    }
    let index = u32::from_be_bytes(bytes[3..7].try_into()?);
    if bytes[7..]
        .iter()
        .enumerate()
        .any(|(offset, byte)| *byte != phase ^ (round as u8) ^ (index as u8) ^ (offset as u8))
    {
        return Err(failure("received payload contents failed validation"));
    }
    Ok(index)
}

pub fn settings_summary() -> String {
    format!(
        "request_capacity={REQUEST_CAPACITY} inflight={INFLIGHT} max_request_batch={MAX_REQUEST_BATCH} \
         read_batch_size={READ_BATCH_SIZE} max_packet_size={MAX_PACKET_SIZE} \
         socket_buffer_size={SOCKET_BUFFER_SIZE} keep_alive_secs={KEEP_ALIVE_SECS} \
         connection_timeout_secs={CONNECTION_TIMEOUT_SECS} payload_size={PAYLOAD_SIZE}"
    )
}
