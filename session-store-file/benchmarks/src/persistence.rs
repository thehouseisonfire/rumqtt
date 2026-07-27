use std::collections::BTreeMap;
use std::future::Future;
use std::hint::black_box;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::Instant;

use anyhow::{Context, bail};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::json;

use atomic_blob_store::bench_instrumentation as envelope;
use atomic_blob_store::{
    AtomicBlobStoreError, AtomicBlobStoreOptions, BlobFormatIdentity, BlockingAtomicBlobStore,
    DEFAULT_MAX_BLOB_SIZE, ENVELOPE_VERSION_V1, tokio::AtomicBlobStore,
};

use super::{
    BenchOutput, CommonArgs, Protocol, allocation_counters, environment, print_output,
    reset_allocation_counters, run_id, unix_secs,
};

const COUNTS: [usize; 5] = [0, 1, 10, 100, 1_000];

fn benchmark_format() -> BlobFormatIdentity {
    BlobFormatIdentity::new(b"BLOBBNCH", ".bench", ENVELOPE_VERSION_V1).unwrap()
}

fn benchmark_options() -> AtomicBlobStoreOptions {
    AtomicBlobStoreOptions::new(benchmark_format())
}

#[derive(Subcommand, Debug)]
pub enum PersistenceCommand {
    Envelope(EnvelopeArgs),
    Codec(CodecArgs),
    FileStore(FileStoreArgs),
    Coordination(CoordinationArgs),
    Lifecycle(LifecycleArgs),
    Maintenance(MaintenanceArgs),
    Backpressure(BackpressureArgs),
    Recovery(RecoveryArgs),
    Growth(GrowthArgs),
    Mqtt(MqttArgs),
}

#[derive(Args, Debug)]
pub struct MaintenanceArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, default_value = "1048576")]
    payload_size: usize,
    #[arg(long, default_value = "4")]
    max_concurrency: usize,
    #[arg(long, default_value = "50")]
    samples: usize,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BackpressureDirection {
    Source,
    Destination,
}

#[derive(Args, Debug)]
pub struct BackpressureArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, value_enum)]
    direction: BackpressureDirection,
    #[arg(long, default_value = "1048576")]
    payload_size: usize,
    #[arg(long, default_value = "4")]
    max_concurrency: usize,
    #[arg(long, default_value = "50")]
    samples: usize,
}

#[derive(Args, Debug)]
pub struct RecoveryArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, default_value = "100")]
    inflight: u16,
    #[arg(long, default_value = "1024")]
    payload_size: usize,
    #[arg(long, default_value = "1", value_parser = super::parse_qos)]
    qos: u8,
    #[arg(long, default_value = "50")]
    samples: usize,
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BlobIoMode {
    Streaming,
    Complete,
}

#[derive(Args, Debug)]
pub struct FileStoreArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, value_enum, default_value = "save-replace")]
    operation: StoreOperation,
    #[arg(long, value_enum, default_value = "streaming")]
    io_mode: BlobIoMode,
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
pub struct LifecycleArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long, default_value = "4")]
    max_concurrency: usize,
    #[arg(long, default_value = "1")]
    stores: usize,
    #[arg(long, default_value = "1048576")]
    payload_size: usize,
    #[arg(long, default_value = "30")]
    samples: usize,
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
        PersistenceCommand::Lifecycle(args) => run_lifecycle(args),
        PersistenceCommand::Maintenance(args) => run_maintenance(args),
        PersistenceCommand::Backpressure(args) => run_backpressure(args).await,
        PersistenceCommand::Recovery(args) => run_recovery(args).await,
        PersistenceCommand::Growth(args) => run_growth(&args),
        PersistenceCommand::Mqtt(args) => run_mqtt(args).await,
    }
}

#[cfg(target_os = "linux")]
fn process_threads() -> anyhow::Result<usize> {
    Ok(std::fs::read_dir("/proc/self/task")?.count())
}

#[cfg(not(target_os = "linux"))]
fn process_threads() -> anyhow::Result<usize> {
    bail!("thread-count measurement is currently implemented only for Linux")
}

#[cfg(target_os = "linux")]
fn peak_rss_bytes() -> anyhow::Result<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .context("VmHWM is absent from /proc/self/status")?
        .parse::<u64>()?;
    Ok(value * 1024)
}

#[cfg(not(target_os = "linux"))]
fn peak_rss_bytes() -> anyhow::Result<u64> {
    bail!("peak-RSS measurement is currently implemented only for Linux")
}

fn run_lifecycle(args: LifecycleArgs) -> anyhow::Result<()> {
    validate_samples(args.samples)?;
    if args.stores == 0 || args.max_concurrency == 0 {
        bail!("--stores and --max-concurrency must be greater than zero");
    }
    let started_at = unix_secs();
    let baseline_threads = process_threads()?;
    let baseline_rss = peak_rss_bytes()?;
    let options = benchmark_options().with_max_concurrent_operations(
        NonZeroUsize::new(args.max_concurrency).context("nonzero concurrency")?,
    );
    let mut open_samples = Vec::with_capacity(args.samples);
    let mut cold_save_samples = Vec::with_capacity(args.samples);
    let mut flush_samples = Vec::with_capacity(args.samples);
    let mut close_samples = Vec::with_capacity(args.samples);
    let mut idle_thread_deltas = Vec::with_capacity(args.samples);
    let mut active_thread_deltas = Vec::with_capacity(args.samples);
    let mut allocation_counts = Vec::with_capacity(args.samples);
    let mut allocated_bytes = Vec::with_capacity(args.samples);
    let body = payload(args.payload_size);

    for sample in 0..args.samples {
        let temporary = tempfile::tempdir()?;
        let mut stores = Vec::with_capacity(args.stores);
        let opened = Instant::now();
        for index in 0..args.stores {
            stores.push(BlockingAtomicBlobStore::open(
                temporary.path(),
                format!("lifecycle-{sample}-{index}"),
                options.clone(),
            )?);
        }
        open_samples.push(nanos_u64(opened.elapsed().as_nanos()));
        idle_thread_deltas.push(process_threads()?.saturating_sub(baseline_threads) as f64);

        reset_allocation_counters();
        let saving = Instant::now();
        std::thread::scope(|scope| -> Result<(), AtomicBlobStoreError> {
            let mut handles = Vec::with_capacity(stores.len() * args.max_concurrency);
            for (store_index, store) in stores.iter().enumerate() {
                for worker_index in 0..args.max_concurrency {
                    let body = &body;
                    handles.push(scope.spawn(move || {
                        store.save(
                            format!("key-{store_index}-{worker_index}").as_bytes(),
                            body.clone(),
                        )
                    }));
                }
            }
            for handle in handles {
                handle.join().expect("benchmark save thread panicked")?;
            }
            Ok(())
        })?;
        cold_save_samples.push(nanos_u64(saving.elapsed().as_nanos()));
        let (count, bytes) = allocation_counters();
        allocation_counts.push(count as f64);
        allocated_bytes.push(bytes as f64);
        active_thread_deltas.push(process_threads()?.saturating_sub(baseline_threads) as f64);

        let flushing = Instant::now();
        for store in &stores {
            store.flush()?;
        }
        flush_samples.push(nanos_u64(flushing.elapsed().as_nanos()));

        let closing = Instant::now();
        for store in &stores {
            store.close()?;
        }
        close_samples.push(nanos_u64(closing.elapsed().as_nanos()));
    }

    let mut metrics = BTreeMap::new();
    metrics.insert("baseline_threads".to_owned(), baseline_threads as f64);
    metrics.insert("baseline_peak_rss_bytes".to_owned(), baseline_rss as f64);
    metrics.insert("peak_rss_bytes".to_owned(), peak_rss_bytes()? as f64);
    metrics.insert(
        "idle_thread_delta_max".to_owned(),
        idle_thread_deltas.iter().copied().fold(0.0, f64::max),
    );
    metrics.insert(
        "active_thread_delta_max".to_owned(),
        active_thread_deltas.iter().copied().fold(0.0, f64::max),
    );
    let mut samples = BTreeMap::new();
    samples.insert(
        "open_latency_ns".to_owned(),
        open_samples.into_iter().map(|value| value as f64).collect(),
    );
    samples.insert(
        "cold_save_latency_ns".to_owned(),
        cold_save_samples
            .into_iter()
            .map(|value| value as f64)
            .collect(),
    );
    samples.insert(
        "flush_latency_ns".to_owned(),
        flush_samples
            .into_iter()
            .map(|value| value as f64)
            .collect(),
    );
    samples.insert(
        "close_latency_ns".to_owned(),
        close_samples
            .into_iter()
            .map(|value| value as f64)
            .collect(),
    );
    samples.insert("idle_thread_delta".to_owned(), idle_thread_deltas);
    samples.insert("active_thread_delta".to_owned(), active_thread_deltas);
    samples.insert("allocation_count".to_owned(), allocation_counts);
    samples.insert("allocated_bytes".to_owned(), allocated_bytes);
    print_output(&BenchOutput {
        schema_version: 1,
        run_id: run_id(args.common.run_id.as_deref(), "persistence-lifecycle"),
        scenario: "persistence-blob-store-lifecycle".to_owned(),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "stores": args.stores,
            "max_concurrency": args.max_concurrency,
            "payload_size": args.payload_size,
            "samples": args.samples,
            "thread_measurement": "/proc/self/task delta",
            "rss_measurement": "/proc/self/status VmHWM",
            "allocation_measurement": "process-global counting allocator; measured save window",
        }),
        metrics,
        samples,
        environment: environment(),
    })
}

struct BlockingGateReader {
    payload: Cursor<Vec<u8>>,
    reached: Option<std::sync::mpsc::SyncSender<()>>,
    release: std::sync::mpsc::Receiver<()>,
}

impl std::io::Read for BlockingGateReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if let Some(reached) = self.reached.take() {
            reached
                .send(())
                .expect("maintenance observer remains alive");
            self.release
                .recv()
                .expect("maintenance benchmark releases the source");
        }
        std::io::Read::read(&mut self.payload, output)
    }
}

fn receive_benchmark_event(
    receiver: &std::sync::mpsc::Receiver<envelope::BenchmarkEvent>,
    expected: envelope::BenchmarkEvent,
) -> anyhow::Result<()> {
    loop {
        let event = receiver
            .recv_timeout(std::time::Duration::from_secs(30))
            .with_context(|| format!("timed out waiting for benchmark event {expected:?}"))?;
        if event == expected {
            return Ok(());
        }
    }
}

async fn wait_for_benchmark_event(
    receiver: Arc<Mutex<std::sync::mpsc::Receiver<envelope::BenchmarkEvent>>>,
    expected: envelope::BenchmarkEvent,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || {
        let receiver = receiver
            .lock()
            .map_err(|_| anyhow::anyhow!("benchmark event receiver lock was poisoned"))?;
        receive_benchmark_event(&receiver, expected)
    })
    .await
    .context("benchmark event observer task failed")?
}

fn drain_benchmark_events(
    receiver: &Arc<Mutex<std::sync::mpsc::Receiver<envelope::BenchmarkEvent>>>,
) -> anyhow::Result<()> {
    let receiver = receiver
        .lock()
        .map_err(|_| anyhow::anyhow!("benchmark event receiver lock was poisoned"))?;
    loop {
        match receiver.try_recv() {
            Ok(_) => {}
            Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(()),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                bail!("benchmark event channel disconnected")
            }
        }
    }
}

fn run_maintenance(args: MaintenanceArgs) -> anyhow::Result<()> {
    validate_samples(args.samples)?;
    let maximum = NonZeroUsize::new(args.max_concurrency)
        .context("--max-concurrency must be greater than zero")?;
    let started_at = unix_secs();
    let temporary = tempfile::tempdir()?;
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let store = BlockingAtomicBlobStore::open_with_benchmark_events(
        temporary.path(),
        "maintenance",
        benchmark_options().with_max_concurrent_operations(maximum),
        event_sender,
    )?;
    let mut idle = Vec::with_capacity(args.samples);
    let mut ordered = Vec::with_capacity(args.samples);
    for sample in 0..args.samples {
        let started = Instant::now();
        store.flush()?;
        receive_benchmark_event(&event_receiver, envelope::BenchmarkEvent::FlushAccepted)?;
        idle.push(nanos_u64(started.elapsed().as_nanos()));

        let (reached_sender, reached_receiver) = std::sync::mpsc::sync_channel(1);
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
        let payload = payload(args.payload_size);
        let save_store = store.clone();
        let save = std::thread::spawn(move || {
            let mut source = BlockingGateReader {
                payload: Cursor::new(payload),
                reached: Some(reached_sender),
                release: release_receiver,
            };
            save_store.save_from(
                format!("queued-{sample}").as_bytes(),
                &mut source,
                args.payload_size as u64,
            )
        });
        reached_receiver.recv()?;
        let flush_store = store.clone();
        let (flush_sender, flush_receiver) = std::sync::mpsc::sync_channel(1);
        let started = Instant::now();
        let flush = std::thread::spawn(move || flush_sender.send(flush_store.flush()).unwrap());
        receive_benchmark_event(&event_receiver, envelope::BenchmarkEvent::FlushAccepted)?;
        assert!(matches!(
            flush_receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        release_sender.send(())?;
        flush_receiver.recv()??;
        ordered.push(nanos_u64(started.elapsed().as_nanos()));
        save.join().expect("save benchmark thread panicked")?;
        flush.join().expect("flush benchmark thread panicked");
    }
    store.close()?;
    let mut metrics = BTreeMap::new();
    insert_distribution_metrics(&mut metrics, "idle_barrier", &idle);
    insert_distribution_metrics(&mut metrics, "ordered_barrier", &ordered);
    let mut samples = BTreeMap::new();
    samples.insert(
        "idle_barrier_latency_ns".to_owned(),
        idle.into_iter().map(|value| value as f64).collect(),
    );
    samples.insert(
        "ordered_barrier_latency_ns".to_owned(),
        ordered.into_iter().map(|value| value as f64).collect(),
    );
    print_output(&BenchOutput {
        schema_version: 1,
        run_id: run_id(args.common.run_id.as_deref(), "persistence-maintenance"),
        scenario: "persistence-maintenance-barrier".to_owned(),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "payload_size": args.payload_size,
            "worker_bound": args.max_concurrency,
            "samples": args.samples,
            "barrier": "flush",
            "ordered_work": "accepted streaming save with deterministically gated source",
            "ordering_observation": "coordinator FlushAccepted event before source release",
        }),
        metrics,
        samples,
        environment: environment(),
    })
}

struct GatedAsyncReader {
    bytes: Cursor<Vec<u8>>,
    release: tokio::sync::oneshot::Receiver<()>,
    released: bool,
}

impl tokio::io::AsyncRead for GatedAsyncReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        output: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.released {
            if Pin::new(&mut self.release).poll(context).is_pending() {
                return Poll::Pending;
            }
            self.released = true;
        }
        let mut buffer = vec![0; output.remaining()];
        let count = std::io::Read::read(&mut self.bytes, &mut buffer)?;
        output.put_slice(&buffer[..count]);
        Poll::Ready(Ok(()))
    }
}

struct GatedAsyncWriter {
    release: tokio::sync::oneshot::Receiver<()>,
    released: bool,
    bytes: usize,
}

impl tokio::io::AsyncWrite for GatedAsyncWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if !self.released {
            if Pin::new(&mut self.release).poll(context).is_pending() {
                return Poll::Pending;
            }
            self.released = true;
        }
        self.bytes += bytes.len();
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _context: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _context: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

async fn run_backpressure(args: BackpressureArgs) -> anyhow::Result<()> {
    validate_samples(args.samples)?;
    let maximum = NonZeroUsize::new(args.max_concurrency)
        .context("--max-concurrency must be greater than zero")?;
    let minimum = envelope::STREAM_CHUNK_BYTES * (envelope::STREAM_CHANNEL_CAPACITY + 2);
    if args.payload_size < minimum {
        bail!("--payload-size must be at least {minimum} bytes");
    }
    let started_at = unix_secs();
    let temporary = tempfile::tempdir()?;
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let event_receiver = Arc::new(Mutex::new(event_receiver));
    let store = AtomicBlobStore::open_with_benchmark_events(
        temporary.path(),
        "backpressure",
        benchmark_options().with_max_concurrent_operations(maximum),
        event_sender,
    )
    .await?;
    let mut established = Vec::with_capacity(args.samples);
    let mut completion = Vec::with_capacity(args.samples);
    for sample in 0..args.samples {
        let key = format!("key-{sample}");
        let body = payload(args.payload_size);
        let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
        match args.direction {
            BackpressureDirection::Source => {
                let mut source = GatedAsyncReader {
                    bytes: Cursor::new(body),
                    release: release_receiver,
                    released: false,
                };
                drain_benchmark_events(&event_receiver)?;
                let started = Instant::now();
                let operation =
                    store.save_from(key.as_bytes(), &mut source, args.payload_size as u64);
                tokio::pin!(operation);
                let backpressure = wait_for_benchmark_event(
                    Arc::clone(&event_receiver),
                    envelope::BenchmarkEvent::SaveStreamInputStarved,
                );
                tokio::select! {
                    result = &mut operation => result?,
                    result = backpressure => result?,
                }
                established.push(nanos_u64(started.elapsed().as_nanos()));
                let released = Instant::now();
                let _ = release_sender.send(());
                operation.await?;
                completion.push(nanos_u64(released.elapsed().as_nanos()));
            }
            BackpressureDirection::Destination => {
                store.save(key.as_bytes(), body).await?;
                let mut destination = GatedAsyncWriter {
                    release: release_receiver,
                    released: false,
                    bytes: 0,
                };
                drain_benchmark_events(&event_receiver)?;
                let started = Instant::now();
                let completion_latency = {
                    let operation = store.load_into(key.as_bytes(), &mut destination);
                    tokio::pin!(operation);
                    let backpressure = wait_for_benchmark_event(
                        Arc::clone(&event_receiver),
                        envelope::BenchmarkEvent::LoadStreamOutputBackpressured,
                    );
                    tokio::select! {
                        result = &mut operation => { result?; },
                        result = backpressure => result?,
                    }
                    established.push(nanos_u64(started.elapsed().as_nanos()));
                    let released = Instant::now();
                    let _ = release_sender.send(());
                    operation.as_mut().await?;
                    nanos_u64(released.elapsed().as_nanos())
                };
                completion.push(completion_latency);
                assert_eq!(destination.bytes, args.payload_size);
            }
        }
    }
    store.close().await?;
    let mut metrics = BTreeMap::new();
    insert_distribution_metrics(&mut metrics, "backpressure_established", &established);
    insert_distribution_metrics(&mut metrics, "completion_after_release", &completion);
    let mut samples = BTreeMap::new();
    samples.insert(
        "backpressure_established_ns".to_owned(),
        established.into_iter().map(|value| value as f64).collect(),
    );
    samples.insert(
        "completion_after_release_ns".to_owned(),
        completion.into_iter().map(|value| value as f64).collect(),
    );
    print_output(&BenchOutput {
        schema_version: 1,
        run_id: run_id(args.common.run_id.as_deref(), "persistence-backpressure"),
        scenario: "persistence-streaming-backpressure".to_owned(),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "direction": format!("{:?}", args.direction).to_lowercase(),
            "payload_size": args.payload_size,
            "chunk_size": envelope::STREAM_CHUNK_BYTES,
            "channel_capacity": envelope::STREAM_CHANNEL_CAPACITY,
            "worker_bound": args.max_concurrency,
            "samples": args.samples,
            "coordination": "endpoint remains Pending until the worker reports an empty input or full output channel, then an explicit oneshot release",
            "event_scope": "first pressure event per completed stream; queued events drained before each timed operation",
            "fixture_scope": "destination fixture creation excluded from backpressure establishment",
        }),
        metrics,
        samples,
        environment: environment(),
    })
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
    let format = benchmark_format();
    let encoded = envelope::encode(&format, &payload, DEFAULT_MAX_BLOB_SIZE)?;
    let mut samples = Vec::with_capacity(args.samples);
    for _ in 0..args.samples {
        let started = Instant::now();
        for _ in 0..args.operations_per_sample {
            match args.mode {
                EnvelopeMode::Encode => {
                    black_box(envelope::encode(
                        &format,
                        black_box(&payload),
                        DEFAULT_MAX_BLOB_SIZE,
                    )?);
                }
                EnvelopeMode::Decode => {
                    let mut reader = Cursor::new(black_box(encoded.as_slice()));
                    black_box(envelope::decode(
                        &format,
                        &mut reader,
                        DEFAULT_MAX_BLOB_SIZE,
                    )?);
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
                    black_box(
                        envelope::decode(&format, &mut reader, DEFAULT_MAX_BLOB_SIZE).is_err(),
                    );
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
    let store = AtomicBlobStore::open(temporary.path(), "benchmark", benchmark_options()).await?;
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
                match args.io_mode {
                    BlobIoMode::Streaming => {
                        let setup_len = u64::try_from(setup.len())?;
                        store
                            .save_from(key.as_bytes(), &mut Cursor::new(setup), setup_len)
                            .await?;
                    }
                    BlobIoMode::Complete => store.save(key.as_bytes(), setup).await?,
                }
            }
            _ => {}
        }
        let started = Instant::now();
        match args.operation {
            StoreOperation::SaveCreate
            | StoreOperation::SaveReplace
            | StoreOperation::SaveGrowing
            | StoreOperation::SaveShrinking => match args.io_mode {
                BlobIoMode::Streaming => {
                    store
                        .save_from(
                            key.as_bytes(),
                            &mut Cursor::new(body.as_slice()),
                            u64::try_from(body.len())?,
                        )
                        .await?;
                }
                BlobIoMode::Complete => {
                    store.save(key.as_bytes(), body.clone()).await?;
                }
            },
            StoreOperation::LoadPresent | StoreOperation::LoadMissing => match args.io_mode {
                BlobIoMode::Streaming => {
                    let mut output = Vec::new();
                    black_box(store.load_into(key.as_bytes(), &mut output).await?);
                    black_box(output);
                }
                BlobIoMode::Complete => {
                    black_box(store.load(key.as_bytes()).await?);
                }
            },
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
            "payload_io": format!("{:?}", args.io_mode).to_lowercase(),
            "load_validation": match args.io_mode {
                BlobIoMode::Streaming => "complete-first-pass-then-stream",
                BlobIoMode::Complete => "single-pass-into-complete-allocation",
            },
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
    let store = AtomicBlobStore::open(temporary.path(), "coord", benchmark_options()).await?;
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
            Ok::<_, AtomicBlobStoreError>(latencies)
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

async fn run_recovery(args: RecoveryArgs) -> anyhow::Result<()> {
    validate_samples(args.samples)?;
    if args.qos == 0 || args.inflight == 0 {
        bail!("recovery fixtures require nonzero inflight QoS 1 or QoS 2 state");
    }
    let started_at = unix_secs();
    let temporary = tempfile::tempdir()?;
    let mut load_samples = Vec::with_capacity(args.samples);
    let mut apply_samples = Vec::with_capacity(args.samples);
    let checkpoint_size;
    let replay_count;
    match args.common.protocol {
        Protocol::V4 => {
            use rumqttc_store::v4::SessionFileStore;
            use rumqttc_v4::{SessionStore, SessionStoreKey};

            let fixture = fixture_v4(usize::from(args.inflight), args.payload_size, args.qos);
            let key = SessionStoreKey::new("benchmark", "benchmark-client-v4");
            let store = SessionFileStore::open(temporary.path()).await?;
            store
                .save(&key, &fixture)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            checkpoint_size = std::fs::metadata(store.checkpoint_path(&key)?)?.len();
            let mut options = rumqttc_v4::MqttOptions::new("benchmark-client-v4", "localhost");
            options.set_clean_session(false).set_inflight(args.inflight);
            let mut observed_replay = 0;
            for _ in 0..args.samples {
                let started = Instant::now();
                let loaded = store
                    .load(&key)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?
                    .context("checkpoint disappeared")?;
                load_samples.push(nanos_u64(started.elapsed().as_nanos()));
                let started = Instant::now();
                observed_replay =
                    rumqttc_v4::bench_instrumentation::apply_persisted_session(&options, &loaded)?;
                apply_samples.push(nanos_u64(started.elapsed().as_nanos()));
            }
            replay_count = observed_replay;
        }
        Protocol::V5 => {
            use rumqttc_store::v5::SessionFileStore;
            use rumqttc_v5::{SessionStore, SessionStoreKey};

            let fixture = fixture_v5(usize::from(args.inflight), args.payload_size, args.qos);
            let key = SessionStoreKey::new("benchmark", "benchmark-client-v5");
            let store = SessionFileStore::open(temporary.path()).await?;
            store
                .save(&key, &fixture)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            checkpoint_size = std::fs::metadata(store.checkpoint_path(&key)?)?.len();
            let mut options = rumqttc_v5::MqttOptions::new("benchmark-client-v5", "localhost");
            options
                .set_clean_start(false)
                .set_session_expiry_interval(Some(3600))
                .set_outgoing_inflight_upper_limit(args.inflight);
            let mut observed_replay = 0;
            for _ in 0..args.samples {
                let started = Instant::now();
                let loaded = store
                    .load(&key)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?
                    .context("checkpoint disappeared")?;
                load_samples.push(nanos_u64(started.elapsed().as_nanos()));
                let started = Instant::now();
                observed_replay =
                    rumqttc_v5::bench_instrumentation::apply_persisted_session(&options, &loaded)?;
                apply_samples.push(nanos_u64(started.elapsed().as_nanos()));
            }
            replay_count = observed_replay;
        }
    }
    if replay_count != usize::from(args.inflight) {
        bail!(
            "restored replay count {replay_count} did not match configured inflight {}",
            args.inflight
        );
    }
    let mut metrics = BTreeMap::from([("replay_count".to_owned(), replay_count as f64)]);
    insert_distribution_metrics(&mut metrics, "adapter_load", &load_samples);
    insert_distribution_metrics(&mut metrics, "state_apply", &apply_samples);
    let mut samples = BTreeMap::new();
    samples.insert(
        "adapter_load_latency_ns".to_owned(),
        load_samples.into_iter().map(|value| value as f64).collect(),
    );
    samples.insert(
        "state_apply_latency_ns".to_owned(),
        apply_samples
            .into_iter()
            .map(|value| value as f64)
            .collect(),
    );
    print_output(&BenchOutput {
        schema_version: 1,
        run_id: run_id(args.common.run_id.as_deref(), "persistence-recovery"),
        scenario: "persistence-mqtt-recovery".to_owned(),
        started_at_unix: started_at,
        finished_at_unix: unix_secs(),
        config: json!({
            "protocol": args.common.protocol,
            "checkpoint_shape": "outgoing publish replay",
            "payload_size": args.payload_size,
            "inflight": args.inflight,
            "qos": args.qos,
            "checkpoint_size": checkpoint_size,
            "samples": args.samples,
            "filesystem_backend": if cfg!(unix) { "atomic-write-file" } else { "windows-native" },
            "load_path": "production SessionFileStore::load",
            "apply_path": "production MqttState persisted-session restore",
        }),
        metrics,
        samples,
        environment: environment(),
    })
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

fn insert_distribution_metrics(metrics: &mut BTreeMap<String, f64>, prefix: &str, samples: &[u64]) {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    metrics.insert(format!("{prefix}_p50_ns"), percentile(&sorted, 50) as f64);
    metrics.insert(format!("{prefix}_p95_ns"), percentile(&sorted, 95) as f64);
    metrics.insert(format!("{prefix}_p99_ns"), percentile(&sorted, 99) as f64);
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
        let format = benchmark_format();
        let actual = envelope::encode(&format, b"abc", 1024).unwrap();
        assert_eq!(&actual[..18], b"BLOBBNCH\0\x01\0\0\0\0\0\0\0\x03");
        assert_eq!(
            envelope::decode(&format, &mut Cursor::new(actual), 1024).unwrap(),
            b"abc"
        );
    }

    #[test]
    fn draining_benchmark_events_removes_prior_sample_observations() {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender
            .send(envelope::BenchmarkEvent::SaveStreamInputStarved)
            .unwrap();
        sender
            .send(envelope::BenchmarkEvent::LoadStreamOutputBackpressured)
            .unwrap();
        let receiver = Arc::new(Mutex::new(receiver));

        drain_benchmark_events(&receiver).unwrap();

        assert!(matches!(
            receiver.lock().unwrap().try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
    }
}
