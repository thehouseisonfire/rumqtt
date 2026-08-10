#![expect(clippy::cast_precision_loss)]
#![expect(clippy::too_many_lines)]
#![expect(clippy::too_many_arguments)]

use anyhow::{Context, bail};
use bytes::{Bytes, BytesMut};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{Duration, Instant};

mod nats_codec;

const BENCH_MAX_PACKET_SIZE: usize = 256 * 1024;

#[derive(Parser, Debug)]
#[command(name = "rumqtt-bench")]
#[command(about = "Maintained benchmark harness for rumqtt client and codec performance")]
struct Cli {
    #[command(subcommand)]
    command: CommandGroup,
}

#[derive(Subcommand, Debug)]
enum CommandGroup {
    Client {
        #[command(subcommand)]
        command: ClientCommand,
    },
    Codec {
        #[command(subcommand)]
        command: CodecCommand,
    },
    Options {
        #[command(subcommand)]
        command: OptionsCommand,
    },
}

#[derive(Subcommand, Debug)]
enum ClientCommand {
    Throughput(ClientThroughputArgs),
    Latency(ClientLatencyArgs),
    Connections(ClientConnectionArgs),
    PublishPath(ClientPublishPathArgs),
}

#[derive(Subcommand, Debug)]
enum CodecCommand {
    Encode(CodecArgs),
    Decode(CodecArgs),
    Roundtrip(CodecArgs),
    ValidationCost(CodecValidationArgs),
}

#[derive(Subcommand, Debug)]
enum OptionsCommand {
    ParseUrl(OptionsParseUrlArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Protocol {
    V4,
    V5,
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodecProtocol {
    V4,
    V5,
    Nats,
}

impl CodecProtocol {
    const fn as_str(self) -> &'static str {
        match self {
            Self::V4 => "v4",
            Self::V5 => "v5",
            Self::Nats => "nats",
        }
    }
}

impl Protocol {
    const fn as_str(self) -> &'static str {
        match self {
            Self::V4 => "v4",
            Self::V5 => "v5",
        }
    }
}

#[derive(Args, Debug, Clone)]
struct CommonArgs {
    #[arg(long, value_enum, default_value = "v5")]
    protocol: Protocol,

    #[arg(long)]
    run_id: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct ClientThroughputArgs {
    #[command(flatten)]
    common: CommonArgs,

    #[command(flatten)]
    client: ClientCommonArgs,

    #[arg(long, default_value = "1")]
    publishers: usize,

    #[arg(long, default_value = "1")]
    subscribers: usize,
}

#[derive(Args, Debug, Clone)]
struct ClientLatencyArgs {
    #[command(flatten)]
    common: CommonArgs,

    #[command(flatten)]
    client: ClientCommonArgs,

    #[arg(long, default_value = "1000")]
    rate: u64,
}

#[derive(Args, Debug, Clone)]
struct ClientConnectionArgs {
    #[command(flatten)]
    common: CommonArgs,

    #[arg(long, default_value = "mqtt://127.0.0.1:1883")]
    broker_url: String,

    #[arg(long, default_value = "10")]
    duration_sec: u64,

    #[arg(long, default_value = "10")]
    concurrency: usize,

    #[arg(long)]
    ca_cert: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
struct ClientPublishPathArgs {
    #[command(flatten)]
    common: CommonArgs,

    #[arg(long, default_value = "12")]
    rounds: usize,

    #[arg(long, default_value = "1")]
    warmup_rounds: usize,

    #[arg(long, default_value = "10000")]
    messages: usize,

    #[arg(long, default_value = "64")]
    payload_size: usize,

    #[arg(long, default_value = "bench/client")]
    topic: String,

    #[arg(long, default_value = "1", value_parser = parse_qos)]
    qos: u8,
}

#[derive(Args, Debug, Clone)]
struct ClientCommonArgs {
    #[arg(long, default_value = "mqtt://127.0.0.1:1883")]
    broker_url: String,

    #[arg(long, default_value = "10")]
    duration_sec: u64,

    #[arg(long, default_value = "2")]
    warmup_sec: u64,

    #[arg(long, default_value = "64")]
    payload_size: usize,

    #[arg(long, default_value = "bench/rumqtt")]
    topic: String,

    #[arg(long)]
    filter: Option<String>,

    #[arg(long, default_value = "1", value_parser = parse_qos)]
    qos: u8,

    #[arg(long)]
    ca_cert: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
struct CodecArgs {
    #[arg(long, value_enum, default_value = "v5")]
    protocol: CodecProtocol,

    #[arg(long)]
    run_id: Option<String>,

    #[arg(long, default_value = "100000")]
    messages: usize,

    #[arg(long, default_value = "64")]
    payload_size: usize,

    #[arg(long, default_value = "1", value_parser = parse_qos)]
    qos: u8,

    #[arg(long, default_value = "bench/codec")]
    topic: String,

    #[arg(long)]
    profile_output: Option<PathBuf>,

    #[arg(long, default_value = "100")]
    profile_frequency: i32,
}

#[derive(Args, Debug, Clone)]
struct CodecValidationArgs {
    #[arg(long, value_enum, default_value = "v5")]
    protocol: Protocol,

    #[arg(long)]
    run_id: Option<String>,

    #[arg(long, default_value = "10")]
    rounds: usize,

    #[arg(long, default_value = "100000")]
    messages: usize,

    #[arg(long, default_value = "64")]
    payload_size: usize,

    #[arg(long, default_value = "1", value_parser = parse_qos)]
    qos: u8,

    #[arg(long, default_value = "bench/codec")]
    topic: String,
}

#[derive(Args, Debug, Clone)]
struct OptionsParseUrlArgs {
    #[command(flatten)]
    common: CommonArgs,

    #[arg(long, default_value = "100000")]
    parses: usize,

    #[arg(long, default_value = "mqtt://localhost:1883?client_id=bench-url")]
    url: String,
}

#[derive(Debug, Clone)]
struct BrokerEndpoint {
    #[cfg_attr(not(feature = "websocket"), expect(dead_code))]
    url: String,
    host: String,
    port: u16,
    transport: TransportKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum TransportKind {
    Tcp,
    Tls,
    #[cfg_attr(not(feature = "websocket"), expect(dead_code))]
    Websocket,
}

impl TransportKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Tls => "tls",
            Self::Websocket => "websocket",
        }
    }
}

#[derive(Serialize)]
struct BenchOutput {
    schema_version: u32,
    run_id: String,
    scenario: String,
    started_at_unix: u64,
    finished_at_unix: u64,
    config: Value,
    metrics: BTreeMap<String, f64>,
    samples: BTreeMap<String, Vec<f64>>,
    environment: Environment,
}

#[derive(Serialize)]
struct Environment {
    git_commit: Option<String>,
    rustc: Option<String>,
    target: String,
    os: String,
    arch: String,
    cpu_count: usize,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CommandGroup::Client { command } => match command {
            ClientCommand::Throughput(args) => run_client_throughput(args).await,
            ClientCommand::Latency(args) => run_client_latency(args).await,
            ClientCommand::Connections(args) => run_client_connections(args).await,
            ClientCommand::PublishPath(args) => run_client_publish_path(args).await,
        },
        CommandGroup::Codec { command } => match command {
            CodecCommand::Encode(args) => run_codec(args, CodecMode::Encode),
            CodecCommand::Decode(args) => run_codec(args, CodecMode::Decode),
            CodecCommand::Roundtrip(args) => run_codec(args, CodecMode::Roundtrip),
            CodecCommand::ValidationCost(args) => run_codec_validation_cost(args),
        },
        CommandGroup::Options { command } => match command {
            OptionsCommand::ParseUrl(args) => run_options_parse_url(args),
        },
    }
}

#[derive(Debug, Clone, Copy)]
enum CodecMode {
    Encode,
    Decode,
    Roundtrip,
}

impl CodecMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Encode => "encode",
            Self::Decode => "decode",
            Self::Roundtrip => "roundtrip",
        }
    }
}

async fn read_benchmark_frame(
    stream: &mut tokio::io::DuplexStream,
) -> anyhow::Result<(u8, Vec<u8>)> {
    use tokio::io::AsyncReadExt;

    let byte1 = stream.read_u8().await?;
    let mut multiplier = 1_usize;
    let mut remaining = 0_usize;
    loop {
        let encoded = stream.read_u8().await?;
        remaining = remaining
            .checked_add(usize::from(encoded & 0x7f) * multiplier)
            .context("invalid MQTT remaining length")?;
        if encoded & 0x80 == 0 {
            break;
        }
        multiplier = multiplier
            .checked_mul(128)
            .context("invalid MQTT remaining length")?;
        if multiplier > 128 * 128 * 128 {
            bail!("malformed MQTT remaining length");
        }
    }
    let mut body = vec![0; remaining];
    stream.read_exact(&mut body).await?;
    Ok((byte1, body))
}

async fn run_publish_path_peer(
    mut stream: tokio::io::DuplexStream,
    protocol: Protocol,
    messages: usize,
    ready: tokio::sync::oneshot::Sender<()>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let (connect_header, _) = read_benchmark_frame(&mut stream).await?;
    if connect_header >> 4 != 1 {
        bail!("expected CONNECT as first benchmark frame");
    }
    match protocol {
        Protocol::V4 => stream.write_all(&[0x20, 0x02, 0x00, 0x00]).await?,
        Protocol::V5 => stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).await?,
    }
    stream.flush().await?;
    let _ = ready.send(());

    for _ in 0..messages {
        let (header, body) = read_benchmark_frame(&mut stream).await?;
        if header >> 4 != 3 || body.len() < 2 {
            bail!("expected PUBLISH benchmark frame");
        }
        let qos = (header >> 1) & 0x03;
        if qos != 0 {
            let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
            let pkid_offset = 2 + topic_len;
            if body.len() < pkid_offset + 2 {
                bail!("truncated PUBLISH packet identifier");
            }
            let pkid = [body[pkid_offset], body[pkid_offset + 1]];
            stream.write_all(&[0x40, 0x02, pkid[0], pkid[1]]).await?;
        }
    }
    stream.flush().await?;
    Ok(())
}

fn paired_bootstrap_interval(checked: &[f64], validated: &[f64]) -> (f64, f64, f64) {
    let deltas: Vec<f64> = checked
        .iter()
        .zip(validated)
        .map(|(checked, validated)| (checked / validated - 1.0) * 100.0)
        .collect();
    let point = median(&deltas);
    let mut state = 0x4d59_5df4_d0f3_3173_u64;
    let mut bootstrapped = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        let mut sample = Vec::with_capacity(deltas.len());
        for _ in 0..deltas.len() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            sample.push(deltas[(state as usize) % deltas.len()]);
        }
        bootstrapped.push(median(&sample));
    }
    bootstrapped.sort_by(f64::total_cmp);
    let lower = bootstrapped[bootstrapped.len() * 25 / 1000];
    let upper = bootstrapped[bootstrapped.len() * 975 / 1000];
    (point, lower, upper)
}

async fn run_client_publish_path(args: ClientPublishPathArgs) -> anyhow::Result<()> {
    if args.rounds == 0 || args.messages == 0 {
        bail!("--rounds and --messages must be greater than zero");
    }
    if args.qos != 0 && args.messages > usize::from(u16::MAX) {
        bail!("QoS 1 publish-path runs support at most 65,535 messages");
    }
    if args.topic.is_empty() {
        bail!("--topic must be non-empty");
    }

    let started_at = unix_secs();
    let run_id = run_id(args.common.run_id.as_deref(), "client-publish-path");
    let payload = Bytes::from(vec![0_u8; args.payload_size]);
    for _ in 0..args.warmup_rounds {
        std::hint::black_box(run_publish_path_variant(&args, payload.clone(), false).await?);
        std::hint::black_box(run_publish_path_variant(&args, payload.clone(), true).await?);
    }
    let mut checked = Vec::with_capacity(args.rounds);
    let mut validated = Vec::with_capacity(args.rounds);
    for round in 0..args.rounds {
        if round % 2 == 0 {
            checked.push(run_publish_path_variant(&args, payload.clone(), false).await?);
            validated.push(run_publish_path_variant(&args, payload.clone(), true).await?);
        } else {
            validated.push(run_publish_path_variant(&args, payload.clone(), true).await?);
            checked.push(run_publish_path_variant(&args, payload.clone(), false).await?);
        }
    }

    let checked_median = median(&checked);
    let validated_median = median(&validated);
    let (speedup, ci_lower, ci_upper) = paired_bootstrap_interval(&checked, &validated);
    let mut metrics = BTreeMap::new();
    metrics.insert("messages".to_owned(), args.messages as f64);
    metrics.insert("rounds".to_owned(), args.rounds as f64);
    metrics.insert("checked_median_sec".to_owned(), checked_median);
    metrics.insert("validated_median_sec".to_owned(), validated_median);
    metrics.insert("validated_speedup_percent".to_owned(), speedup);
    metrics.insert("validated_speedup_ci95_lower_percent".to_owned(), ci_lower);
    metrics.insert("validated_speedup_ci95_upper_percent".to_owned(), ci_upper);

    let mut samples = BTreeMap::new();
    samples.insert("checked_elapsed_sec".to_owned(), checked);
    samples.insert("validated_elapsed_sec".to_owned(), validated);
    print_output(BenchOutput {
        schema_version: 1,
        run_id,
        scenario: format!("client-{}-publish-path", args.common.protocol.as_str()),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "protocol": args.common.protocol,
            "rounds": args.rounds,
            "warmup_rounds": args.warmup_rounds,
            "messages": args.messages,
            "payload_size": args.payload_size,
            "topic": args.topic,
            "topic_len": args.topic.len(),
            "qos": args.qos,
            "sink": "in-process-duplex",
        }),
        metrics,
        samples,
        environment: environment(),
    })
}

async fn run_publish_path_variant(
    args: &ClientPublishPathArgs,
    payload: Bytes,
    validated: bool,
) -> anyhow::Result<f64> {
    match args.common.protocol {
        Protocol::V4 => run_v4_publish_path_variant(args, payload, validated).await,
        Protocol::V5 => run_v5_publish_path_variant(args, payload, validated).await,
    }
}

async fn run_v4_publish_path_variant(
    args: &ClientPublishPathArgs,
    payload: Bytes,
    validated: bool,
) -> anyhow::Result<f64> {
    let (client_stream, peer_stream) = tokio::io::duplex(BENCH_MAX_PACKET_SIZE);
    let available = Arc::new(Mutex::new(Some(client_stream)));
    let connector_stream = Arc::clone(&available);
    let mut options = rumqttc_v4::MqttOptions::new("publish-path-v4", "benchmark.invalid");
    options.set_inflight(args.messages.min(usize::from(u16::MAX)) as u16);
    options.set_socket_connector(move |_host, _network_options| {
        let stream = connector_stream
            .lock()
            .expect("connector lock poisoned")
            .take();
        async move { stream.ok_or_else(|| std::io::Error::other("connector already used")) }
    });
    let (client, mut eventloop) = rumqttc_v4::AsyncClient::builder(options)
        .capacity(args.messages)
        .build();
    let eventloop_task = tokio::spawn(async move {
        loop {
            eventloop.poll().await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), rumqttc_v4::ConnectionError>(())
    });
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let peer_task = tokio::spawn(run_publish_path_peer(
        peer_stream,
        Protocol::V4,
        args.messages,
        ready_tx,
    ));
    ready_rx.await?;
    let checked_topics = (!validated).then(|| vec![args.topic.clone(); args.messages]);
    let validated_topics = validated
        .then(|| {
            (0..args.messages)
                .map(|_| rumqttc_v4::ValidatedTopic::new(args.topic.clone()))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let started = Instant::now();
    if let Some(topics) = checked_topics {
        for topic in topics {
            client
                .publish(
                    topic,
                    payload.clone(),
                    rumqttc_v4::PublishOptions::new(v4_qos(args.qos)),
                )
                .await?;
        }
    } else if let Some(topics) = validated_topics {
        for topic in topics {
            client
                .publish(
                    topic,
                    payload.clone(),
                    rumqttc_v4::PublishOptions::new(v4_qos(args.qos)),
                )
                .await?;
        }
    }
    peer_task.await??;
    let elapsed = started.elapsed().as_secs_f64();
    eventloop_task.abort();
    Ok(elapsed)
}

async fn run_v5_publish_path_variant(
    args: &ClientPublishPathArgs,
    payload: Bytes,
    validated: bool,
) -> anyhow::Result<f64> {
    let (client_stream, peer_stream) = tokio::io::duplex(BENCH_MAX_PACKET_SIZE);
    let available = Arc::new(Mutex::new(Some(client_stream)));
    let connector_stream = Arc::clone(&available);
    let mut options = rumqttc_v5::MqttOptions::new("publish-path-v5", "benchmark.invalid");
    options.set_outgoing_inflight_upper_limit(args.messages.min(usize::from(u16::MAX)) as u16);
    options.set_socket_connector(move |_host, _network_options| {
        let stream = connector_stream
            .lock()
            .expect("connector lock poisoned")
            .take();
        async move { stream.ok_or_else(|| std::io::Error::other("connector already used")) }
    });
    let (client, mut eventloop) = rumqttc_v5::AsyncClient::builder(options)
        .capacity(args.messages)
        .build();
    let eventloop_task = tokio::spawn(async move {
        loop {
            eventloop.poll().await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), rumqttc_v5::ConnectionError>(())
    });
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let peer_task = tokio::spawn(run_publish_path_peer(
        peer_stream,
        Protocol::V5,
        args.messages,
        ready_tx,
    ));
    ready_rx.await?;
    let checked_topics = (!validated).then(|| vec![args.topic.clone(); args.messages]);
    let validated_topics = validated
        .then(|| {
            (0..args.messages)
                .map(|_| rumqttc_v5::ValidatedTopic::new(args.topic.clone()))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let started = Instant::now();
    let options = rumqttc_v5::PublishOptions::new(v5_qos(args.qos));
    if let Some(topics) = checked_topics {
        for topic in topics {
            client
                .publish(topic, payload.clone(), options.clone())
                .await?;
        }
    } else if let Some(topics) = validated_topics {
        for topic in topics {
            client
                .publish(topic, payload.clone(), options.clone())
                .await?;
        }
    }
    peer_task.await??;
    let elapsed = started.elapsed().as_secs_f64();
    eventloop_task.abort();
    Ok(elapsed)
}

async fn run_client_throughput(args: ClientThroughputArgs) -> anyhow::Result<()> {
    if args.publishers == 0 || args.subscribers == 0 {
        bail!("--publishers and --subscribers must be greater than zero");
    }

    let endpoint = parse_endpoint(&args.client.broker_url)?;
    let ca_pem = read_ca_cert(args.client.ca_cert.as_ref())?;
    let started_at = unix_secs();
    let run_id = run_id(args.common.run_id.as_deref(), "client-throughput");
    let received = Arc::new(AtomicU64::new(0));
    let published = Arc::new(AtomicU64::new(0));
    let running = Arc::new(AtomicBool::new(true));
    let filter = args
        .client
        .filter
        .clone()
        .unwrap_or_else(|| args.client.topic.clone());

    let mut abort_handles = Vec::new();
    match args.common.protocol {
        Protocol::V4 => {
            run_v4_subscribers(
                &mut abort_handles,
                args.subscribers,
                &run_id,
                &endpoint,
                ca_pem.as_deref(),
                &filter,
                args.client.qos,
                Arc::clone(&received),
                Arc::clone(&running),
            )
            .await?;
            run_v4_publishers(
                &mut abort_handles,
                args.publishers,
                &run_id,
                &endpoint,
                ca_pem.as_deref(),
                &args.client.topic,
                args.client.payload_size,
                args.client.qos,
                Arc::clone(&published),
                Arc::clone(&running),
            );
        }
        Protocol::V5 => {
            run_v5_subscribers(
                &mut abort_handles,
                args.subscribers,
                &run_id,
                &endpoint,
                ca_pem.as_deref(),
                &filter,
                args.client.qos,
                Arc::clone(&received),
                Arc::clone(&running),
            )
            .await?;
            run_v5_publishers(
                &mut abort_handles,
                args.publishers,
                &run_id,
                &endpoint,
                ca_pem.as_deref(),
                &args.client.topic,
                args.client.payload_size,
                args.client.qos,
                Arc::clone(&published),
                Arc::clone(&running),
            );
        }
    }

    tokio::time::sleep(Duration::from_secs(args.client.warmup_sec)).await;
    received.store(0, Ordering::SeqCst);
    published.store(0, Ordering::SeqCst);

    let rss_initial = resident_set_bytes();
    let mut rss_max = rss_initial;
    let measure_start = Instant::now();
    let mut last_received = 0;
    let mut next_sample = measure_start + Duration::from_secs(1);
    let mut samples = Vec::new();
    while measure_start.elapsed() < Duration::from_secs(args.client.duration_sec) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if Instant::now() >= next_sample {
            let current = received.load(Ordering::Relaxed);
            samples.push(current.saturating_sub(last_received) as f64);
            last_received = current;
            next_sample += Duration::from_secs(1);
            if let Some(current_rss) = resident_set_bytes() {
                rss_max = Some(rss_max.map_or(current_rss, |max| max.max(current_rss)));
            }
        }
    }

    running.store(false, Ordering::SeqCst);
    abort_all(abort_handles).await;

    let elapsed_sec = measure_start.elapsed().as_secs_f64();
    let total_published = published.load(Ordering::Relaxed);
    let total_received = received.load(Ordering::Relaxed);
    let mut metrics = BTreeMap::new();
    metrics.insert("published".to_owned(), total_published as f64);
    metrics.insert("received".to_owned(), total_received as f64);
    metrics.insert("elapsed_sec".to_owned(), elapsed_sec);
    metrics.insert(
        "throughput_msg_sec".to_owned(),
        total_received as f64 / elapsed_sec,
    );
    insert_throughput_stability_metrics(&mut metrics, &samples);
    if let (Some(initial), Some(max)) = (rss_initial, rss_max) {
        metrics.insert("rss_initial_bytes".to_owned(), initial as f64);
        metrics.insert("rss_max_bytes".to_owned(), max as f64);
        metrics.insert(
            "rss_growth_bytes".to_owned(),
            max.saturating_sub(initial) as f64,
        );
    }

    let mut samples_out = BTreeMap::new();
    samples_out.insert("received_per_sec".to_owned(), samples);

    print_output(BenchOutput {
        schema_version: 1,
        run_id,
        scenario: format!("client-throughput-{}", args.common.protocol.as_str()),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "protocol": args.common.protocol,
            "transport": endpoint.transport.as_str(),
            "broker_url": args.client.broker_url,
            "duration_sec": args.client.duration_sec,
            "warmup_sec": args.client.warmup_sec,
            "payload_size": args.client.payload_size,
            "topic": args.client.topic,
            "filter": filter,
            "qos": args.client.qos,
            "publishers": args.publishers,
            "subscribers": args.subscribers,
        }),
        metrics,
        samples: samples_out,
        environment: environment(),
    })
}

async fn run_client_latency(args: ClientLatencyArgs) -> anyhow::Result<()> {
    if args.rate == 0 {
        bail!("--rate must be greater than zero");
    }

    let endpoint = parse_endpoint(&args.client.broker_url)?;
    let ca_pem = read_ca_cert(args.client.ca_cert.as_ref())?;
    let started_at = unix_secs();
    let run_id = run_id(args.common.run_id.as_deref(), "client-latency");
    let filter = args
        .client
        .filter
        .clone()
        .unwrap_or_else(|| args.client.topic.clone());

    let running = Arc::new(AtomicBool::new(true));
    let latencies = Arc::new(Mutex::new(Vec::<u64>::new()));
    let mut handles = Vec::new();

    match args.common.protocol {
        Protocol::V4 => {
            let (pub_client, mut pub_eventloop) =
                new_v4_client(format!("{run_id}-pub"), &endpoint, ca_pem.as_deref(), 100);
            let pub_running = Arc::clone(&running);
            handles.push(tokio::spawn(async move {
                while pub_running.load(Ordering::Relaxed) {
                    if pub_eventloop.poll().await.is_err() {
                        break;
                    }
                }
            }));

            let (sub_client, mut sub_eventloop) =
                new_v4_client(format!("{run_id}-sub"), &endpoint, ca_pem.as_deref(), 100);
            sub_client
                .subscribe(filter, v4_qos(args.client.qos))
                .await?;
            let sub_running = Arc::clone(&running);
            let sub_latencies = Arc::clone(&latencies);
            handles.push(tokio::spawn(async move {
                while sub_running.load(Ordering::Relaxed) {
                    match sub_eventloop.poll().await {
                        Ok(rumqttc_v4::Event::Incoming(rumqttc_v4::Incoming::Publish(publish))) => {
                            record_latency_sample(&publish.payload, &sub_latencies);
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }));

            publish_latency_samples_v4(&pub_client, &args, args.client.warmup_sec).await?;
            latencies.lock().expect("latencies lock").clear();
            publish_latency_samples_v4(&pub_client, &args, args.client.duration_sec).await?;
            drop(pub_client.disconnect().await);
            drop(sub_client.disconnect().await);
        }
        Protocol::V5 => {
            let (pub_client, mut pub_eventloop) =
                new_v5_client(format!("{run_id}-pub"), &endpoint, ca_pem.as_deref(), 100);
            let pub_running = Arc::clone(&running);
            handles.push(tokio::spawn(async move {
                while pub_running.load(Ordering::Relaxed) {
                    if pub_eventloop.poll().await.is_err() {
                        break;
                    }
                }
            }));

            let (sub_client, mut sub_eventloop) =
                new_v5_client(format!("{run_id}-sub"), &endpoint, ca_pem.as_deref(), 100);
            sub_client
                .subscribe(filter, v5_qos(args.client.qos))
                .await?;
            let sub_running = Arc::clone(&running);
            let sub_latencies = Arc::clone(&latencies);
            handles.push(tokio::spawn(async move {
                while sub_running.load(Ordering::Relaxed) {
                    match sub_eventloop.poll().await {
                        Ok(rumqttc_v5::Event::Incoming(rumqttc_v5::Incoming::Publish(publish))) => {
                            record_latency_sample(&publish.payload, &sub_latencies);
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }));

            publish_latency_samples_v5(&pub_client, &args, args.client.warmup_sec).await?;
            latencies.lock().expect("latencies lock").clear();
            publish_latency_samples_v5(&pub_client, &args, args.client.duration_sec).await?;
            drop(pub_client.disconnect().await);
            drop(sub_client.disconnect().await);
        }
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    running.store(false, Ordering::SeqCst);
    abort_all(handles).await;

    let mut samples = latencies.lock().expect("latencies lock").clone();
    samples.sort_unstable();
    let mut metrics = BTreeMap::new();
    metrics.insert("messages".to_owned(), samples.len() as f64);
    insert_latency_metrics(&mut metrics, &samples);

    let mut samples_out = BTreeMap::new();
    samples_out.insert(
        "latency_us".to_owned(),
        downsample(&samples, 200)
            .into_iter()
            .map(|value| value as f64)
            .collect(),
    );

    print_output(BenchOutput {
        schema_version: 1,
        run_id,
        scenario: format!("client-latency-{}", args.common.protocol.as_str()),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "protocol": args.common.protocol,
            "transport": endpoint.transport.as_str(),
            "broker_url": args.client.broker_url,
            "duration_sec": args.client.duration_sec,
            "warmup_sec": args.client.warmup_sec,
            "payload_size": args.client.payload_size,
            "topic": args.client.topic,
            "filter": args.client.filter,
            "qos": args.client.qos,
            "rate": args.rate,
        }),
        metrics,
        samples: samples_out,
        environment: environment(),
    })
}

async fn run_client_connections(args: ClientConnectionArgs) -> anyhow::Result<()> {
    if args.concurrency == 0 {
        bail!("--concurrency must be greater than zero");
    }

    let endpoint = parse_endpoint(&args.broker_url)?;
    let ca_pem = read_ca_cert(args.ca_cert.as_ref())?;
    let started_at = unix_secs();
    let run_id = run_id(args.common.run_id.as_deref(), "client-connections");
    let running = Arc::new(AtomicBool::new(true));
    let successful = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let counter = Arc::new(AtomicU64::new(0));
    let connect_times = Arc::new(Mutex::new(Vec::<u64>::new()));
    let mut handles = Vec::with_capacity(args.concurrency);

    for _ in 0..args.concurrency {
        let running = Arc::clone(&running);
        let successful = Arc::clone(&successful);
        let failed = Arc::clone(&failed);
        let counter = Arc::clone(&counter);
        let endpoint = endpoint.clone();
        let ca_pem = ca_pem.clone();
        let run_id = run_id.clone();
        let connect_times = Arc::clone(&connect_times);
        let protocol = args.common.protocol;
        handles.push(tokio::spawn(async move {
            while running.load(Ordering::Relaxed) {
                let id = counter.fetch_add(1, Ordering::Relaxed);
                let client_id = format!("{run_id}-{id}");
                let started = Instant::now();
                let connected = match protocol {
                    Protocol::V4 => connect_once_v4(&endpoint, ca_pem.as_deref(), client_id).await,
                    Protocol::V5 => connect_once_v5(&endpoint, ca_pem.as_deref(), client_id).await,
                };
                match connected {
                    Ok(()) => {
                        successful.fetch_add(1, Ordering::Relaxed);
                        connect_times
                            .lock()
                            .expect("connect times lock")
                            .push(started.elapsed().as_micros() as u64);
                    }
                    Err(_) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    let measure_start = Instant::now();
    let mut last_successful = 0;
    let mut next_sample = measure_start + Duration::from_secs(1);
    let mut samples = Vec::new();
    while measure_start.elapsed() < Duration::from_secs(args.duration_sec) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if Instant::now() >= next_sample {
            let current = successful.load(Ordering::Relaxed);
            samples.push(current.saturating_sub(last_successful) as f64);
            last_successful = current;
            next_sample += Duration::from_secs(1);
        }
    }

    running.store(false, Ordering::SeqCst);
    abort_all(handles).await;

    let elapsed_sec = measure_start.elapsed().as_secs_f64();
    let successful_count = successful.load(Ordering::Relaxed);
    let failed_count = failed.load(Ordering::Relaxed);
    let mut times = connect_times.lock().expect("connect times lock").clone();
    times.sort_unstable();

    let mut metrics = BTreeMap::new();
    metrics.insert("successful".to_owned(), successful_count as f64);
    metrics.insert("failed".to_owned(), failed_count as f64);
    metrics.insert("elapsed_sec".to_owned(), elapsed_sec);
    metrics.insert(
        "connections_sec".to_owned(),
        successful_count as f64 / elapsed_sec,
    );
    insert_named_latency_metrics(&mut metrics, "connect", &times);

    let mut samples_out = BTreeMap::new();
    samples_out.insert("connections_per_sec".to_owned(), samples);

    print_output(BenchOutput {
        schema_version: 1,
        run_id,
        scenario: format!("client-connections-{}", args.common.protocol.as_str()),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "protocol": args.common.protocol,
            "transport": endpoint.transport.as_str(),
            "broker_url": args.broker_url,
            "duration_sec": args.duration_sec,
            "concurrency": args.concurrency,
        }),
        metrics,
        samples: samples_out,
        environment: environment(),
    })
}

fn run_codec(args: CodecArgs, mode: CodecMode) -> anyhow::Result<()> {
    if matches!(args.protocol, CodecProtocol::Nats) && args.qos != 0 {
        bail!("NATS codec workloads require --qos 0");
    }
    if args.profile_frequency <= 0 {
        bail!("--profile-frequency must be greater than zero");
    }
    let started_at = unix_secs();
    let run_id = run_id(args.run_id.as_deref(), "codec");
    let profiler = start_profiler(args.profile_output.as_ref(), args.profile_frequency)?;
    let result = match (args.protocol, mode) {
        (CodecProtocol::V4, CodecMode::Encode) => codec_v4_encode(&args)?,
        (CodecProtocol::V4, CodecMode::Decode) => codec_v4_decode(&args)?,
        (CodecProtocol::V4, CodecMode::Roundtrip) => codec_v4_roundtrip(&args)?,
        (CodecProtocol::V5, CodecMode::Encode) => codec_v5_encode(&args)?,
        (CodecProtocol::V5, CodecMode::Decode) => codec_v5_decode(&args)?,
        (CodecProtocol::V5, CodecMode::Roundtrip) => codec_v5_roundtrip(&args)?,
        (CodecProtocol::Nats, CodecMode::Encode) => codec_nats_encode(&args)?,
        (CodecProtocol::Nats, CodecMode::Decode) => codec_nats_decode(&args)?,
        (CodecProtocol::Nats, CodecMode::Roundtrip) => codec_nats_roundtrip(&args)?,
    };
    finish_profiler(profiler, args.profile_output.as_ref())?;

    let mut metrics = BTreeMap::new();
    metrics.insert("messages".to_owned(), args.messages as f64);
    metrics.insert("payload_size".to_owned(), args.payload_size as f64);
    metrics.insert("elapsed_sec".to_owned(), result.elapsed_sec);
    metrics.insert("bytes".to_owned(), result.bytes as f64);
    metrics.insert(
        "messages_sec".to_owned(),
        args.messages as f64 / result.elapsed_sec,
    );
    metrics.insert(
        "bytes_sec".to_owned(),
        result.bytes as f64 / result.elapsed_sec,
    );

    print_output(BenchOutput {
        schema_version: 1,
        run_id,
        scenario: format!("codec-{}-{}", args.protocol.as_str(), mode.as_str()),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "protocol": args.protocol,
            "mode": mode.as_str(),
            "messages": args.messages,
            "payload_size": args.payload_size,
            "topic": args.topic,
            "qos": args.qos,
            "profile_output": args.profile_output,
            "profile_frequency": args.profile_output.as_ref().map(|_| args.profile_frequency),
        }),
        metrics,
        samples: BTreeMap::new(),
        environment: environment(),
    })
}

#[cfg(all(feature = "profiling", unix))]
fn start_profiler(
    output: Option<&PathBuf>,
    frequency: i32,
) -> anyhow::Result<Option<pprof::ProfilerGuard<'static>>> {
    output
        .map(|_| {
            pprof::ProfilerGuardBuilder::default()
                .frequency(frequency)
                .build()
        })
        .transpose()
        .context("failed to start pprof profiler")
}

#[cfg(not(all(feature = "profiling", unix)))]
fn start_profiler(output: Option<&PathBuf>, _frequency: i32) -> anyhow::Result<Option<()>> {
    if output.is_some() {
        #[cfg(not(feature = "profiling"))]
        bail!("profiling requires building benchmarks with --features profiling");
        #[cfg(all(feature = "profiling", not(unix)))]
        bail!("pprof profiling is only supported on POSIX targets");
    }
    Ok(None)
}

#[cfg(all(feature = "profiling", unix))]
fn finish_profiler(
    profiler: Option<pprof::ProfilerGuard<'static>>,
    output: Option<&PathBuf>,
) -> anyhow::Result<()> {
    use pprof::protos::Message;
    use std::io::Write;

    let (Some(profiler), Some(output)) = (profiler, output) else {
        return Ok(());
    };
    let report = profiler
        .report()
        .build()
        .context("failed to build pprof report")?;
    let profile = report.pprof().context("failed to encode pprof report")?;
    let mut encoded = Vec::new();
    profile
        .encode(&mut encoded)
        .context("failed to serialize pprof report")?;
    let mut file = std::fs::File::create(output)
        .with_context(|| format!("failed to create pprof output {}", output.display()))?;
    file.write_all(&encoded)
        .with_context(|| format!("failed to write pprof output {}", output.display()))
}

#[cfg(not(all(feature = "profiling", unix)))]
fn finish_profiler(_profiler: Option<()>, _output: Option<&PathBuf>) -> anyhow::Result<()> {
    Ok(())
}

fn run_options_parse_url(args: OptionsParseUrlArgs) -> anyhow::Result<()> {
    #[cfg(not(feature = "url"))]
    {
        let _ = args;
        bail!("options parse-url requires building benchmarks with --features url");
    }

    #[cfg(feature = "url")]
    {
        let started_at = unix_secs();
        let run_id = run_id(args.common.run_id.as_deref(), "options-parse-url");
        let started = Instant::now();
        match args.common.protocol {
            Protocol::V4 => {
                for _ in 0..args.parses {
                    let options = rumqttc_v4::MqttOptions::parse_url(args.url.clone())?;
                    std::hint::black_box(options);
                }
            }
            Protocol::V5 => {
                for _ in 0..args.parses {
                    let options = rumqttc_v5::MqttOptions::parse_url(args.url.clone())?;
                    std::hint::black_box(options);
                }
            }
        }
        let elapsed_sec = started.elapsed().as_secs_f64();
        let mut metrics = BTreeMap::new();
        metrics.insert("parses".to_owned(), args.parses as f64);
        metrics.insert("elapsed_sec".to_owned(), elapsed_sec);
        metrics.insert("parses_sec".to_owned(), args.parses as f64 / elapsed_sec);

        print_output(BenchOutput {
            schema_version: 1,
            run_id,
            scenario: format!("options-parse-url-{}", args.common.protocol.as_str()),
            started_at_unix: started_at,
            finished_at_unix: unix_secs(),
            config: json!({
                "protocol": args.common.protocol,
                "parses": args.parses,
                "url": args.url,
            }),
            metrics,
            samples: BTreeMap::new(),
            environment: environment(),
        })
    }
}

struct CodecResult {
    elapsed_sec: f64,
    bytes: usize,
}

fn run_codec_validation_cost(args: CodecValidationArgs) -> anyhow::Result<()> {
    if args.rounds == 0 || args.messages == 0 {
        bail!("--rounds and --messages must be greater than zero");
    }

    let started_at = unix_secs();
    let run_id = run_id(args.run_id.as_deref(), "codec-validation-cost");
    let (checked_samples, prevalidated_samples, packet_size) = match args.protocol {
        Protocol::V4 => benchmark_v4_validation_cost(&args)?,
        Protocol::V5 => benchmark_v5_validation_cost(&args)?,
    };
    let checked_median = median(&checked_samples);
    let prevalidated_median = median(&prevalidated_samples);
    let validation_share = (checked_median - prevalidated_median) / checked_median;

    let mut metrics = BTreeMap::new();
    metrics.insert("messages".to_owned(), args.messages as f64);
    metrics.insert("rounds".to_owned(), args.rounds as f64);
    metrics.insert("packet_size".to_owned(), packet_size as f64);
    metrics.insert("checked_median_sec".to_owned(), checked_median);
    metrics.insert("prevalidated_median_sec".to_owned(), prevalidated_median);
    metrics.insert(
        "checked_messages_sec".to_owned(),
        args.messages as f64 / checked_median,
    );
    metrics.insert(
        "prevalidated_messages_sec".to_owned(),
        args.messages as f64 / prevalidated_median,
    );
    metrics.insert(
        "validation_share_percent".to_owned(),
        validation_share * 100.0,
    );
    metrics.insert(
        "prevalidated_speedup_percent".to_owned(),
        (checked_median / prevalidated_median - 1.0) * 100.0,
    );

    let mut samples = BTreeMap::new();
    samples.insert("checked_elapsed_sec".to_owned(), checked_samples);
    samples.insert("prevalidated_elapsed_sec".to_owned(), prevalidated_samples);

    print_output(BenchOutput {
        schema_version: 1,
        run_id,
        scenario: format!("codec-{}-validation-cost", args.protocol.as_str()),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "protocol": args.protocol,
            "rounds": args.rounds,
            "messages": args.messages,
            "payload_size": args.payload_size,
            "topic": args.topic,
            "topic_len": args.topic.len(),
            "qos": args.qos,
        }),
        metrics,
        samples,
        environment: environment(),
    })
}

fn benchmark_v4_validation_cost(
    args: &CodecValidationArgs,
) -> anyhow::Result<(Vec<f64>, Vec<f64>, usize)> {
    let mut publish = rumqttc_v4::mqttbytes::v4::Publish::new(
        args.topic.clone(),
        v4_qos(args.qos),
        vec![0_u8; args.payload_size],
    );
    if args.qos != 0 {
        publish.pkid = 1;
    }
    let packet_size = publish.size();
    let capacity = packet_size
        .checked_mul(args.messages)
        .context("benchmark output buffer capacity overflowed")?;
    let mut buffer = BytesMut::with_capacity(capacity);
    publish.write(&mut buffer)?;
    buffer.clear();
    rumqttc_v4::bench_instrumentation::write_prevalidated_publish(&publish, &mut buffer)?;
    buffer.clear();

    let mut checked = Vec::with_capacity(args.rounds);
    let mut prevalidated = Vec::with_capacity(args.rounds);
    for round in 0..args.rounds {
        if round % 2 == 0 {
            checked.push(time_v4_publish_writes(
                &publish,
                &mut buffer,
                args.messages,
                true,
            )?);
            prevalidated.push(time_v4_publish_writes(
                &publish,
                &mut buffer,
                args.messages,
                false,
            )?);
        } else {
            prevalidated.push(time_v4_publish_writes(
                &publish,
                &mut buffer,
                args.messages,
                false,
            )?);
            checked.push(time_v4_publish_writes(
                &publish,
                &mut buffer,
                args.messages,
                true,
            )?);
        }
    }
    Ok((checked, prevalidated, packet_size))
}

fn time_v4_publish_writes(
    publish: &rumqttc_v4::mqttbytes::v4::Publish,
    buffer: &mut BytesMut,
    messages: usize,
    checked: bool,
) -> anyhow::Result<f64> {
    buffer.clear();
    let started = Instant::now();
    for _ in 0..messages {
        if checked {
            publish.write(buffer)?;
        } else {
            rumqttc_v4::bench_instrumentation::write_prevalidated_publish(publish, buffer)?;
        }
    }
    std::hint::black_box(&buffer);
    Ok(started.elapsed().as_secs_f64())
}

fn benchmark_v5_validation_cost(
    args: &CodecValidationArgs,
) -> anyhow::Result<(Vec<f64>, Vec<f64>, usize)> {
    let mut publish = rumqttc_v5::mqttbytes::v5::Publish::new(
        args.topic.clone(),
        v5_qos(args.qos),
        Bytes::from(vec![0_u8; args.payload_size]),
        None,
    );
    if args.qos != 0 {
        publish.pkid = 1;
    }
    let packet_size = publish.size();
    let capacity = packet_size
        .checked_mul(args.messages)
        .context("benchmark output buffer capacity overflowed")?;
    let mut buffer = BytesMut::with_capacity(capacity);
    publish.write(&mut buffer)?;
    buffer.clear();
    rumqttc_v5::bench_instrumentation::write_prevalidated_publish(&publish, &mut buffer)?;
    buffer.clear();

    let mut checked = Vec::with_capacity(args.rounds);
    let mut prevalidated = Vec::with_capacity(args.rounds);
    for round in 0..args.rounds {
        if round % 2 == 0 {
            checked.push(time_v5_publish_writes(
                &publish,
                &mut buffer,
                args.messages,
                true,
            )?);
            prevalidated.push(time_v5_publish_writes(
                &publish,
                &mut buffer,
                args.messages,
                false,
            )?);
        } else {
            prevalidated.push(time_v5_publish_writes(
                &publish,
                &mut buffer,
                args.messages,
                false,
            )?);
            checked.push(time_v5_publish_writes(
                &publish,
                &mut buffer,
                args.messages,
                true,
            )?);
        }
    }
    Ok((checked, prevalidated, packet_size))
}

fn time_v5_publish_writes(
    publish: &rumqttc_v5::mqttbytes::v5::Publish,
    buffer: &mut BytesMut,
    messages: usize,
    checked: bool,
) -> anyhow::Result<f64> {
    buffer.clear();
    let started = Instant::now();
    for _ in 0..messages {
        if checked {
            publish.write(buffer)?;
        } else {
            rumqttc_v5::bench_instrumentation::write_prevalidated_publish(publish, buffer)?;
        }
    }
    std::hint::black_box(&buffer);
    Ok(started.elapsed().as_secs_f64())
}

fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn codec_v4_encode(args: &CodecArgs) -> anyhow::Result<CodecResult> {
    let packet = v4_publish_packet(args);
    let mut buffer = BytesMut::with_capacity(packet.size() * args.messages);
    let started = Instant::now();
    for _ in 0..args.messages {
        packet.write(&mut buffer, usize::MAX)?;
    }
    std::hint::black_box(&buffer);
    Ok(CodecResult {
        elapsed_sec: started.elapsed().as_secs_f64(),
        bytes: buffer.len(),
    })
}

fn codec_v4_decode(args: &CodecArgs) -> anyhow::Result<CodecResult> {
    let mut stream = v4_stream(args)?;
    let bytes = stream.len();
    let started = Instant::now();
    for _ in 0..args.messages {
        let packet = rumqttc_v4::mqttbytes::v4::Packet::read(&mut stream, usize::MAX)?;
        std::hint::black_box(packet);
    }
    Ok(CodecResult {
        elapsed_sec: started.elapsed().as_secs_f64(),
        bytes,
    })
}

fn codec_v4_roundtrip(args: &CodecArgs) -> anyhow::Result<CodecResult> {
    let packet = v4_publish_packet(args);
    let mut bytes = 0;
    let started = Instant::now();
    for _ in 0..args.messages {
        let mut stream = BytesMut::with_capacity(packet.size());
        packet.write(&mut stream, usize::MAX)?;
        bytes += stream.len();
        let packet = rumqttc_v4::mqttbytes::v4::Packet::read(&mut stream, usize::MAX)?;
        std::hint::black_box(packet);
    }
    Ok(CodecResult {
        elapsed_sec: started.elapsed().as_secs_f64(),
        bytes,
    })
}

fn codec_v5_encode(args: &CodecArgs) -> anyhow::Result<CodecResult> {
    let packet = v5_publish_packet(args);
    let mut buffer = BytesMut::with_capacity(packet.size() * args.messages);
    let started = Instant::now();
    for _ in 0..args.messages {
        packet.write(&mut buffer, None)?;
    }
    std::hint::black_box(&buffer);
    Ok(CodecResult {
        elapsed_sec: started.elapsed().as_secs_f64(),
        bytes: buffer.len(),
    })
}

fn codec_v5_decode(args: &CodecArgs) -> anyhow::Result<CodecResult> {
    let mut stream = v5_stream(args)?;
    let bytes = stream.len();
    let started = Instant::now();
    for _ in 0..args.messages {
        let packet = rumqttc_v5::mqttbytes::v5::Packet::read(&mut stream, None)?;
        std::hint::black_box(packet);
    }
    Ok(CodecResult {
        elapsed_sec: started.elapsed().as_secs_f64(),
        bytes,
    })
}

fn codec_v5_roundtrip(args: &CodecArgs) -> anyhow::Result<CodecResult> {
    let packet = v5_publish_packet(args);
    let mut bytes = 0;
    let started = Instant::now();
    for _ in 0..args.messages {
        let mut stream = BytesMut::with_capacity(packet.size());
        packet.write(&mut stream, None)?;
        bytes += stream.len();
        let packet = rumqttc_v5::mqttbytes::v5::Packet::read(&mut stream, None)?;
        std::hint::black_box(packet);
    }
    Ok(CodecResult {
        elapsed_sec: started.elapsed().as_secs_f64(),
        bytes,
    })
}

fn codec_nats_encode(args: &CodecArgs) -> anyhow::Result<CodecResult> {
    let payload = vec![0_u8; args.payload_size];
    let mut buffer = BytesMut::new();
    let started = Instant::now();
    for _ in 0..args.messages {
        nats_codec::write_publish(&args.topic, &payload, &mut buffer)?;
    }
    std::hint::black_box(&buffer);
    Ok(CodecResult {
        elapsed_sec: started.elapsed().as_secs_f64(),
        bytes: buffer.len(),
    })
}

fn codec_nats_decode(args: &CodecArgs) -> anyhow::Result<CodecResult> {
    let mut stream = nats_stream(args)?;
    let bytes = stream.len();
    let started = Instant::now();
    for _ in 0..args.messages {
        std::hint::black_box(nats_codec::read_publish(&mut stream)?);
    }
    Ok(CodecResult {
        elapsed_sec: started.elapsed().as_secs_f64(),
        bytes,
    })
}

fn codec_nats_roundtrip(args: &CodecArgs) -> anyhow::Result<CodecResult> {
    let payload = vec![0_u8; args.payload_size];
    let mut bytes = 0;
    let started = Instant::now();
    for _ in 0..args.messages {
        let mut stream = BytesMut::new();
        nats_codec::write_publish(&args.topic, &payload, &mut stream)?;
        bytes += stream.len();
        std::hint::black_box(nats_codec::read_publish(&mut stream)?);
    }
    Ok(CodecResult {
        elapsed_sec: started.elapsed().as_secs_f64(),
        bytes,
    })
}

fn nats_stream(args: &CodecArgs) -> anyhow::Result<BytesMut> {
    let payload = vec![0_u8; args.payload_size];
    let mut stream = BytesMut::new();
    for _ in 0..args.messages {
        nats_codec::write_publish(&args.topic, &payload, &mut stream)?;
    }
    Ok(stream)
}

fn v4_stream(args: &CodecArgs) -> anyhow::Result<BytesMut> {
    let packet = v4_publish_packet(args);
    let mut stream = BytesMut::with_capacity(packet.size() * args.messages);
    for _ in 0..args.messages {
        packet.write(&mut stream, usize::MAX)?;
    }
    Ok(stream)
}

fn v5_stream(args: &CodecArgs) -> anyhow::Result<BytesMut> {
    let packet = v5_publish_packet(args);
    let mut stream = BytesMut::with_capacity(packet.size() * args.messages);
    for _ in 0..args.messages {
        packet.write(&mut stream, None)?;
    }
    Ok(stream)
}

fn v4_publish_packet(args: &CodecArgs) -> rumqttc_v4::mqttbytes::v4::Packet {
    let mut publish = rumqttc_v4::mqttbytes::v4::Publish::new(
        args.topic.clone(),
        v4_qos(args.qos),
        vec![0_u8; args.payload_size],
    );
    if args.qos != 0 {
        publish.pkid = 1;
    }
    rumqttc_v4::mqttbytes::v4::Packet::Publish(publish)
}

fn v5_publish_packet(args: &CodecArgs) -> rumqttc_v5::mqttbytes::v5::Packet {
    let mut publish = rumqttc_v5::mqttbytes::v5::Publish::new(
        args.topic.clone(),
        v5_qos(args.qos),
        Bytes::from(vec![0_u8; args.payload_size]),
        None,
    );
    if args.qos != 0 {
        publish.pkid = 1;
    }
    rumqttc_v5::mqttbytes::v5::Packet::Publish(publish)
}

fn run_v4_publishers(
    handles: &mut Vec<tokio::task::JoinHandle<()>>,
    publishers: usize,
    run_id: &str,
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
    topic: &str,
    payload_size: usize,
    qos: u8,
    published: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
) {
    for i in 0..publishers {
        let (client, mut eventloop) =
            new_v4_client(format!("{run_id}-pub-{i}"), endpoint, ca_pem, 100);
        let poll_running = Arc::clone(&running);
        handles.push(tokio::spawn(async move {
            while poll_running.load(Ordering::Relaxed) {
                if eventloop.poll().await.is_err() {
                    break;
                }
            }
        }));

        let publish_running = Arc::clone(&running);
        let published = Arc::clone(&published);
        let topic = topic.to_owned();
        let payload = vec![0_u8; payload_size];
        let qos = v4_qos(qos);
        handles.push(tokio::spawn(async move {
            while publish_running.load(Ordering::Relaxed) {
                if client
                    .publish(
                        topic.clone(),
                        payload.clone(),
                        rumqttc_v4::PublishOptions::new(qos),
                    )
                    .await
                    .is_ok()
                {
                    published.fetch_add(1, Ordering::Relaxed);
                }
            }
            drop(client.disconnect().await);
        }));
    }
}

async fn run_v4_subscribers(
    handles: &mut Vec<tokio::task::JoinHandle<()>>,
    subscribers: usize,
    run_id: &str,
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
    filter: &str,
    qos: u8,
    received: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    for i in 0..subscribers {
        let (client, mut eventloop) =
            new_v4_client(format!("{run_id}-sub-{i}"), endpoint, ca_pem, 100);
        client.subscribe(filter.to_owned(), v4_qos(qos)).await?;
        let received = Arc::clone(&received);
        let running = Arc::clone(&running);
        handles.push(tokio::spawn(async move {
            while running.load(Ordering::Relaxed) {
                match eventloop.poll().await {
                    Ok(rumqttc_v4::Event::Incoming(rumqttc_v4::Incoming::Publish(_))) => {
                        received.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            drop(client.disconnect().await);
        }));
    }
    Ok(())
}

fn run_v5_publishers(
    handles: &mut Vec<tokio::task::JoinHandle<()>>,
    publishers: usize,
    run_id: &str,
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
    topic: &str,
    payload_size: usize,
    qos: u8,
    published: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
) {
    for i in 0..publishers {
        let (client, mut eventloop) =
            new_v5_client(format!("{run_id}-pub-{i}"), endpoint, ca_pem, 100);
        let poll_running = Arc::clone(&running);
        handles.push(tokio::spawn(async move {
            while poll_running.load(Ordering::Relaxed) {
                if eventloop.poll().await.is_err() {
                    break;
                }
            }
        }));

        let publish_running = Arc::clone(&running);
        let published = Arc::clone(&published);
        let topic = topic.to_owned();
        let payload = vec![0_u8; payload_size];
        let qos = v5_qos(qos);
        handles.push(tokio::spawn(async move {
            while publish_running.load(Ordering::Relaxed) {
                if client
                    .publish(
                        topic.clone(),
                        payload.clone(),
                        rumqttc_v5::PublishOptions::new(qos),
                    )
                    .await
                    .is_ok()
                {
                    published.fetch_add(1, Ordering::Relaxed);
                }
            }
            drop(client.disconnect().await);
        }));
    }
}

async fn run_v5_subscribers(
    handles: &mut Vec<tokio::task::JoinHandle<()>>,
    subscribers: usize,
    run_id: &str,
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
    filter: &str,
    qos: u8,
    received: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    for i in 0..subscribers {
        let (client, mut eventloop) =
            new_v5_client(format!("{run_id}-sub-{i}"), endpoint, ca_pem, 100);
        client.subscribe(filter.to_owned(), v5_qos(qos)).await?;
        let received = Arc::clone(&received);
        let running = Arc::clone(&running);
        handles.push(tokio::spawn(async move {
            while running.load(Ordering::Relaxed) {
                match eventloop.poll().await {
                    Ok(rumqttc_v5::Event::Incoming(rumqttc_v5::Incoming::Publish(_))) => {
                        received.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            drop(client.disconnect().await);
        }));
    }
    Ok(())
}

async fn publish_latency_samples_v4(
    client: &rumqttc_v4::AsyncClient,
    args: &ClientLatencyArgs,
    seconds: u64,
) -> anyhow::Result<()> {
    publish_latency_samples(
        args.rate,
        seconds,
        args.client.payload_size,
        |payload| async {
            client
                .publish(
                    args.client.topic.clone(),
                    payload,
                    rumqttc_v4::PublishOptions::new(v4_qos(args.client.qos)),
                )
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

async fn publish_latency_samples_v5(
    client: &rumqttc_v5::AsyncClient,
    args: &ClientLatencyArgs,
    seconds: u64,
) -> anyhow::Result<()> {
    publish_latency_samples(
        args.rate,
        seconds,
        args.client.payload_size,
        |payload| async {
            client
                .publish(
                    args.client.topic.clone(),
                    payload,
                    rumqttc_v5::PublishOptions::new(v5_qos(args.client.qos)),
                )
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

async fn publish_latency_samples<F, Fut>(
    rate: u64,
    seconds: u64,
    payload_size: usize,
    mut publish: F,
) -> anyhow::Result<()>
where
    F: FnMut(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let interval = Duration::from_micros((1_000_000 / rate).max(1));
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut payload = vec![0_u8; payload_size.max(8)];
    while Instant::now() < end {
        let sent_nanos = unix_nanos();
        payload[0..8].copy_from_slice(&sent_nanos.to_be_bytes());
        publish(payload.clone()).await?;
        tokio::time::sleep(interval).await;
    }
    Ok(())
}

fn record_latency_sample(payload: &[u8], latencies: &Arc<Mutex<Vec<u64>>>) {
    if payload.len() < 8 {
        return;
    }
    let mut sent = [0_u8; 8];
    sent.copy_from_slice(&payload[..8]);
    let sent_nanos = u64::from_be_bytes(sent);
    latencies
        .lock()
        .expect("latencies lock")
        .push(unix_nanos().saturating_sub(sent_nanos) / 1000);
}

async fn connect_once_v4(
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
    client_id: String,
) -> anyhow::Result<()> {
    let (client, mut eventloop) = new_v4_client(client_id, endpoint, ca_pem, 10);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match eventloop.poll().await {
                Ok(rumqttc_v4::Event::Incoming(rumqttc_v4::Incoming::ConnAck(_))) => break,
                Ok(_) => {}
                Err(error) => return Err(anyhow::Error::from(error)),
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    drop(client.disconnect().await);
    Ok(())
}

async fn connect_once_v5(
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
    client_id: String,
) -> anyhow::Result<()> {
    let (client, mut eventloop) = new_v5_client(client_id, endpoint, ca_pem, 10);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match eventloop.poll().await {
                Ok(rumqttc_v5::Event::Incoming(rumqttc_v5::Incoming::ConnAck(_))) => break,
                Ok(_) => {}
                Err(error) => return Err(anyhow::Error::from(error)),
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    drop(client.disconnect().await);
    Ok(())
}

fn new_v4_client(
    client_id: String,
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
    capacity: usize,
) -> (rumqttc_v4::AsyncClient, rumqttc_v4::EventLoop) {
    let mut options = v4_options(client_id, endpoint);
    options.set_keep_alive(30);
    options.set_max_packet_size(BENCH_MAX_PACKET_SIZE, BENCH_MAX_PACKET_SIZE);
    if endpoint.transport == TransportKind::Tls {
        if let Some(ca_pem) = ca_pem {
            options.set_transport(rumqttc_v4::Transport::tls(ca_pem.to_vec(), None, None));
        } else {
            options.set_transport(rumqttc_v4::Transport::tls_with_default_config());
        }
    }
    rumqttc_v4::AsyncClient::builder(options)
        .capacity(capacity)
        .build()
}

fn v4_options(client_id: String, endpoint: &BrokerEndpoint) -> rumqttc_v4::MqttOptions {
    match endpoint.transport {
        #[cfg(feature = "websocket")]
        TransportKind::Websocket => rumqttc_v4::MqttOptions::new(
            client_id,
            rumqttc_v4::Broker::websocket(endpoint.url.clone()).expect("validated websocket URL"),
        ),
        TransportKind::Tcp | TransportKind::Tls => {
            rumqttc_v4::MqttOptions::new(client_id, (endpoint.host.clone(), endpoint.port))
        }
        #[cfg(not(feature = "websocket"))]
        TransportKind::Websocket => unreachable!("websocket endpoint requires websocket feature"),
    }
}

fn new_v5_client(
    client_id: String,
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
    capacity: usize,
) -> (rumqttc_v5::AsyncClient, rumqttc_v5::EventLoop) {
    let mut options = v5_options(client_id, endpoint);
    options.set_keep_alive(30);
    options.set_max_packet_size(Some(BENCH_MAX_PACKET_SIZE as u32));
    if endpoint.transport == TransportKind::Tls {
        if let Some(ca_pem) = ca_pem {
            options.set_transport(rumqttc_v5::Transport::tls(ca_pem.to_vec(), None, None));
        } else {
            options.set_transport(rumqttc_v5::Transport::tls_with_default_config());
        }
    }
    rumqttc_v5::AsyncClient::builder(options)
        .capacity(capacity)
        .build()
}

fn v5_options(client_id: String, endpoint: &BrokerEndpoint) -> rumqttc_v5::MqttOptions {
    match endpoint.transport {
        #[cfg(feature = "websocket")]
        TransportKind::Websocket => rumqttc_v5::MqttOptions::new(
            client_id,
            rumqttc_v5::Broker::websocket(endpoint.url.clone()).expect("validated websocket URL"),
        ),
        TransportKind::Tcp | TransportKind::Tls => {
            rumqttc_v5::MqttOptions::new(client_id, (endpoint.host.clone(), endpoint.port))
        }
        #[cfg(not(feature = "websocket"))]
        TransportKind::Websocket => unreachable!("websocket endpoint requires websocket feature"),
    }
}

async fn abort_all(handles: Vec<tokio::task::JoinHandle<()>>) {
    for handle in &handles {
        handle.abort();
    }
    for handle in handles {
        drop(handle.await);
    }
}

fn insert_throughput_stability_metrics(metrics: &mut BTreeMap<String, f64>, samples: &[f64]) {
    if samples.is_empty() {
        metrics.insert("throughput_min_msg_sec".to_owned(), 0.0);
        metrics.insert("throughput_first_half_median_msg_sec".to_owned(), 0.0);
        metrics.insert("throughput_second_half_median_msg_sec".to_owned(), 0.0);
        metrics.insert("throughput_collapse_pct".to_owned(), 0.0);
        return;
    }

    let split = (samples.len() / 2).max(1);
    let (first_half, second_half) = samples.split_at(split);
    let first_median = median_f64(first_half);
    let second_median = median_f64(if second_half.is_empty() {
        first_half
    } else {
        second_half
    });
    let collapse_pct = if first_median > 0.0 {
        ((first_median - second_median) / first_median * 100.0).max(0.0)
    } else {
        0.0
    };

    metrics.insert(
        "throughput_min_msg_sec".to_owned(),
        samples.iter().copied().fold(f64::INFINITY, f64::min),
    );
    metrics.insert(
        "throughput_first_half_median_msg_sec".to_owned(),
        first_median,
    );
    metrics.insert(
        "throughput_second_half_median_msg_sec".to_owned(),
        second_median,
    );
    metrics.insert("throughput_collapse_pct".to_owned(), collapse_pct);
}

fn median_f64(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

fn resident_set_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let value_kib = rest.split_whitespace().next()?.parse::<u64>().ok()?;
                return Some(value_kib * 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn insert_latency_metrics(metrics: &mut BTreeMap<String, f64>, samples: &[u64]) {
    insert_named_latency_metrics(metrics, "", samples);
}

fn insert_named_latency_metrics(
    metrics: &mut BTreeMap<String, f64>,
    prefix: &str,
    samples: &[u64],
) {
    let name = |metric: &str| {
        if prefix.is_empty() {
            format!("{metric}_us")
        } else {
            format!("{prefix}_{metric}_us")
        }
    };

    if samples.is_empty() {
        for metric in ["min", "max", "avg", "p50", "p95", "p99"] {
            metrics.insert(name(metric), 0.0);
        }
        return;
    }

    metrics.insert(name("min"), samples[0] as f64);
    metrics.insert(name("max"), samples[samples.len() - 1] as f64);
    metrics.insert(
        name("avg"),
        samples.iter().sum::<u64>() as f64 / samples.len() as f64,
    );
    metrics.insert(name("p50"), percentile(samples, 50) as f64);
    metrics.insert(name("p95"), percentile(samples, 95) as f64);
    metrics.insert(name("p99"), percentile(samples, 99) as f64);
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() - 1) * percentile) / 100;
    sorted[index]
}

fn downsample(samples: &[u64], max: usize) -> Vec<u64> {
    if samples.len() <= max {
        return samples.to_vec();
    }
    let step = (samples.len() / max).max(1);
    samples.iter().step_by(step).copied().take(max).collect()
}

fn parse_endpoint(url: &str) -> anyhow::Result<BrokerEndpoint> {
    let (transport, rest) = if let Some(rest) = url.strip_prefix("mqtt://") {
        (TransportKind::Tcp, rest)
    } else if let Some(rest) = url.strip_prefix("mqtts://") {
        (TransportKind::Tls, rest)
    } else if let Some(rest) = url.strip_prefix("ssl://") {
        (TransportKind::Tls, rest)
    } else if let Some(rest) = url.strip_prefix("ws://") {
        #[cfg(not(feature = "websocket"))]
        {
            let _ = rest;
            bail!("ws:// broker URLs require building benchmarks with --features websocket");
        }
        #[cfg(feature = "websocket")]
        {
            (TransportKind::Websocket, rest)
        }
    } else {
        bail!("unsupported broker URL scheme: {url}");
    };

    let host_port = rest.split('/').next().unwrap_or(rest);
    let (host, port) = host_port
        .rsplit_once(':')
        .with_context(|| format!("broker URL must include host and port: {url}"))?;
    let port = port
        .parse::<u16>()
        .with_context(|| format!("invalid broker port in URL: {url}"))?;

    Ok(BrokerEndpoint {
        url: url.to_owned(),
        host: host.to_owned(),
        port,
        transport,
    })
}

fn read_ca_cert(path: Option<&PathBuf>) -> anyhow::Result<Option<Vec<u8>>> {
    path.map(std::fs::read)
        .transpose()
        .context("failed to read CA certificate")
}

fn parse_qos(value: &str) -> Result<u8, String> {
    match value {
        "0" => Ok(0),
        "1" => Ok(1),
        "2" => Ok(2),
        _ => Err(format!("QoS must be 0, 1, or 2, got {value}")),
    }
}

const fn v4_qos(qos: u8) -> rumqttc_v4::mqttbytes::QoS {
    match qos {
        0 => rumqttc_v4::mqttbytes::QoS::AtMostOnce,
        1 => rumqttc_v4::mqttbytes::QoS::AtLeastOnce,
        _ => rumqttc_v4::mqttbytes::QoS::ExactlyOnce,
    }
}

const fn v5_qos(qos: u8) -> rumqttc_v5::mqttbytes::QoS {
    match qos {
        0 => rumqttc_v5::mqttbytes::QoS::AtMostOnce,
        1 => rumqttc_v5::mqttbytes::QoS::AtLeastOnce,
        _ => rumqttc_v5::mqttbytes::QoS::ExactlyOnce,
    }
}

fn run_id(input: Option<&str>, prefix: &str) -> String {
    input.map_or_else(
        || format!("{prefix}-{}-{}", unix_secs(), rand::random::<u32>()),
        ToOwned::to_owned,
    )
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn unix_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64)
}

fn environment() -> Environment {
    Environment {
        git_commit: command_stdout("git", &["rev-parse", "HEAD"]),
        rustc: command_stdout("rustc", &["--version"]),
        target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        cpu_count: std::thread::available_parallelism().map_or(1, usize::from),
    }
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn print_output(output: BenchOutput) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
