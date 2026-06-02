#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::too_many_lines)]

use clap::{Parser, ValueEnum};
use rumqttc_v5::Transport;
use rumqttc_v5::mqttbytes::QoS;
use rumqttc_v5::{AsyncClient, Event, Incoming, MqttOptions};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum BenchMode {
    #[default]
    Throughput,
    Latency,
    Connections,
}

#[derive(Parser, Debug, Clone)]
#[command(name = "rumqttv5bench")]
struct BenchCommand {
    #[arg(long, value_enum, default_value = "throughput")]
    mode: BenchMode,

    #[arg(long, default_value = "10")]
    duration: u64,

    #[arg(long, default_value = "2")]
    warmup: u64,

    #[arg(long, default_value = "64")]
    payload_size: usize,

    #[arg(long, default_value = "bench/test")]
    topic: String,

    #[arg(long)]
    filter: Option<String>,

    #[arg(long, default_value = "0", value_parser = parse_qos)]
    qos: QoS,

    #[arg(long, default_value = "mqtt://localhost:1883")]
    url: String,

    #[arg(long)]
    client_id: Option<String>,

    #[arg(long)]
    run_id: Option<String>,

    #[arg(long, default_value = "1")]
    publishers: usize,

    #[arg(long, default_value = "1")]
    subscribers: usize,

    #[arg(long, default_value = "10")]
    concurrency: usize,

    #[arg(long)]
    ca_cert: Option<PathBuf>,
}

#[derive(Serialize)]
struct BenchConfig {
    duration_secs: u64,
    warmup_secs: u64,
    payload_size: usize,
    qos: u8,
    topic: String,
    filter: String,
    transport: String,
    publishers: usize,
    subscribers: usize,
}

#[derive(Serialize)]
struct ThroughputResults {
    published: u64,
    received: u64,
    elapsed_secs: f64,
    throughput_avg: f64,
    samples: Vec<u64>,
}

#[derive(Serialize)]
struct LatencyResults {
    messages: u64,
    min_us: u64,
    max_us: u64,
    avg_us: f64,
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    samples: Vec<u64>,
}

#[derive(Serialize)]
struct ConnectionResults {
    total_connections: u64,
    successful: u64,
    failed: u64,
    elapsed_secs: f64,
    connections_per_sec: f64,
    avg_connect_us: f64,
    p50_connect_us: u64,
    p95_connect_us: u64,
    p99_connect_us: u64,
    samples: Vec<u64>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum BenchResults {
    Throughput(ThroughputResults),
    Latency(LatencyResults),
    Connections(ConnectionResults),
}

#[derive(Serialize)]
struct BenchOutput {
    library: String,
    mode: String,
    meta: BenchMeta,
    config: BenchConfig,
    results: BenchResults,
}

#[derive(Serialize)]
struct BenchMeta {
    schema_version: String,
    run_id: String,
    client_id_base: String,
    started_at_utc: String,
    finished_at_utc: String,
}

fn now_utc_string() -> String {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or_else(
            |_| "0".to_owned(),
            |duration| duration.as_secs().to_string(),
        )
}

#[derive(Clone)]
struct BrokerEndpoint {
    host: String,
    port: u16,
    transport: String,
}

fn parse_qos(s: &str) -> Result<QoS, String> {
    match s {
        "0" => Ok(QoS::AtMostOnce),
        "1" => Ok(QoS::AtLeastOnce),
        "2" => Ok(QoS::ExactlyOnce),
        _ => Err(format!("QoS must be 0, 1, or 2, got: {s}")),
    }
}

fn parse_endpoint(url: &str) -> Result<BrokerEndpoint, String> {
    let (transport, rest) = if let Some(rest) = url.strip_prefix("mqtt://") {
        ("tcp".to_owned(), rest)
    } else if let Some(rest) = url.strip_prefix("mqtts://") {
        ("tls".to_owned(), rest)
    } else if let Some(rest) = url.strip_prefix("ssl://") {
        ("tls".to_owned(), rest)
    } else {
        return Err(format!(
            "unsupported URL scheme in '{url}', expected mqtt:// or mqtts://"
        ));
    };

    let host_port = rest.split('/').next().unwrap_or(rest);
    let mut parts = host_port.rsplitn(2, ':');
    let port = parts
        .next()
        .ok_or_else(|| format!("missing port in '{url}'"))?
        .parse::<u16>()
        .map_err(|_| format!("invalid port in '{url}'"))?;
    let host = parts
        .next()
        .ok_or_else(|| format!("missing host in '{url}'"))?
        .to_owned();

    Ok(BrokerEndpoint {
        host,
        port,
        transport,
    })
}

fn build_options(
    client_id: String,
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
) -> MqttOptions {
    let mut options = MqttOptions::new(client_id, (endpoint.host.clone(), endpoint.port));
    options.set_keep_alive(30);

    if endpoint.transport == "tls"
        && let Some(ca_pem) = ca_pem
    {
        options.set_transport(Transport::tls(ca_pem.to_vec(), None, None));
    } else if endpoint.transport == "tls" {
        options.set_transport(Transport::tls_with_default_config());
    }

    options
}

async fn run_throughput(
    cmd: &BenchCommand,
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
) -> anyhow::Result<()> {
    let started_at = now_utc_string();
    let base_id = cmd
        .client_id
        .clone()
        .unwrap_or_else(|| format!("rumqtt-v5-bench-{}", rand::random::<u32>()));
    let run_id = cmd
        .run_id
        .clone()
        .unwrap_or_else(|| format!("run-{}", rand::random::<u32>()));

    let filter = cmd.filter.clone().unwrap_or_else(|| cmd.topic.clone());
    let received = Arc::new(AtomicU64::new(0));
    let published = Arc::new(AtomicU64::new(0));
    let running = Arc::new(AtomicBool::new(true));

    let mut sub_handles = Vec::with_capacity(cmd.subscribers);
    let mut sub_clients = Vec::with_capacity(cmd.subscribers);
    for i in 0..cmd.subscribers {
        let client_id = format!("{base_id}-sub-{i}");
        let options = build_options(client_id.clone(), endpoint, ca_pem);
        let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
        client.subscribe(filter.clone(), cmd.qos).await?;
        let recv = Arc::clone(&received);
        let run = Arc::clone(&running);
        sub_handles.push(tokio::spawn(async move {
            while run.load(Ordering::Relaxed) {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Incoming::Publish(_))) => {
                        recv.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }));
        sub_clients.push(client);
    }

    let payload: Arc<[u8]> = vec![0_u8; cmd.payload_size].into();
    let mut pub_handles = Vec::with_capacity(cmd.publishers);
    let mut poll_handles = Vec::with_capacity(cmd.publishers);
    let mut pub_clients = Vec::with_capacity(cmd.publishers);
    for i in 0..cmd.publishers {
        let client_id = format!("{base_id}-pub-{i}");
        let options = build_options(client_id.clone(), endpoint, ca_pem);
        let (client, mut eventloop) = AsyncClient::builder(options).capacity(100).build();
        let run = Arc::clone(&running);
        poll_handles.push(tokio::spawn(async move {
            while run.load(Ordering::Relaxed) {
                if eventloop.poll().await.is_err() {
                    break;
                }
            }
        }));

        let run = Arc::clone(&running);
        let published_count = Arc::clone(&published);
        let topic = cmd.topic.clone();
        let payload = Arc::clone(&payload);
        let qos = cmd.qos;
        let publisher_client = client.clone();
        pub_handles.push(tokio::spawn(async move {
            while run.load(Ordering::Relaxed) {
                if publisher_client
                    .publish(topic.clone(), qos, false, payload.to_vec())
                    .await
                    .is_ok()
                {
                    published_count.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
        pub_clients.push(client);
    }

    tokio::time::sleep(Duration::from_secs(cmd.warmup)).await;
    received.store(0, Ordering::SeqCst);
    published.store(0, Ordering::SeqCst);

    let measure_start = Instant::now();
    let measure_end = measure_start + Duration::from_secs(cmd.duration);
    let mut next_sample = measure_start + Duration::from_secs(1);
    let mut last_received = 0_u64;
    let mut samples = Vec::new();

    while Instant::now() < measure_end {
        tokio::time::sleep(Duration::from_millis(10)).await;
        if Instant::now() >= next_sample {
            let current = received.load(Ordering::Relaxed);
            let delta = current.saturating_sub(last_received);
            samples.push(delta);
            last_received = current;
            next_sample += Duration::from_secs(1);
        }
    }

    running.store(false, Ordering::SeqCst);
    for handle in pub_handles {
        drop(handle.await);
    }
    for handle in poll_handles {
        drop(handle.await);
    }
    for handle in sub_handles {
        drop(handle.await);
    }
    for client in pub_clients {
        drop(client.disconnect().await);
    }
    for client in sub_clients {
        drop(client.disconnect().await);
    }

    let total_published = published.load(Ordering::Relaxed);
    let total_received = received.load(Ordering::Relaxed);
    let elapsed = measure_start.elapsed().as_secs_f64();
    let output = BenchOutput {
        library: "rumqtt-v5".to_owned(),
        mode: "throughput".to_owned(),
        meta: BenchMeta {
            schema_version: "1".to_owned(),
            run_id,
            client_id_base: base_id.clone(),
            started_at_utc: started_at,
            finished_at_utc: now_utc_string(),
        },
        config: BenchConfig {
            duration_secs: cmd.duration,
            warmup_secs: cmd.warmup,
            payload_size: cmd.payload_size,
            qos: cmd.qos as u8,
            topic: cmd.topic.clone(),
            filter,
            transport: endpoint.transport.clone(),
            publishers: cmd.publishers,
            subscribers: cmd.subscribers,
        },
        results: BenchResults::Throughput(ThroughputResults {
            published: total_published,
            received: total_received,
            elapsed_secs: elapsed,
            throughput_avg: total_received as f64 / elapsed,
            samples,
        }),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

async fn run_latency(
    cmd: &BenchCommand,
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
) -> anyhow::Result<()> {
    let started_at = now_utc_string();
    let base_id = cmd
        .client_id
        .clone()
        .unwrap_or_else(|| format!("rumqtt-v5-lat-{}", rand::random::<u32>()));
    let run_id = cmd
        .run_id
        .clone()
        .unwrap_or_else(|| format!("run-{}", rand::random::<u32>()));
    let filter = cmd.filter.clone().unwrap_or_else(|| cmd.topic.clone());

    let pub_options = build_options(format!("{base_id}-pub"), endpoint, ca_pem);
    let (pub_client, mut pub_eventloop) = AsyncClient::builder(pub_options).capacity(100).build();
    let pub_running = Arc::new(AtomicBool::new(true));
    let pub_run = Arc::clone(&pub_running);
    let pub_poll = tokio::spawn(async move {
        while pub_run.load(Ordering::Relaxed) {
            if pub_eventloop.poll().await.is_err() {
                break;
            }
        }
    });

    let sub_options = build_options(format!("{base_id}-sub"), endpoint, ca_pem);
    let (sub_client, mut sub_eventloop) = AsyncClient::builder(sub_options).capacity(100).build();
    sub_client.subscribe(filter.clone(), cmd.qos).await?;

    let latencies = Arc::new(Mutex::new(Vec::<u64>::with_capacity(10000)));
    let latencies_in = Arc::clone(&latencies);
    let sub_running = Arc::new(AtomicBool::new(true));
    let sub_run = Arc::clone(&sub_running);
    let sub_poll = tokio::spawn(async move {
        while sub_run.load(Ordering::Relaxed) {
            match sub_eventloop.poll().await {
                Ok(Event::Incoming(Incoming::Publish(publish))) => {
                    if publish.payload.len() >= 8 {
                        let mut buf = [0_u8; 8];
                        buf.copy_from_slice(&publish.payload[..8]);
                        let sent_nanos = u64::from_be_bytes(buf);
                        let now_nanos = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64)
                            .unwrap_or(sent_nanos);
                        latencies_in
                            .lock()
                            .expect("latencies lock")
                            .push((now_nanos.saturating_sub(sent_nanos)) / 1000);
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    let message_rate = 1000_u64;
    let interval_us = 1_000_000 / message_rate;
    let mut payload = vec![0_u8; cmd.payload_size.max(8)];

    for _ in 0..(cmd.warmup * message_rate) {
        let now_nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
        payload[0..8].copy_from_slice(&now_nanos.to_be_bytes());
        pub_client
            .publish(cmd.topic.clone(), cmd.qos, false, payload.clone())
            .await?;
        tokio::time::sleep(Duration::from_micros(interval_us)).await;
    }

    latencies.lock().expect("latencies lock").clear();
    let measure_start = Instant::now();
    while measure_start.elapsed() < Duration::from_secs(cmd.duration) {
        let now_nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
        payload[0..8].copy_from_slice(&now_nanos.to_be_bytes());
        pub_client
            .publish(cmd.topic.clone(), cmd.qos, false, payload.clone())
            .await?;
        tokio::time::sleep(Duration::from_micros(interval_us)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    pub_running.store(false, Ordering::SeqCst);
    sub_running.store(false, Ordering::SeqCst);
    drop(pub_poll.await);
    drop(sub_poll.await);
    drop(pub_client.disconnect().await);
    drop(sub_client.disconnect().await);

    let mut samples = latencies.lock().expect("latencies lock").clone();
    samples.sort_unstable();
    let (min_us, max_us, avg_us, p50_us, p95_us, p99_us) = if samples.is_empty() {
        (0, 0, 0.0, 0, 0, 0)
    } else {
        let len = samples.len();
        let min = samples[0];
        let max = samples[len - 1];
        let avg = samples.iter().sum::<u64>() as f64 / len as f64;
        let p50 = samples[len * 50 / 100];
        let p95 = samples[len * 95 / 100];
        let p99 = samples[len * 99 / 100];
        (min, max, avg, p50, p95, p99)
    };

    let sampled_points = if samples.len() <= 100 {
        samples.clone()
    } else {
        let step = (samples.len() / 100).max(1);
        samples.iter().step_by(step).copied().collect()
    };

    let output = BenchOutput {
        library: "rumqtt-v5".to_owned(),
        mode: "latency".to_owned(),
        meta: BenchMeta {
            schema_version: "1".to_owned(),
            run_id,
            client_id_base: base_id.clone(),
            started_at_utc: started_at,
            finished_at_utc: now_utc_string(),
        },
        config: BenchConfig {
            duration_secs: cmd.duration,
            warmup_secs: cmd.warmup,
            payload_size: cmd.payload_size,
            qos: cmd.qos as u8,
            topic: cmd.topic.clone(),
            filter,
            transport: endpoint.transport.clone(),
            publishers: 1,
            subscribers: 1,
        },
        results: BenchResults::Latency(LatencyResults {
            messages: samples.len() as u64,
            min_us,
            max_us,
            avg_us,
            p50_us,
            p95_us,
            p99_us,
            samples: sampled_points,
        }),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

async fn connect_once(
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
    client_id: String,
) -> anyhow::Result<()> {
    let options = build_options(client_id, endpoint, ca_pem);
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Incoming::ConnAck(_))) => break Ok::<(), anyhow::Error>(()),
                Ok(_) => {}
                Err(e) => break Err(anyhow::anyhow!("{e}")),
            }
        }
    })
    .await??;

    drop(client.disconnect().await);
    Ok(())
}

async fn run_connections(
    cmd: &BenchCommand,
    endpoint: &BrokerEndpoint,
    ca_pem: Option<&[u8]>,
) -> anyhow::Result<()> {
    let started_at = now_utc_string();
    let base_id = cmd
        .client_id
        .clone()
        .unwrap_or_else(|| format!("rumqtt-v5-conn-{}", rand::random::<u32>()));
    let run_id = cmd
        .run_id
        .clone()
        .unwrap_or_else(|| format!("run-{}", rand::random::<u32>()));
    let running = Arc::new(AtomicBool::new(true));
    let successful = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let connect_times = Arc::new(Mutex::new(Vec::<u64>::with_capacity(10000)));
    let counter = Arc::new(AtomicU64::new(0));

    let mut handles = Vec::with_capacity(cmd.concurrency);
    for _ in 0..cmd.concurrency {
        let running = Arc::clone(&running);
        let successful = Arc::clone(&successful);
        let failed = Arc::clone(&failed);
        let connect_times = Arc::clone(&connect_times);
        let counter = Arc::clone(&counter);
        let endpoint = endpoint.clone();
        let ca_pem = ca_pem.map(<[u8]>::to_vec);
        let base_id = base_id.clone();
        handles.push(tokio::spawn(async move {
            while running.load(Ordering::Relaxed) {
                let id = counter.fetch_add(1, Ordering::Relaxed);
                let client_id = format!("{base_id}-{id}");
                let start = Instant::now();
                match connect_once(&endpoint, ca_pem.as_deref(), client_id).await {
                    Ok(()) => {
                        successful.fetch_add(1, Ordering::Relaxed);
                        connect_times
                            .lock()
                            .expect("connect times lock")
                            .push(start.elapsed().as_micros() as u64);
                    }
                    Err(_) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    let measure_start = Instant::now();
    let mut next_sample = measure_start + Duration::from_secs(1);
    let mut last_count = 0_u64;
    let mut samples = Vec::new();
    while Instant::now() < measure_start + Duration::from_secs(cmd.duration) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if Instant::now() >= next_sample {
            let current = successful.load(Ordering::Relaxed);
            let delta = current.saturating_sub(last_count);
            samples.push(delta);
            last_count = current;
            next_sample += Duration::from_secs(1);
        }
    }

    running.store(false, Ordering::SeqCst);
    for handle in handles {
        drop(handle.await);
    }

    let total_successful = successful.load(Ordering::Relaxed);
    let total_failed = failed.load(Ordering::Relaxed);
    let elapsed = measure_start.elapsed().as_secs_f64();
    let mut times = connect_times.lock().expect("connect times lock").clone();
    times.sort_unstable();
    let (avg_connect_us, p50_connect_us, p95_connect_us, p99_connect_us) = if times.is_empty() {
        (0.0, 0, 0, 0)
    } else {
        let len = times.len();
        let avg = times.iter().sum::<u64>() as f64 / len as f64;
        let p50 = times[len * 50 / 100];
        let p95 = times[len * 95 / 100];
        let p99 = times[len * 99 / 100];
        (avg, p50, p95, p99)
    };

    let output = BenchOutput {
        library: "rumqtt-v5".to_owned(),
        mode: "connections".to_owned(),
        meta: BenchMeta {
            schema_version: "1".to_owned(),
            run_id,
            client_id_base: base_id.clone(),
            started_at_utc: started_at,
            finished_at_utc: now_utc_string(),
        },
        config: BenchConfig {
            duration_secs: cmd.duration,
            warmup_secs: 0,
            payload_size: 0,
            qos: 0,
            topic: String::new(),
            filter: String::new(),
            transport: endpoint.transport.clone(),
            publishers: 0,
            subscribers: 0,
        },
        results: BenchResults::Connections(ConnectionResults {
            total_connections: total_successful + total_failed,
            successful: total_successful,
            failed: total_failed,
            elapsed_secs: elapsed,
            connections_per_sec: total_successful as f64 / elapsed,
            avg_connect_us,
            p50_connect_us,
            p95_connect_us,
            p99_connect_us,
            samples,
        }),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cmd = BenchCommand::parse();
    let endpoint = parse_endpoint(&cmd.url).map_err(anyhow::Error::msg)?;
    let ca_pem = if let Some(path) = &cmd.ca_cert {
        Some(std::fs::read(path).map_err(|e| {
            anyhow::anyhow!("failed to read CA certificate '{}': {e}", path.display())
        })?)
    } else {
        None
    };

    match cmd.mode {
        BenchMode::Throughput => run_throughput(&cmd, &endpoint, ca_pem.as_deref()).await?,
        BenchMode::Latency => run_latency(&cmd, &endpoint, ca_pem.as_deref()).await?,
        BenchMode::Connections => run_connections(&cmd, &endpoint, ca_pem.as_deref()).await?,
    }

    Ok(())
}
