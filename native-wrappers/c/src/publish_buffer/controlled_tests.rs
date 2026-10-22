use super::*;
use crate::ffi::publish_buffer::adopt;
#[cfg(feature = "publish-buffer-proof")]
use crate::ffi::publish_buffer::buffer_command;

unsafe extern "C" fn release_count(userdata: *mut std::ffi::c_void, _: *const u8, _: usize) {
    // The counter is retained by the test through joined client cleanup.
    unsafe { &*userdata.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
    assert!(!wc::blocking_wait_allowed());
}

fn owned(data: &[u8], counter: &AtomicUsize) -> Bytes {
    unsafe {
        adopt(
            view_bytes(data),
            Some(release_count),
            std::ptr::from_ref(counter).cast_mut().cast(),
        )
    }
    .unwrap()
}

#[test]
fn private_buffer_proof_retains_storage_through_native_admission_and_teardown() {
    for protocol in [1, 2] {
        for shared in [false, true] {
            let data = vec![42; 1024];
            let count = AtomicUsize::new(0);
            let payload = owned(&data, &count);
            assert_eq!(payload.as_ptr(), data.as_ptr());
            let mut client = Client::configured(protocol, shared, false, false, |_| {});
            assert_eq!(client.peer.frame().0, 0x10);
            let command = wc::PublishCommand {
                topic: "a".into(),
                payload: payload.clone(),
                qos: wc::QoS::AtMostOnce,
                retain: false,
                protocol: wc::PublishProtocolOptions::VersionNeutral,
            };
            let handle = unsafe { &*client.raw }.inner.handle.clone();
            let admission = handle
                .try_admit(wc::Command::Publish(command.clone()))
                .unwrap();
            assert!(
                handle
                    .try_admit(wc::Command::Publish(command.clone()))
                    .is_err()
            );
            drop(command);
            drop(payload);
            assert_eq!(count.load(Ordering::SeqCst), 0);
            drop(admission);
            drop(handle);
            client.destroy();
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the replay and cancellation ownership proof together"
)]
fn private_buffer_proof_replay_and_pending_cancellation_keep_the_owner() {
    for protocol in [1, 2] {
        let data = vec![42; 32];
        let count = AtomicUsize::new(0);
        let mut client =
            Client::configured(protocol, false, false, true, |config| {
                match &mut config.protocol {
                    wc::ProtocolConfig::V4(v4) => v4.clean_session = false,
                    wc::ProtocolConfig::V5(v5) => {
                        v5.clean_start = false;
                        v5.connect_properties.session_expiry_interval = Some(60);
                    }
                }
            });
        let payload = owned(&data, &count);
        let command = wc::PublishCommand {
            topic: "a".into(),
            payload,
            qos: wc::QoS::AtLeastOnce,
            retain: false,
            protocol: wc::PublishProtocolOptions::VersionNeutral,
        };
        let handle = unsafe { &*client.raw }.inner.handle.clone();
        let admission = handle.try_admit(wc::Command::Publish(command)).unwrap();
        let first = client.peer.frame();
        assert_eq!(first.0, 0x32);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let (io, peer) = Peer::new();
        client
            .connector
            .as_ref()
            .unwrap()
            .streams
            .lock()
            .unwrap()
            .push_back(io);
        client.peer.send(Bytes::new());
        loop {
            let event = client.event();
            let disconnected = matches!(
                unsafe { &*event }.inner.event,
                wc::WrapperEvent::Disconnected { .. }
            );
            unsafe { rumqttc_event_destroy(event) };
            if disconnected {
                break;
            }
        }
        client.peer = peer;
        assert_eq!(client.peer.frame().0, 0x10);
        client.peer.send(if protocol == 1 {
            vec![0x20, 2, 1, 0]
        } else {
            vec![0x20, 3, 1, 0, 0]
        });
        unsafe { rumqttc_event_destroy(client.event()) };
        let replay = client.peer.frame();
        assert_eq!(replay.0, 0x3a);
        assert_eq!(replay.1, first.1);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        drop(admission);
        drop(handle);
        client.destroy();
        assert_eq!(count.load(Ordering::SeqCst), 1);

        let mut client = Client::configured(protocol, true, false, false, |_| {});
        assert_eq!(client.peer.frame().0, 0x10);
        let handle = unsafe { &*client.raw }.inner.handle.clone();
        if protocol == 1 {
            let command =
                unsafe { publish_command(view_string("a"), view_bytes(b"fill"), ptr::null()) }
                    .unwrap();
            handle.try_admit(wc::Command::Publish(command)).unwrap();
        }
        let count = AtomicUsize::new(0);
        let reservation = handle.reserve_pending_admission(data.len() + 1).unwrap();
        let pending = handle
            .defer_admission(
                reservation,
                wc::Command::Publish(wc::PublishCommand {
                    topic: "a".into(),
                    payload: owned(&data, &count),
                    qos: wc::QoS::AtLeastOnce,
                    retain: false,
                    protocol: wc::PublishProtocolOptions::VersionNeutral,
                }),
            )
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 0);
        pending.cancel();
        let until = std::time::Instant::now() + DEADLINE;
        loop {
            if let Some(outcome) = pending.poll() {
                assert!(matches!(outcome, wc::PendingAdmissionOutcome::Cancelled));
                break;
            }
            assert!(std::time::Instant::now() < until);
            thread::yield_now();
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(pending);
        drop(handle);
        client.destroy();
    }
}

// Separate producer measurements from ownership behavior tests.
#[cfg(feature = "publish-buffer-proof")]
#[path = "measurement.rs"]
mod measurement;

struct ReleaseContext {
    data: Vec<u8>,
    calls: Arc<AtomicUsize>,
}

unsafe extern "C" fn release_application(
    context: *mut std::ffi::c_void,
    data: *const u8,
    len: usize,
) {
    let context = unsafe { Box::from_raw(context.cast::<ReleaseContext>()) };
    assert_eq!((data, len), (context.data.as_ptr(), context.data.len()));
    assert!(!wc::blocking_wait_allowed());
    context.calls.fetch_add(1, Ordering::SeqCst);
}

fn public_buffer(data: &[u8], calls: &Arc<AtomicUsize>) -> *mut rumqttc_publish_buffer {
    let context = Box::new(ReleaseContext {
        data: data.to_vec(),
        calls: calls.clone(),
    });
    let view = view_bytes(&context.data);
    let context = Box::into_raw(context);
    let mut out = ptr::null_mut();
    assert_eq!(
        unsafe {
            rumqttc_publish_buffer_create(
                view,
                Some(release_application),
                context.cast(),
                &raw mut out,
                ptr::null_mut(),
            )
        },
        0
    );
    out
}

fn publish_options(qos: u32) -> rumqttc_publish_options_t {
    rumqttc_publish_options_t {
        struct_size: struct_size::<rumqttc_publish_options_t>(),
        qos,
        retain: 0,
        reserved: [0; 3],
        protocol_options: 0,
        v5_properties: ptr::null(),
    }
}

fn wait_released(calls: &AtomicUsize) {
    let until = std::time::Instant::now() + DEADLINE;
    while calls.load(Ordering::SeqCst) != 1 {
        assert!(
            std::time::Instant::now() < until,
            "buffer owner was not released"
        );
        thread::yield_now();
    }
}

#[test]
fn public_buffer_qos_completion_and_independent_client_lifetimes() {
    for protocol in [1, 2] {
        for shared in [false, true] {
            for qos in [0, 1, 2] {
                let calls = Arc::new(AtomicUsize::new(0));
                let buffer = public_buffer(b"application bytes", &calls);
                let mut retained = ptr::null_mut();
                assert_eq!(
                    unsafe {
                        rumqttc_publish_buffer_retain(buffer, &raw mut retained, ptr::null_mut())
                    },
                    0
                );
                let mut client = Client::configured(protocol, shared, false, true, |_| {});
                let mut completion = ptr::null_mut();
                let options = publish_options(qos);
                assert_eq!(
                    unsafe {
                        rumqttc_client_publish_buffer_tracked(
                            client.raw,
                            view_string("a"),
                            buffer,
                            &raw const options,
                            &raw mut completion,
                            ptr::null_mut(),
                        )
                    },
                    0
                );
                unsafe { rumqttc_publish_buffer_release(buffer) };
                let (header, body) = client.peer.frame();
                assert_eq!(header, 0x30 | u8::try_from(qos << 1).unwrap());
                assert!(body.ends_with(b"application bytes"));
                if qos != 0 {
                    let id = [body[3], body[4]];
                    client
                        .peer
                        .send(vec![if qos == 1 { 0x40 } else { 0x50 }, 2, id[0], id[1]]);
                    if qos == 2 {
                        let pubrel = client.peer.frame();
                        assert_eq!(pubrel, (0x62, id.to_vec()));
                        client.peer.send(vec![0x70, 2, id[0], id[1]]);
                    }
                }
                // MQTT progress continues while events are drained independently.
                let until = std::time::Instant::now() + DEADLINE;
                loop {
                    let status = unsafe { rumqttc_completion_poll(completion, ptr::null_mut()) };
                    if status == 0 {
                        break;
                    }
                    assert_eq!(status, 11);
                    let mut event = ptr::null_mut();
                    let result = unsafe {
                        rumqttc_client_event_recv_timeout_ms(
                            client.raw,
                            1,
                            &raw mut event,
                            ptr::null_mut(),
                        )
                    };
                    assert!(result == 0 || result == 5);
                    unsafe { rumqttc_event_destroy(event) };
                    assert!(std::time::Instant::now() < until);
                }
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                unsafe { rumqttc_completion_destroy(completion) };
                client.destroy();
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                unsafe { rumqttc_publish_buffer_release(retained) };
                wait_released(&calls);
            }
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep pending reservation and cancellation ownership assertions together"
)]
fn public_buffer_deferred_limits_and_cancellation_preserve_owner() {
    for protocol in [1, 2] {
        for shared in [false, true] {
            let mut client = Client::configured(protocol, shared, false, false, |c| {
                c.common.pending_admission_limits = wc::PendingAdmissionLimits {
                    max_pending: 2,
                    max_bytes: 10,
                };
                if let wc::ProtocolConfig::V5(v5) = &mut c.protocol {
                    v5.publish_budget = wc::PublishBudgetLimits {
                        max_outstanding: 2,
                        max_bytes: 10,
                    };
                }
            });
            assert_eq!(client.peer.frame().0, 0x10);
            let calls = Arc::new(AtomicUsize::new(0));
            let buffer = public_buffer(b"data", &calls);
            let options = publish_options(1);
            if protocol == 1 {
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
            }
            let mut pending = Vec::new();
            for _ in 0..2 {
                let mut out = ptr::null_mut();
                assert_eq!(
                    unsafe {
                        rumqttc_client_publish_buffer_deferred(
                            client.raw,
                            view_string("a"),
                            buffer,
                            &raw const options,
                            &raw mut out,
                            ptr::null_mut(),
                        )
                    },
                    0
                );
                pending.push(out);
            }
            let snapshot = unsafe { &*client.raw }
                .inner
                .handle
                .pending_admission_snapshot();
            assert_eq!((snapshot.pending, snapshot.retained_bytes), (2, 10));
            let mut rejected = ptr::dangling_mut();
            assert_eq!(
                unsafe {
                    rumqttc_client_publish_buffer_deferred(
                        client.raw,
                        view_string("a"),
                        buffer,
                        &raw const options,
                        &raw mut rejected,
                        ptr::null_mut(),
                    )
                },
                4
            );
            assert!(rejected.is_null());
            for p in pending {
                assert_eq!(
                    unsafe { rumqttc_pending_admission_cancel(p, ptr::null_mut()) },
                    0
                );
                let mut state = 99;
                assert_eq!(
                    unsafe {
                        rumqttc_pending_admission_wait_timeout_ms(
                            p,
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
                unsafe { rumqttc_pending_admission_release(p) };
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let snapshot = unsafe { &*client.raw }
                .inner
                .handle
                .pending_admission_snapshot();
            assert_eq!((snapshot.pending, snapshot.retained_bytes), (0, 0));
            client.destroy();
            unsafe { rumqttc_publish_buffer_release(buffer) };
            wait_released(&calls);
        }
    }
}

#[test]
fn public_buffer_native_budget_charges_each_shared_use() {
    for limits in [(2, 100), (3, 10)] {
        // Native MQTT 5 accounting counts each shared allocation use independently.
        let mut client = Client::configured(2, false, false, true, |c| {
            c.common.request_channel_capacity = 4;
            if let wc::ProtocolConfig::V5(v5) = &mut c.protocol {
                v5.publish_budget = wc::PublishBudgetLimits {
                    max_outstanding: limits.0,
                    max_bytes: limits.1,
                };
            }
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let buffer = public_buffer(b"data", &calls);
        let options = publish_options(1);
        for _ in 0..2 {
            let mut id = 0;
            assert_eq!(
                unsafe {
                    rumqttc_client_try_publish_buffer(
                        client.raw,
                        view_string("a"),
                        buffer,
                        &raw const options,
                        &raw mut id,
                        ptr::null_mut(),
                    )
                },
                0
            );
        }
        let snapshot = unsafe { &*client.raw }
            .inner
            .handle
            .publish_budget_snapshot()
            .unwrap();
        assert_eq!((snapshot.outstanding, snapshot.retained_bytes), (2, 10));
        let mut id = 99;
        assert_eq!(
            unsafe {
                rumqttc_client_try_publish_buffer(
                    client.raw,
                    view_string("a"),
                    buffer,
                    &raw const options,
                    &raw mut id,
                    ptr::null_mut(),
                )
            },
            4
        );
        assert_eq!(id, 0);
        unsafe { rumqttc_publish_buffer_release(buffer) };
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        client.destroy();
        wait_released(&calls);
    }
}

#[test]
fn public_buffer_utf8_rejection_and_deferred_transfer_preserve_ownership() {
    let mut client = Client::configured(2, true, false, true, |c| {
        c.common.request_channel_capacity = 8;
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let buffer = public_buffer(&[255], &calls);
    let mut properties: rumqttc_v5_publish_properties_t = unsafe { std::mem::zeroed() };
    properties.struct_size = struct_size::<rumqttc_v5_publish_properties_t>();
    properties.payload_format_present = 1;
    properties.payload_format_indicator = 1;
    let mut options = publish_options(0);
    options.protocol_options = 1;
    options.v5_properties = &raw const properties;
    let mut completion = ptr::dangling_mut();
    assert_ne!(
        unsafe {
            rumqttc_client_publish_buffer_tracked(
                client.raw,
                view_string("a"),
                buffer,
                &raw const options,
                &raw mut completion,
                ptr::null_mut(),
            )
        },
        0
    );
    assert!(completion.is_null());
    let mut pending = ptr::dangling_mut();
    assert_ne!(
        unsafe {
            rumqttc_client_publish_buffer_deferred(
                client.raw,
                view_string("a"),
                buffer,
                &raw const options,
                &raw mut pending,
                ptr::null_mut(),
            )
        },
        0
    );
    assert!(pending.is_null());
    assert_eq!(
        unsafe { &*client.raw }
            .inner
            .handle
            .pending_admission_snapshot()
            .pending,
        0
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // The identical immutable binary bytes remain publishable without UTF-8 PFI.
    assert_eq!(
        unsafe {
            rumqttc_client_publish_buffer_deferred(
                client.raw,
                view_string("a"),
                buffer,
                ptr::null(),
                &raw mut pending,
                ptr::null_mut(),
            )
        },
        0
    );
    unsafe { rumqttc_publish_buffer_release(buffer) };
    let mut state = 99;
    assert_eq!(
        unsafe {
            rumqttc_pending_admission_wait_timeout_ms(
                pending,
                3000,
                &raw mut state,
                ptr::null_mut(),
                &raw mut completion,
                ptr::null_mut(),
            )
        },
        0
    );
    assert_eq!(state, 2);
    assert_eq!(client.peer.frame(), (0x30, vec![0, 1, b'a', 0, 255]));
    unsafe {
        rumqttc_completion_destroy(completion);
        rumqttc_pending_admission_release(pending);
    }
    client.destroy();
    wait_released(&calls);
}

#[test]
fn public_buffer_callback_reentry_is_guarded_even_on_a_caller_thread() {
    struct Context {
        other: *mut rumqttc_publish_buffer,
        execution: *mut rumqttc_execution_context,
        calls: AtomicUsize,
    }
    unsafe extern "C" fn callback(userdata: *mut std::ffi::c_void, _: *const u8, _: usize) {
        let ctx = unsafe { &*userdata.cast::<Context>() };
        let mut clone = ptr::null_mut();
        assert_eq!(
            unsafe { rumqttc_publish_buffer_retain(ctx.other, &raw mut clone, ptr::null_mut()) },
            0
        );
        unsafe { rumqttc_publish_buffer_release(clone) };
        assert_ne!(rumqttc_library_capabilities() & (1 << 23), 0);
        assert_ne!(rumqttc_abi_version(), 0);
        let _ = rumqttc_library_version();
        assert_eq!(
            unsafe { rumqttc_execution_context_join_timeout_ms(ctx.execution, 1, ptr::null_mut()) },
            2
        );
        ctx.calls.fetch_add(1, Ordering::SeqCst);
    }
    let other_calls = Arc::new(AtomicUsize::new(0));
    let other = public_buffer(b"other", &other_calls);
    let mut execution = ptr::null_mut();
    assert_eq!(
        unsafe { rumqttc_execution_context_new(ptr::null(), &raw mut execution, ptr::null_mut()) },
        0
    );
    assert_eq!(
        unsafe { rumqttc_execution_context_request_shutdown(execution, ptr::null_mut()) },
        0
    );
    assert_eq!(
        unsafe { rumqttc_execution_context_join_timeout_ms(execution, 3000, ptr::null_mut()) },
        0
    );
    let context = Context {
        other,
        execution,
        calls: AtomicUsize::new(0),
    };
    let mut buffer = ptr::null_mut();
    assert_eq!(
        unsafe {
            rumqttc_publish_buffer_create(
                view_bytes(b""),
                Some(callback),
                std::ptr::from_ref(&context).cast_mut().cast(),
                &raw mut buffer,
                ptr::null_mut(),
            )
        },
        0
    );
    unsafe { rumqttc_publish_buffer_release(buffer) };
    assert_eq!(context.calls.load(Ordering::SeqCst), 1);
    assert!(wc::blocking_wait_allowed());
    // The same whitelist is safe when native queued work owns the last reference.
    let mut client = Client::configured(1, true, false, false, |_| {});
    assert_eq!(client.peer.frame().0, 0x10);
    assert_eq!(
        unsafe {
            rumqttc_publish_buffer_create(
                view_bytes(b""),
                Some(callback),
                std::ptr::from_ref(&context).cast_mut().cast(),
                &raw mut buffer,
                ptr::null_mut(),
            )
        },
        0
    );
    let mut id = 0;
    assert_eq!(
        unsafe {
            rumqttc_client_try_publish_buffer(
                client.raw,
                view_string("a"),
                buffer,
                ptr::null(),
                &raw mut id,
                ptr::null_mut(),
            )
        },
        0
    );
    unsafe { rumqttc_publish_buffer_release(buffer) };
    assert_eq!(context.calls.load(Ordering::SeqCst), 1);
    client.destroy();
    assert_eq!(context.calls.load(Ordering::SeqCst), 2);
    unsafe {
        rumqttc_publish_buffer_release(other);
        rumqttc_execution_context_release(execution);
    }
    wait_released(&other_calls);
}

#[derive(Default)]
struct BufferStore {
    checkpoint: Mutex<Option<wc::SessionCheckpoint>>,
    fail: AtomicBool,
}
impl wc::SessionStore for BufferStore {
    fn load(&self, _: wc::SessionStoreKey) -> wc::StoreFuture<Option<wc::SessionCheckpoint>> {
        Box::pin(async { Ok(None) })
    }
    fn save(
        &self,
        _: wc::SessionStoreKey,
        checkpoint: wc::SessionCheckpoint,
    ) -> wc::StoreFuture<()> {
        *self.checkpoint.lock().unwrap() = Some(checkpoint);
        let fail = self.fail.load(Ordering::SeqCst);
        Box::pin(async move {
            if fail {
                Err(wc::StoreFailure::Save)
            } else {
                Ok(())
            }
        })
    }
    fn clear(&self, _: wc::SessionStoreKey) -> wc::StoreFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn public_buffer_checkpoint_copies_and_persistence_failure_release_original_storage() {
    for protocol in [1, 2] {
        for fail in [false, true] {
            let store = Arc::new(BufferStore::default());
            let mut client = Client::configured(protocol, true, false, true, |c| {
                c.common.event_buffer_capacity = 64;
                let store =
                    wc::SessionStoreConfig::new(store.clone(), format!("buffer-{protocol}-{fail}"));
                match &mut c.protocol {
                    wc::ProtocolConfig::V4(v4) => {
                        v4.clean_session = false;
                        v4.session_store = Some(store);
                    }
                    wc::ProtocolConfig::V5(v5) => {
                        v5.clean_start = false;
                        v5.connect_properties.session_expiry_interval = Some(60);
                        v5.session_store = Some(store);
                    }
                }
            });
            store.fail.store(fail, Ordering::SeqCst);
            let calls = Arc::new(AtomicUsize::new(0));
            let buffer = public_buffer(b"retained checkpoint payload", &calls);
            let options = publish_options(1);
            let mut completion = ptr::null_mut();
            assert_eq!(
                unsafe {
                    rumqttc_client_publish_buffer_tracked(
                        client.raw,
                        view_string("a"),
                        buffer,
                        &raw const options,
                        &raw mut completion,
                        ptr::null_mut(),
                    )
                },
                0
            );
            unsafe { rumqttc_publish_buffer_release(buffer) };
            let until = std::time::Instant::now() + DEADLINE;
            let checkpoint = loop {
                if let Some(checkpoint) = store.checkpoint.lock().unwrap().clone()
                    && checkpoint
                        .0
                        .windows(b"retained checkpoint payload".len())
                        .any(|w| w == b"retained checkpoint payload")
                {
                    break checkpoint;
                }
                assert!(std::time::Instant::now() < until);
                thread::yield_now();
            };
            if fail {
                assert_ne!(
                    unsafe {
                        rumqttc_completion_wait_timeout_ms(completion, 3000, ptr::null_mut())
                    },
                    0
                );
            } else {
                let frame = client.peer.frame();
                assert_eq!(frame.0, 0x32);
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                client.peer.send(vec![0x40, 2, frame.1[3], frame.1[4]]);
                assert_eq!(
                    unsafe {
                        rumqttc_completion_wait_timeout_ms(completion, 3000, ptr::null_mut())
                    },
                    0
                );
            }
            unsafe { rumqttc_completion_destroy(completion) };
            client.destroy();
            wait_released(&calls);
            // The saved, independently allocated checkpoint remains readable
            // after the original C-owned payload has been reclaimed.
            assert!(
                checkpoint
                    .0
                    .windows(b"retained checkpoint payload".len())
                    .any(|w| w == b"retained checkpoint payload")
            );
        }
    }
}

#[test]
fn public_buffer_broker_rejection_preserves_reason_and_reusable_handle() {
    let mut client = Client::configured(2, false, false, true, |c| {
        c.common.event_buffer_capacity = 16;
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let buffer = public_buffer(b"rejected", &calls);
    let options = publish_options(1);
    let mut completion = ptr::null_mut();
    assert_eq!(
        unsafe {
            rumqttc_client_publish_buffer_tracked(
                client.raw,
                view_string("a"),
                buffer,
                &raw const options,
                &raw mut completion,
                ptr::null_mut(),
            )
        },
        0
    );
    let frame = client.peer.frame();
    assert_eq!(frame.0, 0x32);
    client
        .peer
        .send(vec![0x40, 4, frame.1[3], frame.1[4], 0x80, 0]);
    let mut error = ptr::null_mut();
    assert_eq!(
        unsafe { rumqttc_completion_wait_timeout_ms(completion, 3000, &raw mut error) },
        8
    );
    let mut present = 0;
    let mut reason = 0;
    assert_eq!(
        unsafe { rumqttc_error_broker_reason(error, &raw mut present, &raw mut reason) },
        0
    );
    assert_eq!((present, reason), (1, 0x80));
    unsafe {
        rumqttc_error_destroy(error);
        rumqttc_completion_destroy(completion);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    client.destroy();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    unsafe { rumqttc_publish_buffer_release(buffer) };
    wait_released(&calls);
}
