//! Allocation and caller-retention evidence for optional protocol observations.
use rumqttc_wrapper_core::{
    ClientConfig, Command, NativeClient, PublishCommand, PublishProtocolOptions, QoS, WrapperEvent,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct Allocator;
static TRACK: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn allocated(size: usize) {
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    if TRACK.load(Ordering::Relaxed) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(size, Ordering::Relaxed);
        PEAK.fetch_max(live, Ordering::Relaxed);
    }
}
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            allocated(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe {
            System.dealloc(p, layout);
        }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(p, layout, size) };
        if !next.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            allocated(size);
        }
        next
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn frame(stream: &mut impl Read) -> (u8, Vec<u8>) {
    let mut b = [0];
    stream.read_exact(&mut b).unwrap();
    let header = b[0];
    let mut len = 0;
    for shift in [0, 7, 14, 21] {
        stream.read_exact(&mut b).unwrap();
        len |= usize::from(b[0] & 127) << shift;
        if b[0] < 128 {
            let mut body = vec![0; len];
            stream.read_exact(&mut body).unwrap();
            return (header, body);
        }
    }
    panic!("invalid fixture packet");
}
fn number(args: &[String], key: &str, default: usize) -> usize {
    args.windows(2)
        .find(|v| v[0] == key)
        .map_or(default, |v| v[1].parse().unwrap())
}

fn run_peer(listener: &TcpListener, mqtt5: bool, property_bytes: usize) {
    let (mut socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    assert_eq!(frame(&mut socket).0, 0x10);
    socket
        .write_all(if mqtt5 {
            &[0x20, 3, 0, 0, 0]
        } else {
            &[0x20, 2, 0, 0]
        })
        .unwrap();
    let mut ack = bytes::BytesMut::new();
    if mqtt5 {
        rumqttc_v5::PubAck::new(
            1,
            Some(rumqttc_v5::PubAckProperties {
                reason_string: Some("r".repeat(property_bytes)),
                user_properties: vec![("k".into(), "first".into()), ("k".into(), String::new())],
            }),
        )
        .write(&mut ack)
        .unwrap();
    } else {
        ack.extend_from_slice(&[0x40, 2, 0, 1]);
    }
    let id_offset = ack.iter().skip(1).position(|v| *v < 128).unwrap() + 2;
    loop {
        let (header, body) = frame(&mut socket);
        if header == 0xe0 {
            break;
        }
        assert_eq!(header, 0x32);
        // Fixed one-byte topic "a".
        ack[id_offset..id_offset + 2].copy_from_slice(&body[3..5]);
        socket.write_all(&ack).unwrap();
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let selected = args.iter().any(|v| v == "--observations");
    let mqtt5 = args.iter().any(|v| v == "--v5");
    let messages = number(&args, "--messages", 1000);
    let property_bytes = number(&args, "--property-bytes", 8192);
    let retained_limit = number(&args, "--retain", 0);
    let slow_us = u64::try_from(number(&args, "--slow-us", 0)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = std::thread::spawn(move || run_peer(&listener, mqtt5, property_bytes));
    let mut config = if mqtt5 {
        ClientConfig::v5("observations-bench", "127.0.0.1", port)
    } else {
        ClientConfig::v4("observations-bench", "127.0.0.1", port)
    };
    config.common.network.tcp_nodelay = true;
    // BEGIN TODO44 SELECTION: omitted when measuring the pre-change baseline.
    config.common.protocol_observations = rumqttc_wrapper_core::ProtocolObservationSelection {
        incoming_control: selected,
        precise_outgoing: selected,
        raw_auth: false,
    };
    // END TODO44 SELECTION
    let mut client = NativeClient::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));
    let mut retained = VecDeque::with_capacity(retained_limit + 1);
    let (elapsed, initial_live) = {
        let mut transaction = |keep: bool| {
            client
                .handle()
                .try_admit(Command::Publish(PublishCommand {
                    topic: "a".into(),
                    payload: bytes::Bytes::from_static(b"value"),
                    qos: QoS::AtLeastOnce,
                    retain: false,
                    protocol: PublishProtocolOptions::VersionNeutral,
                }))
                .unwrap()
                .completion
                .wait_timeout(Duration::from_secs(5))
                .unwrap();
            if selected {
                for _ in 0..2 {
                    let event = events
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap()
                        .unwrap();
                    if slow_us != 0 {
                        std::thread::sleep(Duration::from_micros(slow_us));
                    }
                    if keep && retained_limit != 0 {
                        retained.push_back(event);
                        if retained.len() > retained_limit {
                            retained.pop_front();
                        }
                    }
                }
            }
        };
        for _ in 0..32 {
            transaction(false);
        }
        let initial_live = LIVE.load(Ordering::Relaxed);
        PEAK.store(initial_live, Ordering::Relaxed);
        TRACK.store(true, Ordering::Relaxed);
        let started = Instant::now();
        for _ in 0..messages {
            transaction(true);
        }
        let elapsed = started.elapsed();
        TRACK.store(false, Ordering::Relaxed);
        (elapsed, initial_live)
    };
    let calls = CALLS.load(Ordering::Relaxed);
    let allocated_bytes = BYTES.load(Ordering::Relaxed);
    let peak_extra = PEAK.load(Ordering::Relaxed).saturating_sub(initial_live);
    client.closer().close(Duration::from_secs(5)).unwrap();
    peer.join().unwrap();
    drop(client);
    drop(events);
    let before_release = LIVE.load(Ordering::Relaxed);
    let retained_count = retained.len();
    drop(retained);
    let released = before_release.saturating_sub(LIVE.load(Ordering::Relaxed));
    println!(
        "mqtt5,selected,messages,property_bytes,retain,slow_us,allocations,allocated_bytes,peak_extra_bytes,retained_events,released_bytes,elapsed_ns"
    );
    println!(
        "{mqtt5},{selected},{messages},{property_bytes},{retained_limit},{slow_us},{calls},{allocated_bytes},{peak_extra},{retained_count},{released},{}",
        elapsed.as_nanos()
    );
}
