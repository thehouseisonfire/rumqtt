//! Whole-workload measurements against an immediately acknowledging TCP peer.
#![allow(
    clippy::cast_precision_loss,
    reason = "Benchmark counters are reported as floating point rates"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use rumqttc_wrapper_core::{
    ClientConfig, Command, NativeClient, ProtocolConfig, PublishCommand, PublishProtocolOptions,
    QoS, WrapperEvent,
};

struct Allocator;
static TRACK: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() && TRACK.load(Ordering::Relaxed) {
            CALLS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        result
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe {
            System.dealloc(pointer, layout);
        }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() && TRACK.load(Ordering::Relaxed) {
            CALLS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(size, Ordering::Relaxed);
        }
        result
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn number(arguments: &[String], name: &str, default: usize) -> usize {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map_or(default, |pair| pair[1].parse().unwrap())
}

fn frame(socket: &mut impl Read) -> Vec<u8> {
    let mut byte = [0];
    socket.read_exact(&mut byte).unwrap();
    let mut packet = vec![byte[0]];
    let mut length = 0;
    for shift in [0, 7, 14, 21] {
        socket.read_exact(&mut byte).unwrap();
        packet.push(byte[0]);
        length |= usize::from(byte[0] & 127) << shift;
        if byte[0] < 128 {
            let header = packet.len();
            packet.resize(header + length, 0);
            socket.read_exact(&mut packet[header..]).unwrap();
            return packet;
        }
    }
    panic!("invalid benchmark peer packet");
}

#[allow(
    clippy::too_many_lines,
    reason = "One benchmark transaction with a paired peer and producer threads"
)]
fn main() {
    let arguments: Vec<_> = std::env::args().collect();
    let mqtt5 = arguments.iter().any(|argument| argument == "--v5");
    let ordered = arguments.iter().any(|argument| argument == "--ordered");
    let allocations = arguments.iter().any(|argument| argument == "--allocations");
    assert!(!ordered || cfg!(feature = "ordered-shutdown"));
    let messages = number(&arguments, "--messages", 10_000);
    let producers = number(&arguments, "--producers", 1);
    let capacity = number(&arguments, "--capacity", 64);
    let inflight = u16::try_from(number(&arguments, "--inflight", 64)).unwrap();
    let qos = match number(&arguments, "--qos", 1) {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        _ => panic!("invalid QoS"),
    };
    assert!(messages > 0 && producers > 0);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_nodelay(true).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        assert_eq!(frame(&mut socket)[0] >> 4, 1);
        socket
            .write_all(if mqtt5 {
                &[0x20, 3, 0, 0, 0]
            } else {
                &[0x20, 2, 0, 0]
            })
            .unwrap();
        let mut received = 0;
        loop {
            let packet = frame(&mut socket);
            match packet[0] >> 4 {
                3 => {
                    received += 1;
                    if qos != QoS::AtMostOnce {
                        socket
                            .write_all(&[
                                if qos == QoS::AtLeastOnce { 0x40 } else { 0x50 },
                                2,
                                packet[5],
                                packet[6],
                            ])
                            .unwrap();
                    }
                }
                6 => socket.write_all(&[0x70, 2, packet[2], packet[3]]).unwrap(),
                14 => {
                    assert_eq!(received, messages);
                    break;
                }
                _ => panic!("unexpected benchmark packet"),
            }
        }
    });
    let mut config = if mqtt5 {
        ClientConfig::v5("wrapper-bench", "127.0.0.1", port)
    } else {
        ClientConfig::v4("wrapper-bench", "127.0.0.1", port)
    };
    config.common.request_channel_capacity = capacity;
    config.common.network.tcp_nodelay = true;
    match &mut config.protocol {
        ProtocolConfig::V4(config) => config.inflight_limit = inflight,
        ProtocolConfig::V5(config) => config.outgoing_inflight_upper_limit = Some(inflight),
    }
    let mut client = NativeClient::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    loop {
        if matches!(
            events.recv_timeout(Duration::from_secs(60)).unwrap(),
            Some(WrapperEvent::Connected { .. })
        ) {
            break;
        }
    }
    let barrier = Arc::new(Barrier::new(producers + 1));
    let payload = bytes::Bytes::from_static(&[42; 64]);
    let (observations, mut latencies, started, admission_elapsed) = std::thread::scope(|scope| {
        let workers = (0..producers)
            .map(|index| {
                let handle = client.handle();
                let barrier = barrier.clone();
                let payload = payload.clone();
                scope.spawn(move || {
                    let count = messages / producers + usize::from(index < messages % producers);
                    let mut observations = Vec::with_capacity(count);
                    let mut latencies = Vec::with_capacity(count);
                    barrier.wait();
                    for _ in 0..count {
                        let started = Instant::now();
                        let admission = handle
                            .admit(Command::Publish(PublishCommand {
                                topic: "a".into(),
                                payload: payload.clone(),
                                qos,
                                retain: false,
                                protocol: PublishProtocolOptions::VersionNeutral,
                            }))
                            .unwrap();
                        latencies.push(u64::try_from(started.elapsed().as_nanos()).unwrap());
                        observations.push(admission);
                    }
                    (observations, latencies)
                })
            })
            .collect::<Vec<_>>();
        let started = Instant::now();
        TRACK.store(allocations, Ordering::Relaxed);
        barrier.wait();
        let mut observations = Vec::with_capacity(messages);
        let mut latencies = Vec::with_capacity(messages);
        for worker in workers {
            let (mut results, mut times) = worker.join().unwrap();
            observations.append(&mut results);
            latencies.append(&mut times);
        }
        (observations, latencies, started, started.elapsed())
    });
    TRACK.store(false, Ordering::Relaxed);
    let shutdown_started = Instant::now();
    #[cfg(feature = "ordered-shutdown")]
    if ordered {
        // Include waiting for a bounded request queue in measured shutdown time.
        client
            .handle()
            .admit(Command::OrderedDisconnect {
                timeout: Some(Duration::from_secs(60)),
            })
            .unwrap();
        client
            .closer()
            .close_after_queued(Duration::from_secs(60))
            .unwrap();
    }
    if !ordered {
        for observation in observations {
            observation
                .completion
                .wait_timeout(Duration::from_secs(60))
                .unwrap();
        }
        client.closer().close(Duration::from_secs(60)).unwrap();
    }
    let shutdown_elapsed = shutdown_started.elapsed();
    let elapsed = started.elapsed();
    peer.join().unwrap();
    latencies.sort_unstable();
    let percentile =
        |percent: usize| latencies[(latencies.len() - 1) * percent / 100] as f64 / 1000.0;
    println!(
        "{{\"protocol\":\"{}\",\"feature\":{},\"ordered\":{},\"allocation_tracking\":{},\"qos\":{},\"messages\":{},\"producers\":{},\"capacity\":{},\"inflight\":{},\"admissions_per_second\":{},\"completed_per_second\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"shutdown_ms\":{},\"allocations_per_message\":{},\"allocated_bytes_per_message\":{}}}",
        if mqtt5 { "v5" } else { "v4" },
        cfg!(feature = "ordered-shutdown"),
        ordered,
        allocations,
        qos as u8,
        messages,
        producers,
        capacity,
        inflight,
        messages as f64 / admission_elapsed.as_secs_f64(),
        messages as f64 / elapsed.as_secs_f64(),
        percentile(50),
        percentile(95),
        percentile(99),
        shutdown_elapsed.as_secs_f64() * 1000.0,
        CALLS.load(Ordering::Relaxed) as f64 / messages as f64,
        BYTES.load(Ordering::Relaxed) as f64 / messages as f64
    );
}
