#![expect(clippy::too_many_lines)]
#![expect(clippy::cast_precision_loss)]

use anyhow::{Context, bail};
use async_trait::async_trait;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

#[cfg(feature = "alloc-metrics")]
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[cfg(feature = "alloc-metrics")]
struct CountingAllocator;

#[cfg(feature = "alloc-metrics")]
static ALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "alloc-metrics")]
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "alloc-metrics")]
unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: Delegates the unchanged layout to the system allocator.
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: Delegates the original allocation and layout to the system allocator.
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
}

#[derive(Parser)]
#[command(about = "Matched MQTT 5 library benchmark")]
struct Cli {
    #[arg(long, value_enum)]
    client: BackendKind,
    #[arg(long)]
    run_id: Option<String>,
    #[command(subcommand)]
    command: Workload,
}

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum BackendKind {
    Rumqttc,
    Mqtt5,
}

impl BackendKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Rumqttc => "rumqttc",
            Self::Mqtt5 => "mqtt5",
        }
    }
}

#[derive(Subcommand)]
enum Workload {
    Throughput(ThroughputArgs),
    Latency(LatencyArgs),
    Connections(ConnectionArgs),
}

#[derive(Args, Clone)]
struct CommonArgs {
    #[arg(long, default_value = "mqtt://127.0.0.1:1883")]
    broker_url: String,
    #[arg(long)]
    ca_cert: Option<PathBuf>,
    #[arg(long, default_value = "bench/library")]
    topic: String,
    #[arg(long)]
    filter: Option<String>,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(0..=1))]
    qos: u8,
    #[arg(long, default_value_t = 64)]
    payload_size: usize,
    #[arg(long, default_value_t = 2)]
    warmup_sec: u64,
    #[arg(long, default_value_t = 10)]
    duration_sec: u64,
    #[arg(long, default_value_t = 5)]
    drain_sec: u64,
    #[arg(long, default_value_t = 100)]
    window: usize,
    #[arg(long, default_value_t = 100)]
    receive_maximum: u16,
    #[arg(long, default_value_t = 30)]
    keepalive_sec: u16,
}

#[derive(Args, Clone)]
struct ThroughputArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, default_value_t = 1)]
    publishers: usize,
    #[arg(long, default_value_t = 1)]
    subscribers: usize,
}

#[derive(Args, Clone)]
struct LatencyArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, default_value_t = 1000)]
    rate: u64,
}

#[derive(Args, Clone)]
struct ConnectionArgs {
    #[arg(long, default_value = "mqtt://127.0.0.1:1883")]
    broker_url: String,
    #[arg(long)]
    ca_cert: Option<PathBuf>,
    #[arg(long, default_value_t = 10)]
    duration_sec: u64,
    #[arg(long, default_value_t = 1)]
    concurrency: usize,
    #[arg(long, default_value_t = 30)]
    keepalive_sec: u16,
}

#[derive(Debug)]
struct Delivery {
    topic: String,
    payload: Vec<u8>,
    correlation: Option<Vec<u8>>,
    received_at: Instant,
}

#[async_trait]
trait ClientAdapter: Send + Sync {
    async fn subscribe(
        &self,
        filter: &str,
        qos: u8,
    ) -> anyhow::Result<mpsc::UnboundedReceiver<Delivery>>;
    async fn publish(
        &self,
        topic: &str,
        payload: &[u8],
        correlation: Vec<u8>,
        qos: u8,
    ) -> anyhow::Result<()>;
    async fn disconnect(&self) -> anyhow::Result<()>;
}

struct RumqttAdapter {
    client: rumqttc_v5::AsyncClient,
    deliveries: Mutex<Option<mpsc::UnboundedReceiver<Delivery>>>,
}

impl RumqttAdapter {
    async fn connect(
        id: String,
        args: &CommonArgs,
        ca_certificate: Option<&[u8]>,
    ) -> anyhow::Result<Self> {
        let (host, port, tls) = parse_broker(&args.broker_url)?;
        let mut options = rumqttc_v5::MqttOptions::new(id, (host, port));
        options
            .set_clean_start(true)
            .set_session_expiry_interval(Some(0))
            .set_receive_maximum(Some(args.receive_maximum))
            .set_outgoing_inflight_upper_limit(args.window.min(u16::MAX as usize) as u16)
            .set_keep_alive(args.keepalive_sec);
        if tls {
            options.set_transport(
                ca_certificate.map_or_else(rumqttc_v5::Transport::tls_with_default_config, |ca| {
                    rumqttc_v5::Transport::tls(ca.to_vec(), None, None)
                }),
            );
        }
        let (client, mut eventloop) = rumqttc_v5::AsyncClient::builder(options)
            .capacity(args.window.max(1))
            .build();
        let (delivery_tx, delivery_rx) = mpsc::unbounded_channel();
        let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut connected_tx = Some(connected_tx);
            loop {
                match eventloop.poll().await {
                    Ok(rumqttc_v5::Event::Incoming(rumqttc_v5::Incoming::ConnAck(_))) => {
                        if let Some(tx) = connected_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                    Ok(rumqttc_v5::Event::Incoming(rumqttc_v5::Incoming::Publish(publish))) => {
                        let received_at = Instant::now();
                        let correlation = publish
                            .properties
                            .as_ref()
                            .and_then(|properties| properties.correlation_data.as_ref())
                            .map(|bytes| bytes.to_vec());
                        let _ = delivery_tx.send(Delivery {
                            topic: String::from_utf8_lossy(&publish.topic).into_owned(),
                            payload: publish.payload.to_vec(),
                            correlation,
                            received_at,
                        });
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        tokio::time::timeout(Duration::from_secs(10), connected_rx)
            .await
            .context("rumqttc CONNACK timeout")?
            .context("rumqttc event loop stopped before CONNACK")?;
        Ok(Self {
            client,
            deliveries: Mutex::new(Some(delivery_rx)),
        })
    }
}

#[async_trait]
impl ClientAdapter for RumqttAdapter {
    async fn subscribe(
        &self,
        filter: &str,
        qos: u8,
    ) -> anyhow::Result<mpsc::UnboundedReceiver<Delivery>> {
        self.client
            .subscribe_tracked(filter, rumqtt_qos(qos))
            .await?
            .wait_completion_async()
            .await?;
        self.deliveries
            .lock()
            .expect("delivery receiver lock")
            .take()
            .context("adapter can only create one subscription receiver")
    }

    async fn publish(
        &self,
        topic: &str,
        payload: &[u8],
        correlation: Vec<u8>,
        qos: u8,
    ) -> anyhow::Result<()> {
        let properties = rumqttc_v5::mqttbytes::v5::PublishProperties {
            correlation_data: Some(correlation.into()),
            ..Default::default()
        };
        self.client
            .publish_tracked(
                topic,
                payload,
                rumqttc_v5::PublishOptions::new(rumqtt_qos(qos)).properties(properties),
            )
            .await?
            .wait_completion_async()
            .await?;
        Ok(())
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        self.client
            .disconnect_with_timeout(Duration::from_secs(5))
            .await?;
        Ok(())
    }
}

struct Mqtt5Adapter {
    client: mqtt5::MqttClient,
}

impl Mqtt5Adapter {
    async fn connect(
        id: String,
        args: &CommonArgs,
        ca_certificate: Option<&[u8]>,
    ) -> anyhow::Result<Self> {
        let client = mqtt5::MqttClient::new(&id);
        let options = mqtt5::ConnectOptions::new(id)
            .with_clean_start(true)
            .with_session_expiry_interval(0)
            .with_receive_maximum(args.receive_maximum)
            .with_keep_alive(Duration::from_secs(u64::from(args.keepalive_sec)))
            .with_automatic_reconnect(false);
        if let Some(ca) = ca_certificate {
            let (host, port, tls) = parse_broker(&args.broker_url)?;
            if !tls {
                bail!("--ca-cert requires an mqtts:// broker URL");
            }
            let addresses = tokio::net::lookup_host((host.as_str(), port))
                .await?
                .collect::<Vec<_>>();
            let address = addresses
                .iter()
                .copied()
                .find(std::net::SocketAddr::is_ipv4)
                .or_else(|| addresses.first().copied())
                .with_context(|| format!("could not resolve TLS broker {host}:{port}"))?;
            let mut tls_config =
                mqtt5::transport::TlsConfig::new(address, host).with_system_roots(false);
            tls_config.load_ca_cert_pem_bytes(ca)?;
            client
                .connect_with_tls_and_options(tls_config, options)
                .await?;
        } else {
            client
                .connect_with_options(&args.broker_url, options)
                .await?;
        }
        client.set_queue_on_disconnect(false).await;
        Ok(Self { client })
    }
}

#[async_trait]
impl ClientAdapter for Mqtt5Adapter {
    async fn subscribe(
        &self,
        filter: &str,
        qos: u8,
    ) -> anyhow::Result<mpsc::UnboundedReceiver<Delivery>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let callback_tx = tx.clone();
        let options = mqtt5::SubscribeOptions {
            qos: mqtt5_qos(qos),
            ..Default::default()
        };
        self.client
            .subscribe_with_options(filter, options, move |message| {
                let received_at = Instant::now();
                let _ = callback_tx.send(Delivery {
                    topic: message.topic,
                    payload: message.payload,
                    correlation: message.properties.correlation_data,
                    received_at,
                });
            })
            .await?;
        Ok(rx)
    }

    async fn publish(
        &self,
        topic: &str,
        payload: &[u8],
        correlation: Vec<u8>,
        qos: u8,
    ) -> anyhow::Result<()> {
        let mut options = mqtt5::PublishOptions {
            qos: mqtt5_qos(qos),
            ..Default::default()
        };
        options.properties.correlation_data = Some(correlation);
        self.client
            .publish_with_options(topic, payload, options)
            .await?;
        Ok(())
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        self.client.disconnect().await?;
        Ok(())
    }
}

async fn connect_adapter(
    backend: BackendKind,
    id: String,
    args: &CommonArgs,
    ca_certificate: Option<&[u8]>,
) -> anyhow::Result<Arc<dyn ClientAdapter>> {
    match backend {
        BackendKind::Rumqttc => Ok(Arc::new(
            RumqttAdapter::connect(id, args, ca_certificate).await?,
        )),
        BackendKind::Mqtt5 => Ok(Arc::new(
            Mqtt5Adapter::connect(id, args, ca_certificate).await?,
        )),
    }
}

#[derive(Default)]
struct Counters {
    attempts: AtomicU64,
    accepted: AtomicU64,
    completed: AtomicU64,
    rejected: AtomicU64,
    unique: AtomicU64,
    in_window_unique: AtomicU64,
    duplicates: AtomicU64,
    malformed: AtomicU64,
    late: AtomicU64,
    missed_deadlines: AtomicU64,
}

#[derive(Serialize)]
struct Output {
    schema_version: u32,
    run_id: String,
    scenario: String,
    started_at_unix: u64,
    finished_at_unix: u64,
    client: BackendKind,
    config: Value,
    effective_config: Value,
    metrics: BTreeMap<String, f64>,
    samples: BTreeMap<String, Vec<f64>>,
    quality: Value,
    environment: Value,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();
    let run_id = cli
        .run_id
        .unwrap_or_else(|| format!("library-{}-{}", unix_secs(), rand::random::<u32>()));
    match cli.command {
        Workload::Throughput(args) => run_messages(cli.client, run_id, args, None).await,
        Workload::Latency(args) => {
            let throughput = ThroughputArgs {
                common: args.common,
                publishers: 1,
                subscribers: 1,
            };
            run_messages(cli.client, run_id, throughput, Some(args.rate)).await
        }
        Workload::Connections(args) => run_connections(cli.client, run_id, args).await,
    }
}

async fn run_messages(
    backend: BackendKind,
    run_id: String,
    args: ThroughputArgs,
    rate: Option<u64>,
) -> anyhow::Result<()> {
    if args.publishers == 0 || args.subscribers == 0 || args.common.window == 0 {
        bail!("publishers, subscribers, and window must be greater than zero");
    }
    if rate == Some(0) {
        bail!("rate must be greater than zero");
    }
    let ca_certificate =
        load_ca_certificate(&args.common.broker_url, args.common.ca_cert.as_deref())?;
    let started_at = unix_secs();
    let payload = Arc::new(vec![0x5a; args.common.payload_size]);
    let filter = args
        .common
        .filter
        .clone()
        .unwrap_or_else(|| args.common.topic.clone());
    let nonce = rand::random::<u64>();
    let phase = Arc::new(AtomicU8::new(1));
    let origin = Instant::now();
    let counters = Arc::new(Counters::default());
    let seen = Arc::new(Mutex::new(HashSet::<(usize, u64)>::new()));
    let latencies = Arc::new(Mutex::new(Vec::<u64>::new()));
    let running = Arc::new(AtomicBool::new(true));
    let sequence = Arc::new(AtomicU64::new(0));
    let measurement_deadline = Arc::new(OnceLock::<Instant>::new());

    let mut clients = Vec::<Arc<dyn ClientAdapter>>::new();
    let mut receiver_tasks = Vec::new();
    for index in 0..args.subscribers {
        let client = connect_adapter(
            backend,
            format!("{run_id}-sub-{index}"),
            &args.common,
            ca_certificate.as_deref(),
        )
        .await?;
        let mut receiver = client.subscribe(&filter, args.common.qos).await?;
        let expected_topic = args.common.topic.clone();
        let expected_payload = Arc::clone(&payload);
        let counters = Arc::clone(&counters);
        let seen = Arc::clone(&seen);
        let latencies = Arc::clone(&latencies);
        let phase = Arc::clone(&phase);
        let measurement_deadline = Arc::clone(&measurement_deadline);
        receiver_tasks.push(tokio::spawn(async move {
            while let Some(delivery) = receiver.recv().await {
                let Some((message_nonce, seq, sent_ns)) =
                    delivery.correlation.as_deref().and_then(decode_correlation)
                else {
                    counters.malformed.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                if delivery.topic != expected_topic
                    || delivery.payload.as_slice() != expected_payload.as_slice()
                {
                    counters.malformed.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let expected_nonce = nonce ^ u64::from(phase.load(Ordering::Acquire));
                if message_nonce != expected_nonce {
                    counters.late.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if !seen.lock().expect("seen lock").insert((index, seq)) {
                    counters.duplicates.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                counters.unique.fetch_add(1, Ordering::Relaxed);
                if measurement_deadline.get().is_some_and(|deadline| {
                    delivery_observed_in_window(delivery.received_at, *deadline)
                }) {
                    counters.in_window_unique.fetch_add(1, Ordering::Relaxed);
                }
                if sent_ns != 0 {
                    latencies
                        .lock()
                        .expect("latency lock")
                        .push(observed_latency_nanos(
                            origin,
                            delivery.received_at,
                            sent_ns,
                        ));
                }
            }
        }));
        clients.push(client);
    }

    let mut publishers = Vec::new();
    for index in 0..args.publishers {
        publishers.push(
            connect_adapter(
                backend,
                format!("{run_id}-pub-{index}"),
                &args.common,
                ca_certificate.as_deref(),
            )
            .await?,
        );
    }
    let mut publish_tasks = start_publishers(
        &publishers,
        &args.common,
        Arc::clone(&payload),
        Arc::clone(&running),
        Arc::clone(&phase),
        Arc::clone(&sequence),
        Arc::clone(&counters),
        nonce,
        origin,
        rate,
        Instant::now() + Duration::from_secs(args.common.warmup_sec),
    );

    tokio::time::sleep(Duration::from_secs(args.common.warmup_sec)).await;
    running.store(false, Ordering::Release);
    while publish_tasks.join_next().await.is_some() {}
    wait_for_quiet(&counters.unique, Duration::from_secs(args.common.drain_sec)).await;
    phase.store(2, Ordering::Release);
    reset_counters(&counters);
    seen.lock().expect("seen lock").clear();
    latencies.lock().expect("latency lock").clear();
    running.store(true, Ordering::Release);
    let cpu_start = process_cpu_nanos();
    let alloc_start = allocation_snapshot();
    let measure_start = Instant::now();
    let measure_deadline = measure_start + Duration::from_secs(args.common.duration_sec);
    measurement_deadline
        .set(measure_deadline)
        .expect("measurement deadline is set exactly once");
    publish_tasks = start_publishers(
        &publishers,
        &args.common,
        Arc::clone(&payload),
        Arc::clone(&running),
        Arc::clone(&phase),
        Arc::clone(&sequence),
        Arc::clone(&counters),
        nonce,
        origin,
        rate,
        measure_deadline,
    );
    let mut samples = Vec::new();
    let mut last_in_window_unique = 0;
    while Instant::now() < measure_deadline {
        tokio::time::sleep_until(
            (Instant::now() + Duration::from_secs(1))
                .min(measure_deadline)
                .into(),
        )
        .await;
        let current = counters.in_window_unique.load(Ordering::Relaxed);
        samples.push(current.saturating_sub(last_in_window_unique) as f64);
        last_in_window_unique = current;
    }
    let measured_elapsed = measure_start.elapsed().as_secs_f64();
    running.store(false, Ordering::Release);
    while publish_tasks.join_next().await.is_some() {}
    let expected = counters
        .accepted
        .load(Ordering::Relaxed)
        .saturating_mul(args.subscribers as u64);
    let drain = tokio::time::timeout(Duration::from_secs(args.common.drain_sec), async {
        loop {
            let received = counters.unique.load(Ordering::Relaxed);
            if received >= expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let total_elapsed = measure_start.elapsed().as_secs_f64();
    let cpu_nanos = process_cpu_nanos().saturating_sub(cpu_start);
    let alloc = allocation_snapshot().delta(alloc_start);

    for client in publishers.iter().chain(clients.iter()) {
        let _ = client.disconnect().await;
    }
    for task in receiver_tasks {
        task.abort();
    }

    let total_unique = counters.unique.load(Ordering::Relaxed);
    let in_window_unique = counters.in_window_unique.load(Ordering::Relaxed);
    if in_window_unique > last_in_window_unique {
        let delayed_in_window = in_window_unique - last_in_window_unique;
        if let Some(last_sample) = samples.last_mut() {
            *last_sample += delayed_in_window as f64;
        } else {
            samples.push(delayed_in_window as f64);
        }
    }
    let lost = expected.saturating_sub(total_unique);
    let maintained_rate = rate.is_none_or(|requested| {
        counters.accepted.load(Ordering::Relaxed) as f64 / measured_elapsed
            >= requested as f64 * 0.99
    });
    let valid = drain.is_ok()
        && lost == 0
        && counters.duplicates.load(Ordering::Relaxed) == 0
        && counters.malformed.load(Ordering::Relaxed) == 0
        && counters.rejected.load(Ordering::Relaxed) == 0
        && maintained_rate;
    let mut metrics = counter_metrics(&counters);
    metrics.insert("expected_deliveries".into(), expected as f64);
    metrics.insert("lost".into(), lost as f64);
    metrics.insert("elapsed_sec".into(), measured_elapsed);
    metrics.insert("elapsed_with_drain_sec".into(), total_elapsed);
    insert_delivery_window_metrics(
        &mut metrics,
        in_window_unique,
        total_unique,
        measured_elapsed,
    );
    metrics.insert("cpu_nanos".into(), cpu_nanos as f64);
    metrics.insert(
        "cpu_nanos_per_delivery".into(),
        ratio(cpu_nanos, total_unique),
    );
    if let Some(rss) = resident_set_bytes() {
        metrics.insert("rss_max_bytes".into(), rss as f64);
    }
    if alloc.enabled {
        metrics.insert("allocation_calls".into(), alloc.calls as f64);
        metrics.insert("allocated_bytes".into(), alloc.bytes as f64);
        metrics.insert(
            "allocated_bytes_per_delivery".into(),
            ratio(alloc.bytes, total_unique),
        );
    }
    let mut latency = latencies.lock().expect("latency lock").clone();
    latency.sort_unstable();
    if rate.is_some() {
        insert_latency_metrics(&mut metrics, &latency);
        metrics.insert("offered_rate".into(), rate.unwrap_or_default() as f64);
        metrics.insert(
            "achieved_rate".into(),
            counters.accepted.load(Ordering::Relaxed) as f64 / measured_elapsed,
        );
    }
    let mut samples_out = BTreeMap::new();
    samples_out.insert("unique_deliveries_per_sec".into(), samples);
    if rate.is_some() {
        samples_out.insert(
            "latency_us".into(),
            latency.iter().map(|value| *value as f64 / 1000.0).collect(),
        );
    }
    print_output(Output {
        schema_version: 2,
        run_id,
        scenario: if rate.is_some() {
            "matched-library-latency".into()
        } else {
            "matched-library-throughput".into()
        },
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        client: backend,
        config: json!({
            "protocol": "v5", "transport": transport_name(&args.common.broker_url),
            "broker_url": args.common.broker_url, "topic": args.common.topic, "filter": filter,
            "qos": args.common.qos, "retain": false, "payload_size": args.common.payload_size,
            "warmup_sec": args.common.warmup_sec, "duration_sec": args.common.duration_sec,
            "drain_sec": args.common.drain_sec, "window": args.common.window,
            "receive_maximum": args.common.receive_maximum, "keepalive_sec": args.common.keepalive_sec,
            "clean_start": true, "session_expiry_interval": 0,
            "publishers": args.publishers, "subscribers": args.subscribers, "rate": rate,
            "message_identity": "correlation-data-v1",
            "ca_certificate": ca_certificate_metadata(
                args.common.ca_cert.as_deref(),
                ca_certificate.as_deref(),
            )
        }),
        effective_config: json!({
            "backend": backend.name(), "publish_completion": if args.common.qos == 0 {"socket-flush"} else {"puback"},
            "automatic_reconnect": false, "offline_queue": false,
            "local_admission_observable": false,
            "locally_accepted_definition": "successful-public-publish-completion"
        }),
        metrics,
        samples: samples_out,
        quality: json!({
            "valid": valid,
            "complete_drain": drain.is_ok(),
            "overloaded": !maintained_rate,
            "coordinated_omission_warning": counters.missed_deadlines.load(Ordering::Relaxed) != 0
        }),
        environment: environment(backend),
    })
}

async fn wait_for_quiet(counter: &AtomicU64, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut previous = counter.load(Ordering::Relaxed);
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let current = counter.load(Ordering::Relaxed);
        if current == previous || Instant::now() >= deadline {
            break;
        }
        previous = current;
    }
}

#[allow(clippy::too_many_arguments)]
fn start_publishers(
    publishers: &[Arc<dyn ClientAdapter>],
    args: &CommonArgs,
    payload: Arc<Vec<u8>>,
    running: Arc<AtomicBool>,
    phase: Arc<AtomicU8>,
    sequence: Arc<AtomicU64>,
    counters: Arc<Counters>,
    nonce: u64,
    origin: Instant,
    rate: Option<u64>,
    stop_at: Instant,
) -> JoinSet<()> {
    let semaphore = Arc::new(Semaphore::new(args.window));
    let mut tasks = JoinSet::new();
    for client in publishers {
        let client = Arc::clone(client);
        let running = Arc::clone(&running);
        let phase = Arc::clone(&phase);
        let sequence = Arc::clone(&sequence);
        let counters = Arc::clone(&counters);
        let semaphore = Arc::clone(&semaphore);
        let payload = Arc::clone(&payload);
        let topic = args.topic.clone();
        let qos = args.qos;
        let per_publisher_rate = rate.map(|value| (value / publishers.len() as u64).max(1));
        let interval = per_publisher_rate.map(|value| Duration::from_nanos(1_000_000_000 / value));
        tasks.spawn(async move {
            let mut deadline = Instant::now();
            let mut outstanding = JoinSet::new();
            while publishing_active(&running, stop_at) {
                if let Some(interval) = interval {
                    deadline += interval;
                    let now = Instant::now();
                    if now > deadline {
                        counters.missed_deadlines.fetch_add(1, Ordering::Relaxed);
                        deadline = now + interval;
                    }
                    tokio::time::sleep_until(deadline.min(stop_at).into()).await;
                    if !publishing_active(&running, stop_at) {
                        break;
                    }
                }
                let permit = tokio::select! {
                    permit = Arc::clone(&semaphore).acquire_owned() => match permit {
                        Ok(permit) => permit,
                        Err(_) => break,
                    },
                    () = tokio::time::sleep_until(stop_at.into()) => break,
                };
                if !publishing_active(&running, stop_at) {
                    drop(permit);
                    break;
                }
                counters.attempts.fetch_add(1, Ordering::Relaxed);
                let seq = sequence.fetch_add(1, Ordering::Relaxed);
                let sent = rate.map_or(0, |_| origin.elapsed().as_nanos() as u64);
                let correlation =
                    encode_correlation(nonce ^ u64::from(phase.load(Ordering::Acquire)), seq, sent);
                let client = Arc::clone(&client);
                let counters = Arc::clone(&counters);
                let topic = topic.clone();
                let payload = Arc::clone(&payload);
                outstanding.spawn(async move {
                    match client.publish(&topic, &payload, correlation, qos).await {
                        Ok(()) => {
                            counters.accepted.fetch_add(1, Ordering::Relaxed);
                            counters.completed.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            counters.rejected.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    drop(permit);
                });
            }
            while outstanding.join_next().await.is_some() {}
        });
    }
    tasks
}

fn publishing_active(running: &AtomicBool, stop_at: Instant) -> bool {
    running.load(Ordering::Acquire) && Instant::now() < stop_at
}

async fn run_connections(
    backend: BackendKind,
    run_id: String,
    args: ConnectionArgs,
) -> anyhow::Result<()> {
    if args.concurrency == 0 {
        bail!("concurrency must be greater than zero");
    }
    let ca_certificate = Arc::new(load_ca_certificate(
        &args.broker_url,
        args.ca_cert.as_deref(),
    )?);
    let started_at = unix_secs();
    let successful = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let running = Arc::new(AtomicBool::new(true));
    let common = CommonArgs {
        broker_url: args.broker_url.clone(),
        ca_cert: args.ca_cert.clone(),
        topic: String::new(),
        filter: None,
        qos: 0,
        payload_size: 0,
        warmup_sec: 0,
        duration_sec: args.duration_sec,
        drain_sec: 5,
        window: 1,
        receive_maximum: 100,
        keepalive_sec: args.keepalive_sec,
    };
    let start = Instant::now();
    let mut tasks = JoinSet::new();
    for worker in 0..args.concurrency {
        let successful = Arc::clone(&successful);
        let failed = Arc::clone(&failed);
        let running = Arc::clone(&running);
        let common = common.clone();
        let run_id = run_id.clone();
        let ca_certificate = Arc::clone(&ca_certificate);
        tasks.spawn(async move {
            let mut iteration = 0u64;
            while running.load(Ordering::Acquire) {
                match connect_adapter(
                    backend,
                    format!("{run_id}-{worker}-{iteration}"),
                    &common,
                    ca_certificate.as_deref(),
                )
                .await
                {
                    Ok(client) if client.disconnect().await.is_ok() => {
                        successful.fetch_add(1, Ordering::Relaxed);
                    }
                    _ => {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
                iteration += 1;
            }
        });
    }
    tokio::time::sleep(Duration::from_secs(args.duration_sec)).await;
    running.store(false, Ordering::Release);
    while tasks.join_next().await.is_some() {}
    let elapsed = start.elapsed().as_secs_f64();
    let count = successful.load(Ordering::Relaxed);
    let failures = failed.load(Ordering::Relaxed);
    let mut metrics = BTreeMap::new();
    metrics.insert("successful".into(), count as f64);
    metrics.insert("failed".into(), failures as f64);
    metrics.insert("elapsed_sec".into(), elapsed);
    metrics.insert("connections_sec".into(), count as f64 / elapsed);
    print_output(Output {
        schema_version: 2,
        run_id,
        scenario: "matched-library-connections".into(),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        client: backend,
        config: json!({"protocol":"v5", "broker_url":args.broker_url, "duration_sec":args.duration_sec,
        "concurrency":args.concurrency, "clean_start":true, "session_expiry_interval":0,
        "keepalive_sec":args.keepalive_sec,
        "ca_certificate": ca_certificate_metadata(
            args.ca_cert.as_deref(),
            ca_certificate.as_deref(),
        )}),
        effective_config: json!({"backend":backend.name(), "cycle_completion":"connack-and-disconnect-flush"}),
        metrics,
        samples: BTreeMap::new(),
        quality: json!({"valid":failures == 0}),
        environment: environment(backend),
    })
}

fn encode_correlation(nonce: u64, sequence: u64, sent_nanos: u64) -> Vec<u8> {
    let mut value = Vec::with_capacity(24);
    value.extend_from_slice(&nonce.to_be_bytes());
    value.extend_from_slice(&sequence.to_be_bytes());
    value.extend_from_slice(&sent_nanos.to_be_bytes());
    value
}

fn decode_correlation(value: &[u8]) -> Option<(u64, u64, u64)> {
    if value.len() != 24 {
        return None;
    }
    Some((
        u64::from_be_bytes(value[0..8].try_into().ok()?),
        u64::from_be_bytes(value[8..16].try_into().ok()?),
        u64::from_be_bytes(value[16..24].try_into().ok()?),
    ))
}

fn reset_counters(counters: &Counters) {
    for counter in [
        &counters.attempts,
        &counters.accepted,
        &counters.completed,
        &counters.rejected,
        &counters.unique,
        &counters.in_window_unique,
        &counters.duplicates,
        &counters.malformed,
        &counters.late,
        &counters.missed_deadlines,
    ] {
        counter.store(0, Ordering::Relaxed);
    }
}

fn counter_metrics(counters: &Counters) -> BTreeMap<String, f64> {
    [
        ("publish_attempts", &counters.attempts),
        ("locally_accepted", &counters.accepted),
        ("publish_completed", &counters.completed),
        ("rejected", &counters.rejected),
        ("unique_deliveries", &counters.unique),
        ("duplicates", &counters.duplicates),
        ("malformed", &counters.malformed),
        ("late", &counters.late),
        ("missed_deadlines", &counters.missed_deadlines),
    ]
    .into_iter()
    .map(|(name, counter)| (name.into(), counter.load(Ordering::Relaxed) as f64))
    .collect()
}

fn insert_latency_metrics(metrics: &mut BTreeMap<String, f64>, values: &[u64]) {
    if values.is_empty() {
        for name in ["p50_us", "p95_us", "p99_us", "max_us"] {
            metrics.insert(name.into(), 0.0);
        }
        return;
    }
    for (name, percentile) in [("p50_us", 50), ("p95_us", 95), ("p99_us", 99)] {
        let index = ((values.len() - 1) * percentile) / 100;
        metrics.insert(name.into(), values[index] as f64 / 1000.0);
    }
    metrics.insert("max_us".into(), values[values.len() - 1] as f64 / 1000.0);
}

fn insert_delivery_window_metrics(
    metrics: &mut BTreeMap<String, f64>,
    in_window_unique: u64,
    total_unique: u64,
    measured_elapsed: f64,
) {
    metrics.insert(
        "in_window_unique_deliveries".into(),
        in_window_unique as f64,
    );
    metrics.insert(
        "drain_deliveries".into(),
        total_unique.saturating_sub(in_window_unique) as f64,
    );
    metrics.insert(
        "throughput_msg_sec".into(),
        in_window_unique as f64 / measured_elapsed,
    );
}

fn delivery_observed_in_window(received_at: Instant, measurement_deadline: Instant) -> bool {
    received_at <= measurement_deadline
}

fn observed_latency_nanos(origin: Instant, received_at: Instant, sent_nanos: u64) -> u64 {
    let latency = received_at
        .saturating_duration_since(origin)
        .as_nanos()
        .saturating_sub(u128::from(sent_nanos));
    u64::try_from(latency).unwrap_or(u64::MAX)
}

const fn rumqtt_qos(qos: u8) -> rumqttc_v5::mqttbytes::QoS {
    if qos == 0 {
        rumqttc_v5::mqttbytes::QoS::AtMostOnce
    } else {
        rumqttc_v5::mqttbytes::QoS::AtLeastOnce
    }
}

const fn mqtt5_qos(qos: u8) -> mqtt5::QoS {
    if qos == 0 {
        mqtt5::QoS::AtMostOnce
    } else {
        mqtt5::QoS::AtLeastOnce
    }
}

fn load_ca_certificate(
    broker_url: &str,
    ca_cert: Option<&Path>,
) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(path) = ca_cert else {
        return Ok(None);
    };
    if !parse_broker(broker_url)?.2 {
        bail!("--ca-cert requires an mqtts:// broker URL");
    }
    std::fs::read(path)
        .with_context(|| format!("failed to read CA certificate {}", path.display()))
        .map(Some)
}

fn ca_certificate_metadata(path: Option<&Path>, certificate: Option<&[u8]>) -> Value {
    match (path, certificate) {
        (Some(path), Some(certificate)) => json!({
            "path": path,
            "sha256": Sha256::digest(certificate).iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        }),
        _ => Value::Null,
    }
}

fn parse_broker(url: &str) -> anyhow::Result<(String, u16, bool)> {
    let parsed = url_parser::Url::parse(url).context("invalid matched benchmark broker URL")?;
    let (tls, default_port) = match parsed.scheme() {
        "mqtt" => (false, 1883),
        "mqtts" => (true, 8883),
        _ => bail!("matched benchmark supports mqtt:// and mqtts:// broker URLs"),
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("matched benchmark broker URLs do not support embedded credentials");
    }
    let host = match parsed.host() {
        Some(url_parser::Host::Domain(host)) => host.to_owned(),
        Some(url_parser::Host::Ipv4(host)) => host.to_string(),
        Some(url_parser::Host::Ipv6(host)) => host.to_string(),
        None => bail!("matched benchmark broker URL must include a host"),
    };
    Ok((host, parsed.port().unwrap_or(default_port), tls))
}

fn transport_name(url: &str) -> &'static str {
    if url_parser::Url::parse(url)
        .ok()
        .is_some_and(|parsed| parsed.scheme() == "mqtts")
    {
        "tls"
    } else {
        "tcp"
    }
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn process_cpu_nanos() -> u64 {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage initializes the provided rusage on success.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0 {
            // SAFETY: Successful getrusage initialized the value.
            let usage = unsafe { usage.assume_init() };
            let seconds = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as u64;
            let micros = (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as u64;
            return seconds * 1_000_000_000 + micros * 1000;
        }
    }
    0
}

fn resident_set_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    line.split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()
        .map(|kb| kb * 1024)
}

#[derive(Clone, Copy)]
struct AllocationSnapshot {
    enabled: bool,
    calls: u64,
    bytes: u64,
}

impl AllocationSnapshot {
    const fn delta(self, earlier: Self) -> Self {
        Self {
            enabled: self.enabled,
            calls: self.calls.saturating_sub(earlier.calls),
            bytes: self.bytes.saturating_sub(earlier.bytes),
        }
    }
}

fn allocation_snapshot() -> AllocationSnapshot {
    #[cfg(feature = "alloc-metrics")]
    return AllocationSnapshot {
        enabled: true,
        calls: ALLOC_CALLS.load(Ordering::Relaxed),
        bytes: ALLOC_BYTES.load(Ordering::Relaxed),
    };
    #[cfg(not(feature = "alloc-metrics"))]
    AllocationSnapshot {
        enabled: false,
        calls: 0,
        bytes: 0,
    }
}

fn ratio(value: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        value as f64 / count as f64
    }
}

fn environment(backend: BackendKind) -> Value {
    let cpu_model = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|contents| {
            contents
                .lines()
                .find_map(|line| line.strip_prefix("model name\t: ").map(str::to_owned))
        });
    let memory_bytes = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                line.strip_prefix("MemTotal:")
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|kb| kb * 1024)
            })
        });
    json!({
        "git_commit": command_stdout("git", &["rev-parse", "HEAD"]),
        "rustc": command_stdout("rustc", &["--version", "--verbose"]),
        "target": command_stdout("rustc", &["-vV"]).and_then(|output| output.lines().find_map(|line| line.strip_prefix("host: ").map(str::to_owned))),
        "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
        "logical_cpu_count": std::thread::available_parallelism().map_or(1, usize::from),
        "cpu_model": cpu_model, "total_memory_bytes": memory_bytes,
        "allocator": if cfg!(feature = "alloc-metrics") {"system-counting"} else {"system"},
        "cargo_features": if cfg!(feature = "alloc-metrics") {vec!["alloc-metrics"]} else {Vec::<&str>::new()},
        "optimization_profile": if cfg!(debug_assertions) {"dev"} else {"release"},
        "library": backend.name(),
        "library_version": match backend { BackendKind::Rumqttc => "0.34.0-alpha", BackendKind::Mqtt5 => "0.38.0" },
        "mqtt5_source": "registry+https://github.com/rust-lang/crates.io-index"
    })
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn print_output(output: Output) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CountingAdapter {
        publishes: AtomicU64,
    }

    #[async_trait]
    impl ClientAdapter for CountingAdapter {
        async fn subscribe(
            &self,
            _filter: &str,
            _qos: u8,
        ) -> anyhow::Result<mpsc::UnboundedReceiver<Delivery>> {
            unreachable!("publisher test adapter does not subscribe")
        }

        async fn publish(
            &self,
            _topic: &str,
            _payload: &[u8],
            _correlation: Vec<u8>,
            _qos: u8,
        ) -> anyhow::Result<()> {
            self.publishes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn disconnect(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn correlation_round_trips_and_rejects_wrong_length() {
        let encoded = encode_correlation(1, 2, 3);
        assert_eq!(decode_correlation(&encoded), Some((1, 2, 3)));
        assert_eq!(decode_correlation(&encoded[..23]), None);
    }

    #[test]
    fn latency_percentiles_are_reported_in_microseconds() {
        let mut metrics = BTreeMap::new();
        insert_latency_metrics(&mut metrics, &[1_000, 2_000, 3_000, 4_000]);
        assert_eq!(metrics["p50_us"], 2.0);
        assert_eq!(metrics["max_us"], 4.0);
    }

    #[test]
    fn throughput_excludes_deliveries_observed_during_drain() {
        let mut metrics = BTreeMap::new();
        insert_delivery_window_metrics(&mut metrics, 80, 100, 2.0);

        assert_eq!(metrics["in_window_unique_deliveries"], 80.0);
        assert_eq!(metrics["drain_deliveries"], 20.0);
        assert_eq!(metrics["throughput_msg_sec"], 40.0);
    }

    #[test]
    fn delivery_window_uses_adapter_observation_time() {
        let deadline = Instant::now();
        let before = deadline - Duration::from_nanos(1);
        let after = deadline + Duration::from_nanos(1);

        assert!(delivery_observed_in_window(before, deadline));
        assert!(delivery_observed_in_window(deadline, deadline));
        assert!(!delivery_observed_in_window(after, deadline));
    }

    #[test]
    fn latency_uses_adapter_observation_time_not_receiver_processing_time() {
        let origin = Instant::now();
        let sent_nanos = Duration::from_millis(2).as_nanos() as u64;
        let received_at = origin + Duration::from_millis(7);

        std::thread::sleep(Duration::from_millis(10));

        assert_eq!(
            observed_latency_nanos(origin, received_at, sent_nanos),
            Duration::from_millis(5).as_nanos() as u64
        );
    }

    #[test]
    fn broker_parser_handles_dns_ipv4_and_bracketed_ipv6() {
        assert_eq!(
            parse_broker("mqtt://broker.example/mqtt").unwrap(),
            ("broker.example".into(), 1883, false)
        );
        assert_eq!(
            parse_broker("mqtts://127.0.0.1:9443").unwrap(),
            ("127.0.0.1".into(), 9443, true)
        );
        assert_eq!(
            parse_broker("mqtt://[::1]:2883").unwrap(),
            ("::1".into(), 2883, false)
        );
        assert_eq!(
            parse_broker("mqtts://[2001:db8::1]").unwrap(),
            ("2001:db8::1".into(), 8883, true)
        );
    }

    #[test]
    fn broker_parser_rejects_invalid_ports_and_unsupported_schemes() {
        assert!(parse_broker("mqtt://localhost:not-a-port").is_err());
        assert!(parse_broker("mqtt://localhost:70000").is_err());
        assert!(parse_broker("ws://localhost:9001").is_err());
    }

    #[test]
    fn ca_certificate_requires_tls_and_records_sha256() {
        let path = Path::new("/tmp/fixture-ca.crt");
        let error = load_ca_certificate("mqtt://localhost:1883", Some(path))
            .expect_err("CA certificate with plain MQTT must be rejected");
        assert!(error.to_string().contains("requires an mqtts://"));

        let metadata = ca_certificate_metadata(Some(path), Some(b"fixture-ca"));
        assert_eq!(metadata["path"], "/tmp/fixture-ca.crt");
        assert_eq!(
            metadata["sha256"],
            "fca046ca96fabdc57856c287f889f3a2a20dc3192abefa0443ae0e6505595fdf"
        );
    }

    #[tokio::test]
    async fn publisher_deadline_interrupts_rate_sleep_without_publishing() {
        let adapter = Arc::new(CountingAdapter {
            publishes: AtomicU64::new(0),
        });
        let publishers: Vec<Arc<dyn ClientAdapter>> = vec![adapter.clone()];
        let args = CommonArgs {
            broker_url: "mqtt://127.0.0.1:1883".into(),
            ca_cert: None,
            topic: "bench/test".into(),
            filter: None,
            qos: 1,
            payload_size: 1,
            warmup_sec: 0,
            duration_sec: 1,
            drain_sec: 1,
            window: 1,
            receive_maximum: 1,
            keepalive_sec: 1,
        };
        let running = Arc::new(AtomicBool::new(true));
        let counters = Arc::new(Counters::default());
        let origin = Instant::now();
        let mut tasks = start_publishers(
            &publishers,
            &args,
            Arc::new(vec![0]),
            running,
            Arc::new(AtomicU8::new(1)),
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&counters),
            1,
            origin,
            Some(1),
            origin + Duration::from_millis(20),
        );

        tokio::time::timeout(Duration::from_millis(200), async {
            while tasks.join_next().await.is_some() {}
        })
        .await
        .expect("publisher should stop at its deadline");

        assert_eq!(counters.attempts.load(Ordering::Relaxed), 0);
        assert_eq!(adapter.publishes.load(Ordering::Relaxed), 0);
    }
}
