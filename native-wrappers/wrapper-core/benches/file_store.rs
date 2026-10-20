//! End-to-end checkpoint endpoint/codec costs, deliberately excluding MQTT/network work.
use rumqttc_wrapper_core::{
    FileStoreContext, FileStoreContextOptions, FileStoreOptions, ProtocolVersion,
    SessionCheckpoint, SessionStoreKey,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED: AtomicU64 = AtomicU64::new(0);
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
fn main() {
    let iterations = std::env::var("RUMQTTC_FILE_STORE_BENCH_ITERATIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20)
        .max(1);
    for protocol in [ProtocolVersion::V4, ProtocolVersion::V5] {
        for payload in [1024, 64 * 1024, 1024 * 1024] {
            let start = Instant::now();
            let bytes = match protocol {
                ProtocolVersion::V4 => {
                    use rumqttc_v4::*;
                    PersistedSession {
                        format_version: 2,
                        client_id: "bench".into(),
                        clean_session: false,
                        max_inflight: 10,
                        ack_mode: PersistedAckMode::Automatic,
                        incoming_qos2: vec![],
                        replay: vec![PersistedRequest::Publish(PersistedPublish {
                            dup: true,
                            qos: PersistedQoS::AtLeastOnce,
                            retain: false,
                            topic: b"bench".to_vec(),
                            pkid: 1,
                            payload: vec![42; payload],
                        })],
                    }
                    .encode()
                    .unwrap()
                }
                ProtocolVersion::V5 => {
                    use rumqttc_v5::*;
                    PersistedSession {
                        format_version: 2,
                        client_id: "bench".into(),
                        clean_start: false,
                        session_expiry_interval: Some(60),
                        outgoing_inflight_upper_limit: Some(10),
                        ack_mode: PersistedAckMode::Automatic,
                        incoming_qos2: vec![],
                        replay: vec![PersistedRequest::Publish(PersistedPublish {
                            dup: true,
                            qos: PersistedQoS::AtLeastOnce,
                            retain: false,
                            topic: b"bench".to_vec(),
                            pkid: 1,
                            payload: vec![42; payload],
                            properties: None,
                        })],
                    }
                    .encode()
                    .unwrap()
                }
            };
            let setup_encode_ns = start.elapsed().as_nanos();
            let mut envelope = b"RMWC\0\x01\0\0".to_vec();
            envelope[6] = if protocol == ProtocolVersion::V4 {
                4
            } else {
                5
            };
            envelope.extend_from_slice(&bytes);
            let checkpoint = SessionCheckpoint(envelope.into());
            let root = tempfile::tempdir().unwrap();
            let context = FileStoreContext::new(FileStoreContextOptions::default()).unwrap();
            let options = FileStoreOptions {
                max_checkpoint_size: 2 * 1024 * 1024,
                ..FileStoreOptions::default()
            };
            let store = context
                .open(protocol, root.path().to_owned(), options)
                .unwrap()
                .wait(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .unwrap();
            let config = store
                .configuration(
                    "bench".into(),
                    Duration::from_secs(5),
                    options.max_checkpoint_size + 8,
                )
                .unwrap();
            let key = SessionStoreKey {
                protocol,
                scope: "bench".into(),
                client_id: "bench".into(),
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let allocations = ALLOCATIONS.load(Ordering::Relaxed);
            let allocated = ALLOCATED.load(Ordering::Relaxed);
            let mut samples = Vec::new();
            for _ in 0..iterations {
                let start = Instant::now();
                runtime
                    .block_on(config.store.save(key.clone(), checkpoint.clone()))
                    .unwrap();
                let loaded = runtime
                    .block_on(config.store.load(key.clone()))
                    .unwrap()
                    .unwrap();
                assert_eq!(loaded, checkpoint);
                samples.push(start.elapsed().as_nanos());
            }
            let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations;
            let allocated = ALLOCATED.load(Ordering::Relaxed) - allocated;
            samples.sort_unstable();
            println!(
                "{{\"protocol\":\"{protocol:?}\",\"payload_bytes\":{payload},\"checkpoint_bytes\":{},\"iterations\":{iterations},\"setup_encode_ns\":{setup_encode_ns},\"save_load_p50_ns\":{},\"save_load_p90_ns\":{},\"allocations\":{allocations},\"allocated_bytes\":{allocated},\"maximum_checkpoint_buffer_reservation\":{}}}",
                checkpoint.0.len(),
                samples[iterations / 2],
                samples[(iterations * 9 / 10).min(iterations - 1)],
                (options.max_checkpoint_size + 8) * 8
            );
            context.request_shutdown().unwrap();
            assert!(context.join(Duration::from_secs(5)).unwrap());
        }
    }
}
