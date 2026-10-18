use super::*;
use crate::error::{INVALID_ARGUMENT, OK};

fn output() -> rumqttc_v5_publish_budget_snapshot_v2_t {
    rumqttc_v5_publish_budget_snapshot_v2_t {
        struct_size: struct_size::<rumqttc_v5_publish_budget_snapshot_v2_t>(),
        recovery_pending: 99,
        reserved: [99; 3],
        outstanding: 99,
        retained_bytes: 99,
        max_outstanding: 99,
        max_bytes: 99,
    }
}

fn read(client: &Client) -> rumqttc_v5_publish_budget_snapshot_v2_t {
    let mut out = output();
    assert_eq!(
        unsafe {
            rumqttc_client_v5_publish_budget_snapshot_v2(client.raw, &raw mut out, ptr::null_mut())
        },
        OK
    );
    assert_eq!(out.reserved, [0; 3]);
    out
}

// Call only while store/peer barriers hold accounting stable.
fn assert_projection(client: &Client, expected: (usize, usize, bool)) {
    let out = read(client);
    let core = unsafe { &*client.raw }
        .inner
        .handle
        .publish_budget_snapshot()
        .unwrap();
    assert_eq!(
        (
            out.outstanding,
            out.retained_bytes,
            out.recovery_pending != 0
        ),
        expected
    );
    assert_eq!(
        (out.outstanding, out.retained_bytes),
        (core.outstanding, core.retained_bytes)
    );
    assert_eq!(out.recovery_pending, u8::from(core.recovery_pending));
    assert_eq!(
        (out.max_outstanding, out.max_bytes),
        (core.limits.max_outstanding, core.limits.max_bytes)
    );
    let (mut count, mut bytes, mut count_limit, mut byte_limit) = (99, 99, 99, 99);
    assert_eq!(
        unsafe {
            rumqttc_client_v5_publish_budget_snapshot(
                client.raw,
                &raw mut count,
                &raw mut bytes,
                &raw mut count_limit,
                &raw mut byte_limit,
                ptr::null_mut(),
            )
        },
        OK
    );
    assert_eq!(
        (count, bytes, count_limit, byte_limit),
        (
            out.outstanding,
            out.retained_bytes,
            out.max_outstanding,
            out.max_bytes
        )
    );
}

struct Store {
    checkpoint: Mutex<Option<wc::SessionCheckpoint>>,
    held: AtomicBool,
    fail: AtomicBool,
    release: Arc<tokio::sync::Semaphore>,
    entered: mpsc::Sender<()>,
    saved: mpsc::Sender<wc::SessionCheckpoint>,
    loads: AtomicUsize,
}

impl wc::SessionStore for Store {
    fn load(&self, _: wc::SessionStoreKey) -> wc::StoreFuture<Option<wc::SessionCheckpoint>> {
        let checkpoint = self.checkpoint.lock().unwrap().clone();
        let held = self.held.swap(false, Ordering::SeqCst);
        let fail = self.fail.load(Ordering::SeqCst);
        let release = Arc::clone(&self.release);
        self.loads.fetch_add(1, Ordering::SeqCst);
        self.entered.send(()).unwrap();
        Box::pin(async move {
            if held {
                release.acquire().await.unwrap().forget();
            }
            if fail {
                Err(wc::StoreFailure::Load)
            } else {
                Ok(checkpoint)
            }
        })
    }

    fn save(
        &self,
        _: wc::SessionStoreKey,
        checkpoint: wc::SessionCheckpoint,
    ) -> wc::StoreFuture<()> {
        *self.checkpoint.lock().unwrap() = Some(checkpoint.clone());
        self.saved.send(checkpoint).unwrap();
        Box::pin(async { Ok(()) })
    }

    fn clear(&self, _: wc::SessionStoreKey) -> wc::StoreFuture<()> {
        *self.checkpoint.lock().unwrap() = None;
        Box::pin(async { Ok(()) })
    }
}

fn store() -> (
    Arc<Store>,
    mpsc::Receiver<()>,
    mpsc::Receiver<wc::SessionCheckpoint>,
) {
    let (entered, entered_rx) = mpsc::channel();
    let (saved, saved_rx) = mpsc::channel();
    (
        Arc::new(Store {
            checkpoint: Mutex::new(None),
            held: AtomicBool::new(true),
            fail: AtomicBool::new(false),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
            entered,
            saved,
            loads: AtomicUsize::new(0),
        }),
        entered_rx,
        saved_rx,
    )
}

fn stored_client(store: &Arc<Store>, shared: bool, bytes: usize) -> Client {
    Client::configured(2, shared, false, false, |config| {
        let wc::ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        v5.clean_start = false;
        v5.connect_properties.session_expiry_interval = Some(60);
        v5.session_store = Some(wc::SessionStoreConfig::new(
            store.clone(),
            "budget-snapshot",
        ));
        v5.publish_budget = wc::PublishBudgetLimits {
            max_outstanding: 1,
            max_bytes: bytes,
        };
    })
}

#[test]
fn publish_budget_snapshot_empty_restore_reconnect_and_terminal_reads() {
    for shared in [false, true] {
        let (store, entered, _saved) = store();
        let mut client = stored_client(&store, shared, 5);
        entered.recv_timeout(DEADLINE).unwrap();
        for _ in 0..8 {
            assert_projection(&client, (0, 0, true));
        }
        assert_eq!(store.loads.load(Ordering::SeqCst), 1);
        assert!(client.peer.output.try_recv().is_err());
        store.release.add_permits(1);
        client.handshake();
        assert_projection(&client, (0, 0, false));
        client.reconnect();
        assert_projection(&client, (0, 0, false));
        assert_eq!(store.loads.load(Ordering::SeqCst), 1);
        assert_eq!(
            unsafe { rumqttc_client_close_now_timeout_ms(client.raw, 3000, ptr::null_mut()) },
            OK
        );
        assert_projection(&client, (0, 0, false));
    }
}

#[test]
fn publish_budget_snapshot_failed_load_and_close_during_load_retain_gate() {
    for shared in [false, true] {
        for fail in [false, true] {
            let (store, entered, _saved) = store();
            store.fail.store(fail, Ordering::SeqCst);
            let client = stored_client(&store, shared, 5);
            entered.recv_timeout(DEADLINE).unwrap();
            assert_projection(&client, (0, 0, true));
            if fail {
                store.release.add_permits(1);
                let event = client.event();
                assert!(matches!(
                    unsafe { &*event }.inner.event,
                    wc::WrapperEvent::DriverTerminated(_)
                ));
                unsafe { rumqttc_event_destroy(event) };
            }
            assert_eq!(
                unsafe { rumqttc_client_close_now_timeout_ms(client.raw, 3000, ptr::null_mut()) },
                OK
            );
            assert_projection(&client, (0, 0, true));
        }
    }
}

#[test]
fn publish_budget_snapshot_nonempty_restore_and_budget_failure() {
    let (store, entered, saved) = store();
    let mut source = stored_client(&store, false, 5);
    entered.recv_timeout(DEADLINE).unwrap();
    store.release.add_permits(1);
    source.handshake();
    let admitted = unsafe { &*source.raw }
        .inner
        .handle
        .try_admit(wc::Command::Publish(wc::PublishCommand {
            topic: "a".into(),
            payload: Bytes::from_static(b"data"),
            qos: wc::QoS::AtLeastOnce,
            retain: false,
            protocol: wc::PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap();
    assert_eq!(source.peer.frame().0, 0x32);
    let until = std::time::Instant::now() + DEADLINE;
    let checkpoint = loop {
        let checkpoint = saved
            .recv_timeout(until.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        if checkpoint.0.windows(4).any(|bytes| bytes == b"data") {
            break checkpoint;
        }
    };
    source.destroy();
    drop(admitted);

    for shared in [false, true] {
        for bytes in [4, 5] {
            *store.checkpoint.lock().unwrap() = Some(checkpoint.clone());
            store.held.store(true, Ordering::SeqCst);
            let mut client = stored_client(&store, shared, bytes);
            entered.recv_timeout(DEADLINE).unwrap();
            assert_projection(&client, (0, 0, true));
            store.release.add_permits(1);
            if bytes == 4 {
                let event = client.event();
                let wc::WrapperEvent::DriverTerminated(error) = &unsafe { &*event }.inner.event
                else {
                    panic!("expected restoration failure");
                };
                assert_eq!(
                    error.store_failure(),
                    Some(wc::StoreFailure::PublishBudgetExceeded)
                );
                unsafe { rumqttc_event_destroy(event) };
                assert_projection(&client, (0, 0, true));
                assert_eq!(store.checkpoint.lock().unwrap().as_ref(), Some(&checkpoint));
                assert!(client.peer.output.try_recv().is_err());
            } else {
                // The CONNECT write proves preflight completed; hold CONNACK/replay progress.
                assert_eq!(client.peer.frame().0, 0x10);
                assert_projection(&client, (1, 5, false));
                client.peer.send(vec![0x20, 3, 1, 0, 0]);
                let event = client.event();
                assert!(matches!(
                    unsafe { &*event }.inner.event,
                    wc::WrapperEvent::Connected { .. }
                ));
                unsafe { rumqttc_event_destroy(event) };
                assert_eq!(client.peer.frame().0, 0x3a);
                assert_projection(&client, (1, 5, false));
            }
            client.destroy();
        }
    }
}

#[test]
fn publish_budget_snapshot_extended_records_and_protocol_failure_outputs() {
    #[repr(C)]
    struct Extended {
        record: rumqttc_v5_publish_budget_snapshot_v2_t,
        canary: [u8; 16],
    }
    for protocol in [1, 2] {
        let client = Client::new(protocol, false, false);
        let mut extended = Extended {
            record: output(),
            canary: [0xab; 16],
        };
        extended.record.struct_size = struct_size::<Extended>();
        let mut error = ptr::dangling_mut();
        let status = unsafe {
            rumqttc_client_v5_publish_budget_snapshot_v2(
                client.raw,
                &raw mut extended.record,
                &raw mut error,
            )
        };
        assert_eq!(extended.record.struct_size, struct_size::<Extended>());
        assert_eq!(extended.record.reserved, [0; 3]);
        assert_eq!(extended.canary, [0xab; 16]);
        if protocol == 1 {
            assert_eq!(status, INVALID_ARGUMENT);
            assert!(!error.is_null());
            assert_eq!(
                (
                    extended.record.outstanding,
                    extended.record.retained_bytes,
                    extended.record.max_outstanding,
                    extended.record.max_bytes,
                    extended.record.recovery_pending
                ),
                (0, 0, 0, 0, 0)
            );
            unsafe { rumqttc_error_destroy(error) };
        } else {
            assert_eq!(status, OK);
            assert!(error.is_null());
            assert_eq!(
                (extended.record.max_outstanding, extended.record.max_bytes),
                (1024, 16 * 1024 * 1024)
            );
        }
        assert_eq!(
            unsafe {
                rumqttc_client_v5_publish_budget_snapshot_v2(
                    client.raw,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            INVALID_ARGUMENT
        );
    }
}

#[test]
fn publish_budget_snapshot_null_arguments_and_short_records_initialize_only_known_prefix() {
    let mut out = output();
    assert_eq!(
        unsafe {
            rumqttc_client_v5_publish_budget_snapshot_v2(ptr::null(), &raw mut out, ptr::null_mut())
        },
        INVALID_ARGUMENT
    );
    assert_eq!(
        (
            out.recovery_pending,
            out.reserved,
            out.outstanding,
            out.retained_bytes,
            out.max_outstanding,
            out.max_bytes
        ),
        (0, [0; 3], 0, 0, 0, 0)
    );
    assert_eq!(
        out.struct_size,
        struct_size::<rumqttc_v5_publish_budget_snapshot_v2_t>()
    );
    assert_eq!(
        unsafe {
            rumqttc_client_v5_publish_budget_snapshot_v2(
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        },
        INVALID_ARGUMENT
    );
    for declared in [0_u32, 4, 8] {
        let mut short = [declared, 0xabcd, 0xabcd];
        assert_eq!(
            unsafe {
                rumqttc_client_v5_publish_budget_snapshot_v2(
                    ptr::null(),
                    short.as_mut_ptr().cast(),
                    ptr::null_mut(),
                )
            },
            INVALID_ARGUMENT
        );
        assert_eq!(
            short,
            [declared, if declared == 8 { 0 } else { 0xabcd }, 0xabcd]
        );
    }
}
