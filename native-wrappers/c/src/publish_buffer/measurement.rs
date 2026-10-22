use super::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
    io::Write,
    time::Instant,
};
thread_local! { static COUNTS: Cell<Option<(u64,u64)>> = const { Cell::new(None) }; }
struct Counting;
fn charge(bytes: usize) {
    let _ = COUNTS.try_with(|c| {
        if let Some((n, b)) = c.get() {
            c.set(Some((n + 1, b + bytes as u64)));
        }
    });
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            charge(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            charge(l.size());
        }
        p
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let p = unsafe { System.realloc(p, l, n) };
        if !p.is_null() {
            charge(n);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
#[ignore = "release-build admission/wait measurement, run serially"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep alternating benchmark samples and instrumentation together"
)]
fn publish_buffer_admission_measurements() {
    let path = std::env::var("RUMQTTC_BUFFER_ADMISSION_RESULTS").expect("result path");
    let mut output = std::fs::File::create(path).unwrap();
    for protocol in [1, 2] {
        for len in [64, 1024, 16_384, 262_144, 1_048_576] {
            for phase in ["admitted", "deferred"] {
                let admitted = phase == "admitted";
                let mut client = Client::configured(protocol, false, false, admitted, |c| {
                    c.common.connection_timeout = Duration::from_secs(3600);
                    c.common.event_buffer_capacity = 32_768;
                });
                let mut id = 0;
                if !admitted {
                    assert_eq!(client.peer.frame().0, 0x10);
                    assert_eq!(
                        unsafe {
                            rumqttc_client_try_publish(
                                client.raw,
                                view_string("a"),
                                view_bytes(b"fill"),
                                ptr::null(),
                                &raw mut id,
                                ptr::null_mut(),
                            )
                        },
                        0
                    );
                }
                let data = vec![42; len];
                let count = AtomicUsize::new(0);
                let mut buffer = ptr::null_mut();
                assert_eq!(
                    unsafe {
                        rumqttc_publish_buffer_create(
                            view_bytes(&data),
                            Some(release_count),
                            std::ptr::from_ref(&count).cast_mut().cast(),
                            &raw mut buffer,
                            ptr::null_mut(),
                        )
                    },
                    0
                );
                for sample in 0..21 {
                    for mode in if sample % 2 == 0 {
                        ["copy", "owned"]
                    } else {
                        ["owned", "copy"]
                    } {
                        for instrumented in [false, true] {
                            let mut total_ns = 0u128;
                            let mut cleanup_ns = 0u128;
                            let mut allocations = 0u64;
                            let mut allocated_bytes = 0u64;
                            for iteration in 0..40 {
                                let mut pending = ptr::null_mut();
                                if instrumented {
                                    COUNTS.set(Some((0, 0)));
                                }
                                let start = Instant::now();
                                let status = unsafe {
                                    match (admitted, mode) {
                                        (true, "copy") => rumqttc_client_try_publish(
                                            client.raw,
                                            view_string("a"),
                                            view_bytes(&data),
                                            ptr::null(),
                                            &raw mut id,
                                            ptr::null_mut(),
                                        ),
                                        (true, _) => rumqttc_client_try_publish_buffer(
                                            client.raw,
                                            view_string("a"),
                                            buffer,
                                            ptr::null(),
                                            &raw mut id,
                                            ptr::null_mut(),
                                        ),
                                        (false, "copy") => rumqttc_client_publish_deferred(
                                            client.raw,
                                            view_string("a"),
                                            view_bytes(&data),
                                            ptr::null(),
                                            &raw mut pending,
                                            ptr::null_mut(),
                                        ),
                                        (false, _) => rumqttc_client_publish_buffer_deferred(
                                            client.raw,
                                            view_string("a"),
                                            buffer,
                                            ptr::null(),
                                            &raw mut pending,
                                            ptr::null_mut(),
                                        ),
                                    }
                                };
                                let ns = start.elapsed().as_nanos();
                                let counts = COUNTS.replace(None).unwrap_or((0, 0));
                                assert_eq!(status, 0);
                                let cleanup_start = Instant::now();
                                if admitted {
                                    let frame = client.peer.frame();
                                    assert_eq!(frame.0, 0x30);
                                    assert!(frame.1.ends_with(&data));
                                } else {
                                    assert_eq!(
                                        unsafe {
                                            rumqttc_pending_admission_cancel(
                                                pending,
                                                ptr::null_mut(),
                                            )
                                        },
                                        0
                                    );
                                    let mut state = 99;
                                    assert_eq!(
                                        unsafe {
                                            rumqttc_pending_admission_wait_timeout_ms(
                                                pending,
                                                3000,
                                                &raw mut state,
                                                ptr::null_mut(),
                                                ptr::null_mut(),
                                                ptr::null_mut(),
                                            )
                                        },
                                        0
                                    );
                                    assert_eq!(state, 3);
                                    unsafe { rumqttc_pending_admission_release(pending) };
                                }
                                if iteration >= 8 {
                                    total_ns += ns;
                                    cleanup_ns += cleanup_start.elapsed().as_nanos();
                                    allocations += counts.0;
                                    allocated_bytes += counts.1;
                                }
                            }
                            writeln!(
                                output,
                                "{{\"protocol\":{protocol},\"size\":{len},\"phase\":\"{phase}\",\
                                \"sample\":{sample},\"mode\":\"{mode}\",\
                                \"instrumented\":{instrumented},\"iterations\":32,\"ns\":{total_ns},\
                                \"cleanup_ns\":{cleanup_ns},\"allocations\":{allocations},\
                                \"allocated_bytes\":{allocated_bytes},\
                                \"application_backing_bytes\":{len}}}",
                            ).unwrap();
                        }
                    }
                }
                unsafe { rumqttc_publish_buffer_release(buffer) };
                client.destroy();
                assert_eq!(count.load(Ordering::SeqCst), 1);
            }
        }
    }
}

#[test]
#[ignore = "release-build measurement, run serially with --nocapture"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep alternating benchmark samples and instrumentation together"
)]
fn publish_buffer_measurements() {
    let path = std::env::var("RUMQTTC_BUFFER_RESULTS").expect("result path");
    let mut output = std::fs::File::create(path).unwrap();
    for protocol in [1, 2] {
        let mut client = Client::configured(protocol, false, false, false, |config| {
            config.common.connection_timeout = Duration::from_secs(3600);
        });
        assert_eq!(client.peer.frame().0, 0x10);
        let mut id = 0;
        assert_eq!(
            unsafe {
                rumqttc_client_try_publish(
                    client.raw,
                    view_string("a"),
                    view_bytes(b"fill"),
                    ptr::null(),
                    &raw mut id,
                    ptr::null_mut(),
                )
            },
            0
        );
        for len in [64, 1024, 16_384, 262_144, 1_048_576] {
            let data = vec![42; len];
            let count = AtomicUsize::new(0);
            let mut buffer = ptr::null_mut();
            assert_eq!(
                unsafe {
                    rumqttc_publish_buffer_create(
                        view_bytes(&data),
                        Some(release_count),
                        std::ptr::from_ref(&count).cast_mut().cast(),
                        &raw mut buffer,
                        ptr::null_mut(),
                    )
                },
                0
            );
            let iterations = (64 * 1024 * 1024 / len).clamp(256, 4096);
            for phase in ["build", "rejected", "lifecycle"] {
                for sample in 0..21 {
                    for mode in if sample % 2 == 0 {
                        ["copy", "owned"]
                    } else {
                        ["owned", "copy"]
                    } {
                        for allocations in [false, true] {
                            let mut run = || {
                                let source = if phase == "lifecycle" {
                                    black_box(vec![42; len])
                                } else {
                                    Vec::new()
                                };
                                let input = if phase == "lifecycle" {
                                    source.as_slice()
                                } else {
                                    data.as_slice()
                                };
                                if mode == "copy" {
                                    if phase == "rejected" {
                                        assert_eq!(
                                            unsafe {
                                                rumqttc_client_try_publish(
                                                    client.raw,
                                                    view_string("a"),
                                                    view_bytes(input),
                                                    ptr::null(),
                                                    &raw mut id,
                                                    ptr::null_mut(),
                                                )
                                            },
                                            4
                                        );
                                    } else {
                                        black_box(
                                            unsafe {
                                                publish_command(
                                                    view_string("a"),
                                                    view_bytes(input),
                                                    ptr::null(),
                                                )
                                            }
                                            .unwrap(),
                                        );
                                    }
                                } else {
                                    let active = if phase == "lifecycle" {
                                        let mut fresh = ptr::null_mut();
                                        assert_eq!(
                                            unsafe {
                                                rumqttc_publish_buffer_create(
                                                    view_bytes(input),
                                                    Some(release_count),
                                                    std::ptr::from_ref(&count).cast_mut().cast(),
                                                    &raw mut fresh,
                                                    ptr::null_mut(),
                                                )
                                            },
                                            0
                                        );
                                        fresh
                                    } else {
                                        buffer
                                    };
                                    if phase == "rejected" {
                                        assert_eq!(
                                            unsafe {
                                                rumqttc_client_try_publish_buffer(
                                                    client.raw,
                                                    view_string("a"),
                                                    active,
                                                    ptr::null(),
                                                    &raw mut id,
                                                    ptr::null_mut(),
                                                )
                                            },
                                            4
                                        );
                                    } else {
                                        black_box(
                                            unsafe {
                                                buffer_command(
                                                    view_string("a"),
                                                    active,
                                                    ptr::null(),
                                                )
                                            }
                                            .unwrap(),
                                        );
                                    }
                                    if phase == "lifecycle" {
                                        unsafe { rumqttc_publish_buffer_release(active) };
                                    }
                                }
                            };
                            for _ in 0..32 {
                                run();
                            }
                            if allocations {
                                COUNTS.set(Some((0, 0)));
                            }
                            let start = Instant::now();
                            for _ in 0..iterations {
                                run();
                            }
                            let ns = start.elapsed().as_nanos();
                            let counts = COUNTS.replace(None).unwrap_or((0, 0));
                            writeln!(
                                output,
                                "{{\"protocol\":{protocol},\"size\":{len},\"phase\":\"{phase}\",\
                                \"sample\":{sample},\"mode\":\"{mode}\",\
                                \"instrumented\":{allocations},\"iterations\":{iterations},\
                                \"ns\":{ns},\"allocations\":{},\"allocated_bytes\":{}}}",
                                counts.0, counts.1,
                            )
                            .unwrap();
                        }
                    }
                }
            }
            unsafe { rumqttc_publish_buffer_release(buffer) };
            assert!(count.load(Ordering::SeqCst) > 0);
        }
        client.destroy();
    }
}
