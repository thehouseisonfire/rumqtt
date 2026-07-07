#![expect(clippy::cast_precision_loss)]
#![expect(clippy::too_many_lines)]

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
}

#[derive(Subcommand, Debug)]
enum ClientCommand {
    Throughput(ClientThroughputArgs),
    Latency(ClientLatencyArgs),
    Connections(ClientConnectionArgs),
}

#[derive(Subcommand, Debug)]
enum CodecCommand {
    Encode(CodecArgs),
    Decode(CodecArgs),
    Roundtrip(CodecArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Protocol {
    V4,
    V5,
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
    #[command(flatten)]
    common: CommonArgs,

    #[arg(long, default_value = "100000")]
    messages: usize,

    #[arg(long, default_value = "64")]
    payload_size: usize,

    #[arg(long, default_value = "1", value_parser = parse_qos)]
    qos: u8,

    #[arg(long, default_value = "bench/codec")]
    topic: String,
}

#[derive(Debug, Clone)]
struct BrokerEndpoint {
    host: String,
    port: u16,
    transport: TransportKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum TransportKind {
    Tcp,
    Tls,
}

impl TransportKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Tls => "tls",
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
        },
        CommandGroup::Codec { command } => match command {
            CodecCommand::Encode(args) => run_codec(args, CodecMode::Encode),
            CodecCommand::Decode(args) => run_codec(args, CodecMode::Decode),
            CodecCommand::Roundtrip(args) => run_codec(args, CodecMode::Roundtrip),
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
    let started_at = unix_secs();
    let run_id = run_id(args.common.run_id.as_deref(), "codec");
    let result = match (args.common.protocol, mode) {
        (Protocol::V4, CodecMode::Encode) => codec_v4_encode(&args)?,
        (Protocol::V4, CodecMode::Decode) => codec_v4_decode(&args)?,
        (Protocol::V4, CodecMode::Roundtrip) => codec_v4_roundtrip(&args)?,
        (Protocol::V5, CodecMode::Encode) => codec_v5_encode(&args)?,
        (Protocol::V5, CodecMode::Decode) => codec_v5_decode(&args)?,
        (Protocol::V5, CodecMode::Roundtrip) => codec_v5_roundtrip(&args)?,
    };

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
        scenario: format!("codec-{}-{}", args.common.protocol.as_str(), mode.as_str()),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "protocol": args.common.protocol,
            "mode": mode.as_str(),
            "messages": args.messages,
            "payload_size": args.payload_size,
            "topic": args.topic,
            "qos": args.qos,
        }),
        metrics,
        samples: BTreeMap::new(),
        environment: environment(),
    })
}

struct CodecResult {
    elapsed_sec: f64,
    bytes: usize,
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
    let mut options =
        rumqttc_v4::MqttOptions::new(client_id, (endpoint.host.clone(), endpoint.port));
    options.set_keep_alive(30);
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

fn new_v5_client(
    client_id: String,
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
    capacity: usize,
) -> (rumqttc_v5::AsyncClient, rumqttc_v5::EventLoop) {
    let mut options =
        rumqttc_v5::MqttOptions::new(client_id, (endpoint.host.clone(), endpoint.port));
    options.set_keep_alive(30);
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

async fn abort_all(handles: Vec<tokio::task::JoinHandle<()>>) {
    for handle in &handles {
        handle.abort();
    }
    for handle in handles {
        drop(handle.await);
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
