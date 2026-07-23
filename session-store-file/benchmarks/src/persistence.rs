use std::collections::BTreeMap;
use std::future::Future;
use std::hint::black_box;
use std::io::Cursor;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, bail};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::json;

use rumqttc_store_core::bench_instrumentation as envelope;
use rumqttc_store_core::{DEFAULT_MAX_CHECKPOINT_SIZE, FileStore, FileStoreOptions};

use super::{BenchOutput, CommonArgs, Protocol, environment, print_output, run_id, unix_secs};

const COUNTS: [usize; 5] = [0, 1, 10, 100, 1_000];

#[derive(Subcommand, Debug)]
pub enum PersistenceCommand {
    Envelope(EnvelopeArgs),
    Codec(CodecArgs),
    FileStore(FileStoreArgs),
    Coordination(CoordinationArgs),
    Growth(GrowthArgs),
    Mqtt(MqttArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EnvelopeMode {
    Encode,
    Decode,
    Crc32c,
    ErrorPaths,
}

#[derive(Args, Debug)]
pub struct EnvelopeArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, value_enum, default_value = "encode")]
    mode: EnvelopeMode,
    #[arg(long, default_value = "1024")]
    payload_size: usize,
    #[arg(long, default_value = "100")]
    samples: usize,
    #[arg(long, default_value = "10")]
    operations_per_sample: usize,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CodecMode {
    Encode,
    Decode,
}

#[derive(Args, Debug)]
pub struct CodecArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, value_enum, default_value = "encode")]
    mode: CodecMode,
    #[arg(long, default_value = "10")]
    inflight: usize,
    #[arg(long, default_value = "1024")]
    payload_size: usize,
    #[arg(long, default_value = "1", value_parser = super::parse_qos)]
    qos: u8,
    #[arg(long, default_value = "100")]
    samples: usize,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum StoreOperation {
    SaveCreate,
    SaveReplace,
    SaveGrowing,
    SaveShrinking,
    LoadPresent,
    LoadMissing,
    ClearPresent,
    ClearMissing,
    InspectPresent,
    InspectMissing,
    QuarantinePresent,
    QuarantineMissing,
}

#[derive(Args, Debug)]
pub struct FileStoreArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, value_enum, default_value = "save-replace")]
    operation: StoreOperation,
    #[arg(long, default_value = "1024")]
    payload_size: usize,
    #[arg(long, default_value = "50")]
    samples: usize,
}

#[derive(Args, Debug)]
pub struct CoordinationArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, default_value = "8")]
    concurrency: usize,
    #[arg(long, default_value = "100")]
    operations: usize,
    #[arg(long, default_value_t = false)]
    different_keys: bool,
}

#[derive(Args, Debug)]
pub struct GrowthArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, default_value = "1024")]
    payload_size: usize,
    #[arg(long, default_value = "1", value_parser = super::parse_qos)]
    qos: u8,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PersistenceMode {
    Disabled,
    Enabled,
}

#[derive(Args, Debug)]
pub struct MqttArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, value_enum, default_value = "enabled")]
    persistence: PersistenceMode,
    #[arg(long, default_value = "mqtt://127.0.0.1:1883")]
    broker_url: String,
    #[arg(long, default_value = "100")]
    messages: usize,
    #[arg(long, default_value = "10")]
    warmup_messages: usize,
    #[arg(long, default_value = "10")]
    inflight: usize,
    #[arg(long, default_value = "1024")]
    payload_size: usize,
    #[arg(long, default_value = "1", value_parser = super::parse_qos)]
    qos: u8,
}

pub async fn run(command: PersistenceCommand) -> anyhow::Result<()> {
    match command {
        PersistenceCommand::Envelope(args) => run_envelope(&args),
        PersistenceCommand::Codec(args) => run_codec(&args),
        PersistenceCommand::FileStore(args) => run_file_store(args).await,
        PersistenceCommand::Coordination(args) => run_coordination(args).await,
        PersistenceCommand::Growth(args) => run_growth(&args),
        PersistenceCommand::Mqtt(args) => run_mqtt(args).await,
    }
}

#[derive(Debug, Default)]
struct StoreMeasurements {
    save_requests: AtomicU64,
    save_successes: AtomicU64,
    save_failures: AtomicU64,
    clear_requests: AtomicU64,
    checkpoint_bytes: AtomicU64,
    final_checkpoint_bytes: AtomicU64,
    barriers_ns: Mutex<Vec<u64>>,
}

impl StoreMeasurements {
    fn reset(&self) {
        self.save_requests.store(0, Ordering::Relaxed);
        self.save_successes.store(0, Ordering::Relaxed);
        self.save_failures.store(0, Ordering::Relaxed);
        self.clear_requests.store(0, Ordering::Relaxed);
        self.checkpoint_bytes.store(0, Ordering::Relaxed);
        self.final_checkpoint_bytes.store(0, Ordering::Relaxed);
        self.barriers_ns
            .lock()
            .expect("barrier samples lock")
            .clear();
    }
}

#[derive(Clone, Debug)]
struct MeasuringV4Store {
    inner: rumqttc_store::v4::SessionFileStore,
    measurements: Arc<StoreMeasurements>,
}

type V4StoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, rumqttc_v4::SessionStoreError>> + Send + 'a>>;
type V5StoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, rumqttc_v5::SessionStoreError>> + Send + 'a>>;

impl rumqttc_v4::SessionStore for MeasuringV4Store {
    fn load<'a>(
        &'a self,
        key: &'a rumqttc_v4::SessionStoreKey,
    ) -> V4StoreFuture<'a, Option<rumqttc_v4::PersistedSession>> {
        self.inner.load(key)
    }

    fn save<'a>(
        &'a self,
        key: &'a rumqttc_v4::SessionStoreKey,
        session: &'a rumqttc_v4::PersistedSession,
    ) -> V4StoreFuture<'a, ()> {
        self.measurements
            .save_requests
            .fetch_add(1, Ordering::Relaxed);
        let checkpoint_bytes = session
            .encode()
            .map(|bytes| bytes.len() + envelope::ENVELOPE_OVERHEAD);
        Box::pin(async move {
            let started = Instant::now();
            let result = self.inner.save(key, session).await;
            self.measurements
                .barriers_ns
                .lock()
                .expect("barrier samples lock")
                .push(nanos_u64(started.elapsed().as_nanos()));
            match &result {
                Ok(()) => {
                    self.measurements
                        .save_successes
                        .fetch_add(1, Ordering::Relaxed);
                    if let Ok(bytes) = checkpoint_bytes {
                        self.measurements
                            .checkpoint_bytes
                            .fetch_add(bytes as u64, Ordering::Relaxed);
                        self.measurements
                            .final_checkpoint_bytes
                            .store(bytes as u64, Ordering::Relaxed);
                    }
                }
                Err(_) => {
                    self.measurements
                        .save_failures
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            result
        })
    }

    fn clear<'a>(&'a self, key: &'a rumqttc_v4::SessionStoreKey) -> V4StoreFuture<'a, ()> {
        self.measurements
            .clear_requests
            .fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            let result = self.inner.clear(key).await;
            if result.is_ok() {
                self.measurements
                    .final_checkpoint_bytes
                    .store(0, Ordering::Relaxed);
            }
            result
        })
    }
}

#[derive(Clone, Debug)]
struct MeasuringV5Store {
    inner: rumqttc_store::v5::SessionFileStore,
    measurements: Arc<StoreMeasurements>,
}

impl rumqttc_v5::SessionStore for MeasuringV5Store {
    fn load<'a>(
        &'a self,
        key: &'a rumqttc_v5::SessionStoreKey,
    ) -> V5StoreFuture<'a, Option<rumqttc_v5::PersistedSession>> {
        self.inner.load(key)
    }

    fn save<'a>(
        &'a self,
        key: &'a rumqttc_v5::SessionStoreKey,
        session: &'a rumqttc_v5::PersistedSession,
    ) -> V5StoreFuture<'a, ()> {
        self.measurements
            .save_requests
            .fetch_add(1, Ordering::Relaxed);
        let checkpoint_bytes = session
            .encode()
            .map(|bytes| bytes.len() + envelope::ENVELOPE_OVERHEAD);
        Box::pin(async move {
            let started = Instant::now();
            let result = self.inner.save(key, session).await;
            self.measurements
                .barriers_ns
                .lock()
                .expect("barrier samples lock")
                .push(nanos_u64(started.elapsed().as_nanos()));
            match &result {
                Ok(()) => {
                    self.measurements
                        .save_successes
                        .fetch_add(1, Ordering::Relaxed);
                    if let Ok(bytes) = checkpoint_bytes {
                        self.measurements
                            .checkpoint_bytes
                            .fetch_add(bytes as u64, Ordering::Relaxed);
                        self.measurements
                            .final_checkpoint_bytes
                            .store(bytes as u64, Ordering::Relaxed);
                    }
                }
                Err(_) => {
                    self.measurements
                        .save_failures
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            result
        })
    }

    fn clear<'a>(&'a self, key: &'a rumqttc_v5::SessionStoreKey) -> V5StoreFuture<'a, ()> {
        self.measurements
            .clear_requests
            .fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            let result = self.inner.clear(key).await;
            if result.is_ok() {
                self.measurements
                    .final_checkpoint_bytes
                    .store(0, Ordering::Relaxed);
            }
            result
        })
    }
}

async fn run_mqtt(args: MqttArgs) -> anyhow::Result<()> {
    if args.messages == 0 || args.inflight == 0 || args.qos == 0 {
        bail!("--messages and --inflight must be positive and QoS must be 1 or 2");
    }
    let endpoint = super::parse_endpoint(&args.broker_url)?;
    if !matches!(endpoint.transport, super::TransportKind::Tcp) {
        bail!("persistence MQTT benchmarks currently use plain TCP only");
    }
    let started_at = unix_secs();
    let temporary = tempfile::tempdir()?;
    let measurements = Arc::new(StoreMeasurements::default());
    let client_id = format!(
        "{}-mqtt-persistence",
        run_id(args.common.run_id.as_deref(), args.common.protocol.as_str())
    );
    let (mut latencies, elapsed) = match args.common.protocol {
        Protocol::V4 => {
            run_mqtt_v4(
                &args,
                &endpoint,
                temporary.path(),
                &client_id,
                Arc::clone(&measurements),
            )
            .await?
        }
        Protocol::V5 => {
            run_mqtt_v5(
                &args,
                &endpoint,
                temporary.path(),
                &client_id,
                Arc::clone(&measurements),
            )
            .await?
        }
    };
    latencies.sort_unstable();
    let elapsed_sec = elapsed.as_secs_f64();
    let mut barriers = measurements
        .barriers_ns
        .lock()
        .expect("barrier samples lock")
        .clone();
    barriers.sort_unstable();
    let mut metrics = BTreeMap::new();
    metrics.insert("messages".to_owned(), latencies.len() as f64);
    metrics.insert(
        "throughput_msg_sec".to_owned(),
        if elapsed_sec > 0.0 {
            latencies.len() as f64 / elapsed_sec
        } else {
            0.0
        },
    );
    metrics.insert(
        "latency_p50_ns".to_owned(),
        percentile(&latencies, 50) as f64,
    );
    metrics.insert(
        "latency_p95_ns".to_owned(),
        percentile(&latencies, 95) as f64,
    );
    metrics.insert(
        "latency_p99_ns".to_owned(),
        percentile(&latencies, 99) as f64,
    );
    metrics.insert(
        "save_requests".to_owned(),
        measurements.save_requests.load(Ordering::Relaxed) as f64,
    );
    metrics.insert(
        "save_successes".to_owned(),
        measurements.save_successes.load(Ordering::Relaxed) as f64,
    );
    metrics.insert(
        "save_failures".to_owned(),
        measurements.save_failures.load(Ordering::Relaxed) as f64,
    );
    metrics.insert(
        "clear_requests".to_owned(),
        measurements.clear_requests.load(Ordering::Relaxed) as f64,
    );
    metrics.insert(
        "checkpoint_bytes_submitted".to_owned(),
        measurements.checkpoint_bytes.load(Ordering::Relaxed) as f64,
    );
    metrics.insert(
        "final_checkpoint_bytes".to_owned(),
        measurements.final_checkpoint_bytes.load(Ordering::Relaxed) as f64,
    );
    if barriers.is_empty() {
        for name in [
            "persistence_barrier_p50_ns",
            "persistence_barrier_p95_ns",
            "persistence_barrier_p99_ns",
            "persistence_barrier_max_ns",
        ] {
            metrics.insert(name.to_owned(), 0.0);
        }
    } else {
        metrics.insert(
            "persistence_barrier_p50_ns".to_owned(),
            percentile(&barriers, 50) as f64,
        );
        metrics.insert(
            "persistence_barrier_p95_ns".to_owned(),
            percentile(&barriers, 95) as f64,
        );
        metrics.insert(
            "persistence_barrier_p99_ns".to_owned(),
            percentile(&barriers, 99) as f64,
        );
        metrics.insert(
            "persistence_barrier_max_ns".to_owned(),
            barriers[barriers.len() - 1] as f64,
        );
    }
    let mut samples = BTreeMap::new();
    samples.insert(
        "publish_latency_ns".to_owned(),
        latencies.into_iter().map(|value| value as f64).collect(),
    );
    samples.insert(
        "persistence_barrier_ns".to_owned(),
        barriers.into_iter().map(|value| value as f64).collect(),
    );
    print_output(&BenchOutput {
        schema_version: 1,
        run_id: client_id,
        scenario: format!(
            "mqtt-{}-persistence-{:?}-qos{}",
            args.common.protocol.as_str(),
            args.persistence,
            args.qos
        )
        .to_lowercase(),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "protocol": args.common.protocol,
            "persistence": format!("{:?}", args.persistence).to_lowercase(),
            "broker_url": args.broker_url,
            "broker": "external",
            "transport": "tcp",
            "messages": args.messages,
            "warmup_messages": args.warmup_messages,
            "inflight": args.inflight,
            "payload_size": args.payload_size,
            "qos": args.qos,
            "measurement_note": "checkpoint byte accounting performs one benchmark-only canonical encode before the production adapter encode",
        }),
        metrics,
        samples,
        environment: environment(),
    })
}

async fn run_mqtt_v4(
    args: &MqttArgs,
    endpoint: &super::BrokerEndpoint,
    root: &std::path::Path,
    client_id: &str,
    measurements: Arc<StoreMeasurements>,
) -> anyhow::Result<(Vec<u64>, std::time::Duration)> {
    let mut options =
        rumqttc_v4::MqttOptions::new(client_id, (endpoint.host.clone(), endpoint.port));
    options
        .set_clean_session(false)
        .set_inflight(u16::try_from(args.inflight)?);
    if matches!(args.persistence, PersistenceMode::Enabled) {
        let inner = rumqttc_store::v4::SessionFileStore::open(root).await?;
        options.set_session_store(MeasuringV4Store {
            inner,
            measurements: Arc::clone(&measurements),
        });
    }
    let (client, mut eventloop) = rumqttc_v4::AsyncClient::builder(options)
        .capacity(args.inflight * 2)
        .build();
    let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();
    let eventloop_task: tokio::task::JoinHandle<Result<(), rumqttc_v4::ConnectionError>> =
        tokio::spawn(async move {
            let mut connected_tx = Some(connected_tx);
            loop {
                match eventloop.poll().await {
                    Ok(rumqttc_v4::Event::Incoming(rumqttc_v4::Incoming::ConnAck(_))) => {
                        if let Some(tx) = connected_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                    Ok(_) => {}
                    Err(error) => return Err(error),
                }
            }
        });
    tokio::time::timeout(std::time::Duration::from_secs(10), connected_rx).await??;
    publish_windows_v4(
        &client,
        args.warmup_messages,
        args.inflight,
        args.payload_size,
        args.qos,
    )
    .await?;
    measurements.reset();
    let measured_at = Instant::now();
    let latencies = publish_windows_v4(
        &client,
        args.messages,
        args.inflight,
        args.payload_size,
        args.qos,
    )
    .await?;
    let elapsed = measured_at.elapsed();
    eventloop_task.abort();
    Ok((latencies, elapsed))
}

async fn run_mqtt_v5(
    args: &MqttArgs,
    endpoint: &super::BrokerEndpoint,
    root: &std::path::Path,
    client_id: &str,
    measurements: Arc<StoreMeasurements>,
) -> anyhow::Result<(Vec<u64>, std::time::Duration)> {
    let mut options =
        rumqttc_v5::MqttOptions::new(client_id, (endpoint.host.clone(), endpoint.port));
    options
        .set_clean_start(false)
        .set_session_expiry_interval(Some(3600))
        .set_outgoing_inflight_upper_limit(u16::try_from(args.inflight)?);
    if matches!(args.persistence, PersistenceMode::Enabled) {
        let inner = rumqttc_store::v5::SessionFileStore::open(root).await?;
        options.set_session_store(MeasuringV5Store {
            inner,
            measurements: Arc::clone(&measurements),
        });
    }
    let (client, mut eventloop) = rumqttc_v5::AsyncClient::builder(options)
        .capacity(args.inflight * 2)
        .build();
    let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();
    let eventloop_task: tokio::task::JoinHandle<Result<(), rumqttc_v5::ConnectionError>> =
        tokio::spawn(async move {
            let mut connected_tx = Some(connected_tx);
            loop {
                match eventloop.poll().await {
                    Ok(rumqttc_v5::Event::Incoming(rumqttc_v5::Incoming::ConnAck(_))) => {
                        if let Some(tx) = connected_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                    Ok(_) => {}
                    Err(error) => return Err(error),
                }
            }
        });
    tokio::time::timeout(std::time::Duration::from_secs(10), connected_rx).await??;
    publish_windows_v5(
        &client,
        args.warmup_messages,
        args.inflight,
        args.payload_size,
        args.qos,
    )
    .await?;
    measurements.reset();
    let measured_at = Instant::now();
    let latencies = publish_windows_v5(
        &client,
        args.messages,
        args.inflight,
        args.payload_size,
        args.qos,
    )
    .await?;
    let elapsed = measured_at.elapsed();
    eventloop_task.abort();
    Ok((latencies, elapsed))
}

async fn publish_windows_v4(
    client: &rumqttc_v4::AsyncClient,
    messages: usize,
    inflight: usize,
    payload_size: usize,
    qos: u8,
) -> anyhow::Result<Vec<u64>> {
    let options = rumqttc_v4::PublishOptions::new(super::v4_qos(qos));
    let mut latencies = Vec::with_capacity(messages);
    for start in (0..messages).step_by(inflight) {
        let count = inflight.min(messages - start);
        let mut notices = Vec::with_capacity(count);
        for _ in 0..count {
            let admitted = Instant::now();
            let notice = client
                .publish_tracked("bench/persistence", payload(payload_size), options)
                .await?;
            notices.push((admitted, notice));
        }
        for (admitted, notice) in notices {
            notice.wait_completion_async().await?;
            latencies.push(nanos_u64(admitted.elapsed().as_nanos()));
        }
    }
    Ok(latencies)
}

async fn publish_windows_v5(
    client: &rumqttc_v5::AsyncClient,
    messages: usize,
    inflight: usize,
    payload_size: usize,
    qos: u8,
) -> anyhow::Result<Vec<u64>> {
    let options = rumqttc_v5::PublishOptions::new(super::v5_qos(qos));
    let mut latencies = Vec::with_capacity(messages);
    for start in (0..messages).step_by(inflight) {
        let count = inflight.min(messages - start);
        let mut notices = Vec::with_capacity(count);
        for _ in 0..count {
            let admitted = Instant::now();
            let notice = client
                .publish_tracked("bench/persistence", payload(payload_size), options.clone())
                .await?;
            notices.push((admitted, notice));
        }
        for (admitted, notice) in notices {
            notice.wait_completion_async().await?;
            latencies.push(nanos_u64(admitted.elapsed().as_nanos()));
        }
    }
    Ok(latencies)
}

fn payload(size: usize) -> Vec<u8> {
    (0..size)
        .map(|index| u8::try_from(index % 251).expect("remainder is less than 251"))
        .collect()
}

fn nanos_u64(nanos: u128) -> u64 {
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

fn run_envelope(args: &EnvelopeArgs) -> anyhow::Result<()> {
    validate_samples(args.samples)?;
    if args.operations_per_sample == 0 {
        bail!("--operations-per-sample must be greater than zero");
    }
    let started_at = unix_secs();
    let payload = payload(args.payload_size);
    let encoded = envelope::encode(&payload, DEFAULT_MAX_CHECKPOINT_SIZE)?;
    let mut samples = Vec::with_capacity(args.samples);
    for _ in 0..args.samples {
        let started = Instant::now();
        for _ in 0..args.operations_per_sample {
            match args.mode {
                EnvelopeMode::Encode => {
                    black_box(envelope::encode(
                        black_box(&payload),
                        DEFAULT_MAX_CHECKPOINT_SIZE,
                    )?);
                }
                EnvelopeMode::Decode => {
                    let mut reader = Cursor::new(black_box(encoded.as_slice()));
                    black_box(envelope::decode(&mut reader, DEFAULT_MAX_CHECKPOINT_SIZE)?);
                }
                EnvelopeMode::Crc32c => {
                    black_box(crc32c::crc32c(black_box(&payload)));
                }
                EnvelopeMode::ErrorPaths => {
                    let mut corrupt = encoded.clone();
                    if let Some(byte) = corrupt.get_mut(18) {
                        *byte ^= 1;
                    } else {
                        corrupt.push(1);
                    }
                    let mut reader = Cursor::new(corrupt);
                    black_box(envelope::decode(&mut reader, DEFAULT_MAX_CHECKPOINT_SIZE).is_err());
                }
            }
        }
        let average_nanos = started.elapsed().as_nanos() / args.operations_per_sample as u128;
        samples.push(nanos_u64(average_nanos));
    }
    emit_latency(
        &args.common,
        format!("persistence-envelope-{:?}", args.mode).to_lowercase(),
        started_at,
        json!({
            "mode": format!("{:?}", args.mode).to_lowercase(),
            "payload_size": args.payload_size,
            "envelope_size": encoded.len(),
            "samples": args.samples,
            "operations_per_sample": args.operations_per_sample,
            "page_cache": "not-applicable",
            "synchronization_included": false,
        }),
        "latency_ns",
        samples,
        args.payload_size,
    )
}

fn run_codec(args: &CodecArgs) -> anyhow::Result<()> {
    validate_samples(args.samples)?;
    if args.qos == 0 {
        bail!("persistent inflight fixtures require QoS 1 or QoS 2");
    }
    let started_at = unix_secs();
    let mut samples = Vec::with_capacity(args.samples);
    let canonical_size = match args.common.protocol {
        Protocol::V4 => {
            let fixture = fixture_v4(args.inflight, args.payload_size, args.qos);
            let encoded = fixture.encode()?;
            for _ in 0..args.samples {
                let started = Instant::now();
                match args.mode {
                    CodecMode::Encode => drop(black_box(fixture.encode()?)),
                    CodecMode::Decode => {
                        drop(black_box(rumqttc_v4::PersistedSession::decode(&encoded)?));
                    }
                }
                samples.push(nanos_u64(started.elapsed().as_nanos()));
            }
            encoded.len()
        }
        Protocol::V5 => {
            let fixture = fixture_v5(args.inflight, args.payload_size, args.qos);
            let encoded = fixture.encode()?;
            for _ in 0..args.samples {
                let started = Instant::now();
                match args.mode {
                    CodecMode::Encode => drop(black_box(fixture.encode()?)),
                    CodecMode::Decode => {
                        drop(black_box(rumqttc_v5::PersistedSession::decode(&encoded)?));
                    }
                }
                samples.push(nanos_u64(started.elapsed().as_nanos()));
            }
            encoded.len()
        }
    };
    emit_latency(
        &args.common,
        format!("persistence-codec-{:?}", args.mode).to_lowercase(),
        started_at,
        json!({
            "mode": format!("{:?}", args.mode).to_lowercase(),
            "inflight": args.inflight,
            "payload_size": args.payload_size,
            "qos": args.qos,
            "canonical_size": canonical_size,
            "samples": args.samples,
            "synchronization_included": false,
        }),
        "latency_ns",
        samples,
        canonical_size,
    )
}

async fn run_file_store(args: FileStoreArgs) -> anyhow::Result<()> {
    validate_samples(args.samples)?;
    let started_at = unix_secs();
    let temporary = tempfile::tempdir()?;
    let store = FileStore::open(temporary.path(), "benchmark", FileStoreOptions::default()).await?;
    let body = payload(args.payload_size);
    let mut samples = Vec::with_capacity(args.samples);
    for sample in 0..args.samples {
        let key = format!("key-{sample}");
        match args.operation {
            StoreOperation::SaveReplace
            | StoreOperation::SaveGrowing
            | StoreOperation::SaveShrinking
            | StoreOperation::LoadPresent
            | StoreOperation::ClearPresent
            | StoreOperation::InspectPresent
            | StoreOperation::QuarantinePresent => {
                let setup = match args.operation {
                    StoreOperation::SaveGrowing => payload((args.payload_size / 2).max(1)),
                    StoreOperation::SaveShrinking => payload(args.payload_size.saturating_mul(2)),
                    _ => body.clone(),
                };
                store.save(key.as_bytes(), setup).await?;
            }
            _ => {}
        }
        let started = Instant::now();
        match args.operation {
            StoreOperation::SaveCreate
            | StoreOperation::SaveReplace
            | StoreOperation::SaveGrowing
            | StoreOperation::SaveShrinking => {
                store.save(key.as_bytes(), body.clone()).await?;
            }
            StoreOperation::LoadPresent | StoreOperation::LoadMissing => {
                black_box(store.load(key.as_bytes()).await?);
            }
            StoreOperation::ClearPresent | StoreOperation::ClearMissing => {
                store.clear(key.as_bytes()).await?;
            }
            StoreOperation::InspectPresent | StoreOperation::InspectMissing => {
                black_box(store.inspect(key.as_bytes()).await?);
            }
            StoreOperation::QuarantinePresent => {
                black_box(store.quarantine(key.as_bytes()).await?);
            }
            StoreOperation::QuarantineMissing => {
                black_box(store.quarantine(key.as_bytes()).await.is_err());
            }
        }
        samples.push(nanos_u64(started.elapsed().as_nanos()));
    }
    emit_latency(
        &args.common,
        format!("persistence-file-store-{:?}", args.operation).to_lowercase(),
        started_at,
        json!({
            "operation": format!("{:?}", args.operation).to_lowercase(),
            "payload_size": args.payload_size,
            "checkpoint_size": args.payload_size + envelope::ENVELOPE_OVERHEAD,
            "samples": args.samples,
            "page_cache": "warm-or-new-per-operation",
            "synchronization_included": true,
            "backend": if cfg!(unix) { "atomic-write-file" } else { "windows-native" },
        }),
        "latency_ns",
        samples,
        args.payload_size + envelope::ENVELOPE_OVERHEAD,
    )
}

async fn run_coordination(args: CoordinationArgs) -> anyhow::Result<()> {
    if args.concurrency == 0 || args.operations == 0 {
        bail!("--concurrency and --operations must be greater than zero");
    }
    let started_at = unix_secs();
    let temporary = tempfile::tempdir()?;
    let store = FileStore::open(temporary.path(), "coord", FileStoreOptions::default()).await?;
    let wall = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for worker in 0..args.concurrency {
        let store = store.clone();
        let different_keys = args.different_keys;
        let operations = args.operations;
        tasks.spawn(async move {
            let mut latencies = Vec::with_capacity(operations);
            for operation in 0..operations {
                let key = if different_keys {
                    format!("worker-{worker}-operation-{operation}")
                } else {
                    "shared-key".to_owned()
                };
                let started = Instant::now();
                black_box(store.inspect(key.as_bytes()).await?);
                latencies.push(nanos_u64(started.elapsed().as_nanos()));
            }
            Ok::<_, rumqttc_store_core::FileStoreError>(latencies)
        });
    }
    let mut samples = Vec::new();
    while let Some(result) = tasks.join_next().await {
        samples.extend(result??);
    }
    let elapsed = wall.elapsed().as_secs_f64();
    let total = samples.len();
    let mut extra = BTreeMap::new();
    extra.insert("operations_sec".to_owned(), total as f64 / elapsed);
    emit_latency_with_metrics(
        &args.common,
        "persistence-coordination".to_owned(),
        started_at,
        json!({
            "concurrency": args.concurrency,
            "operations_per_worker": args.operations,
            "different_keys": args.different_keys,
            "operation": "inspect-missing",
            "latency_scope": "submission-to-completion",
        }),
        "latency_ns",
        samples,
        0,
        extra,
    )
}

fn run_growth(args: &GrowthArgs) -> anyhow::Result<()> {
    if args.qos == 0 {
        bail!("growth fixtures require QoS 1 or QoS 2");
    }
    let started_at = unix_secs();
    let mut rows = Vec::new();
    let mut canonical_sizes = Vec::new();
    for count in COUNTS {
        let canonical = match args.common.protocol {
            Protocol::V4 => fixture_v4(count, args.payload_size, args.qos)
                .encode()?
                .len(),
            Protocol::V5 => fixture_v5(count, args.payload_size, args.qos)
                .encode()?
                .len(),
        };
        canonical_sizes.push(canonical as f64);
        rows.push(json!({
            "inflight": count,
            "qos": args.qos,
            "application_payload_bytes": count.saturating_mul(args.payload_size),
            "canonical_session_bytes": canonical,
            "envelope_bytes": envelope::ENVELOPE_OVERHEAD,
            "total_checkpoint_bytes": canonical + envelope::ENVELOPE_OVERHEAD,
        }));
    }
    let mut metrics = BTreeMap::new();
    metrics.insert("rows".to_owned(), rows.len() as f64);
    metrics.insert("minimal_canonical_bytes".to_owned(), canonical_sizes[0]);
    metrics.insert(
        "thousand_inflight_checkpoint_bytes".to_owned(),
        canonical_sizes[4] + envelope::ENVELOPE_OVERHEAD as f64,
    );
    let mut samples = BTreeMap::new();
    samples.insert("canonical_size_bytes".to_owned(), canonical_sizes);
    print_output(&BenchOutput {
        schema_version: 1,
        run_id: run_id(args.common.run_id.as_deref(), "persistence-growth"),
        scenario: "persistence-checkpoint-growth".to_owned(),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "protocol": args.common.protocol,
            "payload_size": args.payload_size,
            "qos": args.qos,
            "rows": rows,
        }),
        metrics,
        samples,
        environment: environment(),
    })
}

fn fixture_v4(count: usize, payload_size: usize, qos: u8) -> rumqttc_v4::PersistedSession {
    use rumqttc_v4::{PersistedAckMode, PersistedPublish, PersistedQoS, PersistedRequest};
    let qos = if qos == 1 {
        PersistedQoS::AtLeastOnce
    } else {
        PersistedQoS::ExactlyOnce
    };
    let replay = (0..count)
        .map(|index| {
            PersistedRequest::Publish(PersistedPublish {
                dup: index % 2 == 0,
                qos,
                retain: false,
                topic: b"bench/persistence".to_vec(),
                pkid: u16::try_from(index + 1).expect("fixture count fits u16"),
                payload: payload(payload_size),
            })
        })
        .collect();
    rumqttc_v4::PersistedSession {
        format_version: 1,
        client_id: "benchmark-client-v4".to_owned(),
        clean_session: false,
        max_inflight: u16::try_from(count.max(1)).expect("fixture count fits u16"),
        ack_mode: PersistedAckMode::Automatic,
        last_pkid: u16::try_from(count).expect("fixture count fits u16"),
        last_puback: 0,
        replay,
        incoming_qos2: Vec::new(),
    }
}

fn fixture_v5(count: usize, payload_size: usize, qos: u8) -> rumqttc_v5::PersistedSession {
    use rumqttc_v5::{PersistedAckMode, PersistedPublish, PersistedQoS, PersistedRequest};
    let qos = if qos == 1 {
        PersistedQoS::AtLeastOnce
    } else {
        PersistedQoS::ExactlyOnce
    };
    let replay = (0..count)
        .map(|index| {
            PersistedRequest::Publish(PersistedPublish {
                dup: index % 2 == 0,
                qos,
                retain: false,
                topic: b"bench/persistence".to_vec(),
                pkid: u16::try_from(index + 1).expect("fixture count fits u16"),
                payload: payload(payload_size),
                properties: None,
            })
        })
        .collect();
    rumqttc_v5::PersistedSession {
        format_version: 1,
        client_id: "benchmark-client-v5".to_owned(),
        clean_start: false,
        session_expiry_interval: Some(3600),
        outgoing_inflight_upper_limit: Some(
            u16::try_from(count.max(1)).expect("fixture count fits u16"),
        ),
        ack_mode: PersistedAckMode::Automatic,
        last_pkid: u16::try_from(count).expect("fixture count fits u16"),
        last_puback: 0,
        replay,
        incoming_qos2: Vec::new(),
    }
}

fn validate_samples(samples: usize) -> anyhow::Result<()> {
    if samples == 0 {
        bail!("--samples must be greater than zero");
    }
    Ok(())
}

fn emit_latency(
    common: &CommonArgs,
    scenario: String,
    started_at: u64,
    config: serde_json::Value,
    sample_name: &str,
    samples: Vec<u64>,
    bytes: usize,
) -> anyhow::Result<()> {
    emit_latency_with_metrics(
        common,
        scenario,
        started_at,
        config,
        sample_name,
        samples,
        bytes,
        BTreeMap::new(),
    )
}

#[allow(clippy::too_many_arguments)]
fn emit_latency_with_metrics(
    common: &CommonArgs,
    scenario: String,
    started_at: u64,
    config: serde_json::Value,
    sample_name: &str,
    mut samples: Vec<u64>,
    bytes: usize,
    mut metrics: BTreeMap<String, f64>,
) -> anyhow::Result<()> {
    samples.sort_unstable();
    let count = samples.len();
    let total: u128 = samples.iter().map(|value| u128::from(*value)).sum();
    let mean = total as f64 / count as f64;
    metrics.insert("samples".to_owned(), count as f64);
    metrics.insert("latency_mean_ns".to_owned(), mean);
    metrics.insert("latency_p50_ns".to_owned(), percentile(&samples, 50) as f64);
    metrics.insert("latency_p95_ns".to_owned(), percentile(&samples, 95) as f64);
    metrics.insert("latency_p99_ns".to_owned(), percentile(&samples, 99) as f64);
    metrics.insert("latency_max_ns".to_owned(), samples[count - 1] as f64);
    if bytes > 0 && mean > 0.0 {
        metrics.insert(
            "bytes_sec".to_owned(),
            bytes as f64 / (mean / 1_000_000_000.0),
        );
    }
    let mut output_samples = BTreeMap::new();
    output_samples.insert(
        sample_name.to_owned(),
        samples.into_iter().map(|value| value as f64).collect(),
    );
    print_output(&BenchOutput {
        schema_version: 1,
        run_id: run_id(common.run_id.as_deref(), &scenario),
        scenario,
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config,
        metrics,
        samples: output_samples,
        environment: environment(),
    })
    .context("failed to emit persistence benchmark output")
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let index = (sorted.len() - 1) * percentile / 100;
    sorted[index]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_round_trip_and_have_requested_inflight_count() {
        for count in COUNTS {
            let v4 = fixture_v4(count, 1024, 1);
            let v4_bytes = v4.encode().unwrap();
            assert_eq!(rumqttc_v4::PersistedSession::decode(&v4_bytes).unwrap(), v4);
            assert_eq!(v4.replay.len(), count);

            let v5 = fixture_v5(count, 1024, 2);
            let v5_bytes = v5.encode().unwrap();
            assert_eq!(rumqttc_v5::PersistedSession::decode(&v5_bytes).unwrap(), v5);
            assert_eq!(v5.replay.len(), count);
        }
    }

    #[test]
    fn envelope_helper_uses_stable_production_bytes() {
        let actual = envelope::encode(b"abc", 1024).unwrap();
        assert_eq!(&actual[..18], b"RUMQSESS\0\x01\0\0\0\0\0\0\0\x03");
        assert_eq!(
            envelope::decode(&mut Cursor::new(actual), 1024).unwrap(),
            b"abc"
        );
    }
}
