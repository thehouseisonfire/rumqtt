use super::*;
const DEADLINE: Duration = Duration::from_secs(5);
fn context() -> FileStoreContext {
    FileStoreContext::new(FileStoreContextOptions::default()).unwrap()
}
fn complete<T>(op: FileStoreOperation<T>) -> FileStoreValue<T> {
    op.wait(DEADLINE)
        .unwrap()
        .expect("operation did not finish")
        .unwrap()
}
fn open(c: &FileStoreContext, root: &std::path::Path, p: ProtocolVersion) -> FileStore {
    (*complete(
        c.open(p, root.to_owned(), FileStoreOptions::default())
            .unwrap(),
    ))
    .clone()
}
fn stop(c: &FileStoreContext) {
    c.request_shutdown().unwrap();
    assert!(c.join(DEADLINE).unwrap());
}
#[test]
fn independent_opens_are_rejected_until_engine_teardown() {
    let root = tempfile::tempdir().unwrap();
    let c = context();
    let s = open(&c, root.path(), ProtocolVersion::V4);
    let duplicate = c
        .open(
            ProtocolVersion::V4,
            root.path().join("."),
            FileStoreOptions {
                max_checkpoint_size: 1024,
                max_concurrent_operations: 1,
                ..FileStoreOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        duplicate.wait(DEADLINE).unwrap().unwrap().unwrap_err().kind,
        FileStoreFailure::InUse
    );
    #[cfg(unix)]
    {
        let aliases = tempfile::tempdir().unwrap();
        let alias = aliases.path().join("root-alias");
        std::os::unix::fs::symlink(root.path(), &alias).unwrap();
        let duplicate = c
            .open(ProtocolVersion::V4, alias, FileStoreOptions::default())
            .unwrap();
        assert_eq!(
            duplicate.wait(DEADLINE).unwrap().unwrap().unwrap_err().kind,
            FileStoreFailure::InUse
        );
    }
    let v5 = open(&c, root.path(), ProtocolVersion::V5);
    complete(s.close().unwrap());
    let reopened = open(&c, root.path(), ProtocolVersion::V4);
    assert!(s.configuration("scope".into(), DEADLINE, 1024).is_err());
    drop((s, v5, reopened));
    stop(&c);
}
#[test]
fn admission_and_administration_share_exclusion() {
    let root = tempfile::tempdir().unwrap();
    let c = context();
    let s = open(&c, root.path(), ProtocolVersion::V4);
    let key = s.key("scope".into(), "client".into()).unwrap();
    let client = s.acquire_client(&key).unwrap();
    assert!(
        matches!(s.clear("scope".into(), "client".into()), Err(e) if e.kind == FileStoreFailure::InUse)
    );
    assert!(matches!(s.close(), Err(e) if e.kind == FileStoreFailure::InUse));
    assert!(matches!(c.request_shutdown(), Err(e) if e.kind == FileStoreFailure::InUse));
    drop(client);
    complete(s.clear("scope".into(), "client".into()).unwrap());
    complete(s.close().unwrap());
    stop(&c);
}
#[test]
fn observer_timeout_and_release_do_not_retire_pending_key_work() {
    let root = tempfile::tempdir().unwrap();
    let c = context();
    let s = open(&c, root.path(), ProtocolVersion::V5);
    let key = s.key("scope".into(), "client".into()).unwrap();
    let client = s.acquire_client(&key).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let op = s
        .submit(Some(key.clone()), false, false, 0, move |_| {
            rx.recv_timeout(DEADLINE).unwrap();
            Ok(())
        })
        .unwrap();
    assert!(op.wait(Duration::ZERO).unwrap().is_none());
    drop(op);
    drop(client);
    assert!(s.acquire_client(&key).is_err());
    assert!(s.clear("scope".into(), "client".into()).is_err());
    let closed = s.close().unwrap();
    assert!(s.acquire_client(&key).is_err());
    tx.send(()).unwrap();
    complete(closed);
    stop(&c);
}
#[test]
fn failed_open_and_shutdown_are_joinable_with_observers_retained() {
    let root = tempfile::tempdir().unwrap();
    let c = context();
    let op = c
        .open(
            ProtocolVersion::V4,
            root.path().join("missing"),
            FileStoreOptions::default(),
        )
        .unwrap();
    assert!(op.wait(DEADLINE).unwrap().unwrap().is_err());
    stop(&c);
    assert!(op.poll().unwrap().is_err());
}
#[test]
fn context_shutdown_drains_an_open_still_initializing() {
    for _ in 0..20 {
        let root = tempfile::tempdir().unwrap();
        let c = context();
        let op = c
            .open(
                ProtocolVersion::V4,
                root.path().to_owned(),
                FileStoreOptions::default(),
            )
            .unwrap();
        stop(&c);
        if let Some(Ok(s)) = op.poll() {
            assert!(s.configuration("scope".into(), DEADLINE, 1024).is_err());
        }
    }
}
#[test]
fn observations_and_payload_reservations_are_bounded() {
    let c = FileStoreContext::new(FileStoreContextOptions {
        observation_capacity: 1,
        byte_capacity: 8,
        ..FileStoreContextOptions::default()
    })
    .unwrap();
    let op = c.operation(8, || Ok(())).unwrap();
    complete(op.clone());
    assert!(matches!(c.operation(1, || Ok(())), Err(e) if e.kind == FileStoreFailure::Resources));
    drop(op);
    // Completion can be observed before the worker releases its last outcome
    // reference. Capacity becomes available when both owners have retired.
    let start = Instant::now();
    loop {
        match c.operation(8, || Ok(())) {
            Ok(op) => {
                complete(op);
                break;
            }
            Err(e) => {
                assert_eq!(e.kind, FileStoreFailure::Resources);
                assert!(start.elapsed() < DEADLINE);
                std::thread::yield_now();
            }
        }
    }
    stop(&c);
}
#[test]
fn stores_interoperate_with_native_rust_and_administration() {
    use rumqttc_v4::{PersistedAckMode, PersistedSession};
    let root = tempfile::tempdir().unwrap();
    let native = disk::v4::BlockingSessionFileStore::open(root.path()).unwrap();
    let key = rumqttc_v4::SessionStoreKey::new("scope", "client");
    let session = PersistedSession {
        format_version: 2,
        client_id: "client".into(),
        clean_session: false,
        max_inflight: 10,
        ack_mode: PersistedAckMode::Automatic,
        replay: vec![],
        incoming_qos2: vec![],
    };
    native.save(&key, &session).unwrap();
    native.close().unwrap();
    let c = context();
    let s = open(&c, root.path(), ProtocolVersion::V4);
    let config = s
        .configuration("scope".into(), DEADLINE, 16 * 1024 * 1024)
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let skey = s.key("scope".into(), "client".into()).unwrap();
    let checkpoint = runtime
        .block_on(config.store.load(skey.clone()))
        .unwrap()
        .unwrap();
    assert_eq!(
        crate::backend::session::payload(checkpoint.clone(), 4, 16 * 1024 * 1024)
            .unwrap()
            .as_ref(),
        session.encode().unwrap()
    );
    let limited = s.configuration("scope".into(), DEADLINE, 8).unwrap();
    assert_eq!(
        runtime
            .block_on(limited.store.load_with_limit(skey.clone(), 8))
            .unwrap_err(),
        StoreFailure::Oversized
    );
    assert_eq!(
        runtime
            .block_on(
                limited
                    .store
                    .save_with_limit(skey.clone(), checkpoint.clone(), 8)
            )
            .unwrap_err(),
        StoreFailure::Oversized
    );
    runtime
        .block_on(config.store.save(skey, checkpoint))
        .unwrap();
    assert!(matches!(
        &*complete(s.inspect("scope".into(), "client".into()).unwrap()),
        FileStoreResult::Inspection { present: true, .. }
    ));
    assert!(
        matches!(&*complete(s.quarantine("scope".into(), "client".into()).unwrap()), FileStoreResult::Quarantined { path, .. } if path.exists())
    );
    assert!(matches!(
        &*complete(s.inspect("scope".into(), "client".into()).unwrap()),
        FileStoreResult::Inspection { present: false, .. }
    ));
    complete(s.close().unwrap());
    stop(&c);
    let native = disk::v4::BlockingSessionFileStore::open(root.path()).unwrap();
    assert!(native.load(&key).unwrap().is_none());
    native.close().unwrap();
}
#[test]
fn nonzero_observation_waits_reject_workers() {
    let c = context();
    let op = c.operation(0, || Ok(())).unwrap();
    complete(op.clone());
    crate::execution::with_managed_thread(|| {
        assert!(op.wait(DEADLINE).is_err());
        assert!(op.wait(Duration::ZERO).unwrap().is_some());
    });
    stop(&c);
}

#[test]
fn last_release_racing_context_shutdown_keeps_resources_joinable() {
    for _ in 0..50 {
        let root = tempfile::tempdir().unwrap();
        let c = context();
        let store = open(&c, root.path(), ProtocolVersion::V4);
        let released = std::thread::spawn(move || drop(store));
        stop(&c);
        released.join().unwrap();
        let fresh = context();
        let reopened = open(&fresh, root.path(), ProtocolVersion::V4);
        complete(reopened.close().unwrap());
        stop(&fresh);
    }
}
#[test]
fn one_worker_drains_prior_work_and_repeated_close_shares_observation() {
    let root = tempfile::tempdir().unwrap();
    let c = FileStoreContext::new(FileStoreContextOptions {
        worker_threads: 1,
        ..FileStoreContextOptions::default()
    })
    .unwrap();
    let store = open(&c, root.path(), ProtocolVersion::V5);
    let (tx, rx) = std::sync::mpsc::channel();
    let work = store
        .submit(None, false, false, 0, move |_| {
            rx.recv_timeout(DEADLINE).unwrap();
            Ok(())
        })
        .unwrap();
    let close = store.close().unwrap();
    let second = store.close().unwrap();
    assert!(Arc::ptr_eq(&close.0, &second.0));
    assert!(store.inspect("scope".into(), "client".into()).is_err());
    tx.send(()).unwrap();
    complete(work);
    complete(close);
    complete(store.close().unwrap());
    assert!(store.configuration("scope".into(), DEADLINE, 1024).is_err());
    stop(&c);
}
#[test]
fn allocation_rejection_releases_the_key_reservation() {
    let root = tempfile::tempdir().unwrap();
    let c = FileStoreContext::new(FileStoreContextOptions {
        byte_capacity: 1024,
        ..FileStoreContextOptions::default()
    })
    .unwrap();
    let store = open(&c, root.path(), ProtocolVersion::V4);
    let key = store.key("scope".into(), "client".into()).unwrap();
    assert!(
        matches!(store.submit(Some(key.clone()), true, false, 2048, |_| Ok(())), Err(e) if e.kind == FileStoreFailure::Resources)
    );
    let lease = store.acquire_client(&key).unwrap();
    drop(lease);
    complete(store.close().unwrap());
    stop(&c);
}

#[test]
fn small_sessions_do_not_reserve_the_maximum_checkpoint_for_each_client() {
    let root = tempfile::tempdir().unwrap();
    let c = FileStoreContext::new(FileStoreContextOptions {
        byte_capacity: 4 * 1024 * 1024,
        ..FileStoreContextOptions::default()
    })
    .unwrap();
    let store = open(&c, root.path(), ProtocolVersion::V5);
    let config = store
        .configuration("scope".into(), DEADLINE, 16 * 1024 * 1024)
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let futures = (0..64).map(|id| {
            config.store.load(SessionStoreKey {
                protocol: ProtocolVersion::V5,
                scope: "scope".into(),
                client_id: format!("client-{id}"),
            })
        });
        for value in futures_util::future::join_all(futures).await {
            assert!(value.unwrap().is_none());
        }
    });
    complete(store.close().unwrap());
    stop(&c);
}
#[test]
fn file_error_details_survive_error_projection_and_replacement() {
    assert!(std::mem::size_of::<crate::Error>() < 128);
    let details = io_error(&std::io::Error::from_raw_os_error(13), 17);
    let failure = crate::Error::store(details.store());
    assert_eq!(failure.store_failure(), Some(StoreFailure::File(details)));
    assert_eq!(
        failure.diagnostic_copy().store_failure(),
        failure.store_failure()
    );
    assert_eq!(
        failure
            .with_store_failure(StoreFailure::Load)
            .store_failure(),
        Some(StoreFailure::Load)
    );
}
#[test]
fn wrong_protocol_attachment_fails_without_mutating_the_store() {
    let root = tempfile::tempdir().unwrap();
    let c = context();
    let store = open(&c, root.path(), ProtocolVersion::V4);
    let config = store.configuration("scope".into(), DEADLINE, 1024).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(
        runtime
            .block_on(config.store.load(SessionStoreKey {
                protocol: ProtocolVersion::V5,
                scope: "scope".into(),
                client_id: "client".into()
            }))
            .unwrap_err(),
        StoreFailure::Protocol
    );
    complete(store.close().unwrap());
    stop(&c);
}

#[test]
fn cleanup_preserves_the_namespace_barrier_and_platform_contract() {
    let root = tempfile::tempdir().unwrap();
    let c = context();
    let store = open(&c, root.path(), ProtocolVersion::V4);
    let key = store.key("scope".into(), "client".into()).unwrap();
    let client = store.acquire_client(&key).unwrap();
    assert!(
        matches!(store.cleanup(Duration::from_secs(1)), Err(e) if e.kind == FileStoreFailure::InUse)
    );
    drop(client);

    // Hold maintenance authority while racing client and operation admission.
    let lease = store.reserve(None, true, true).unwrap();
    assert!(store.acquire_client(&key).is_err());
    assert!(store.inspect("scope".into(), "client".into()).is_err());
    drop(lease);
    let result = store
        .cleanup(Duration::from_secs(1))
        .unwrap()
        .wait(DEADLINE)
        .unwrap()
        .unwrap();
    #[cfg(windows)]
    assert!(matches!(&*result.unwrap(), FileStoreResult::Cleanup { .. }));
    #[cfg(not(windows))]
    assert_eq!(result.unwrap_err().kind, FileStoreFailure::Unsupported);
    complete(store.close().unwrap());
    stop(&c);
}

#[test]
fn malformed_checkpoints_are_typed_and_left_untouched_for_operator_recovery() {
    use rumqttc_v4::{PersistedAckMode, PersistedSession};
    for (change, expected) in [
        (0, StoreFailure::Corrupt),
        (1, StoreFailure::Version),
        (2, StoreFailure::Protocol),
    ] {
        let root = tempfile::tempdir().unwrap();
        let native = disk::v4::BlockingSessionFileStore::open(root.path()).unwrap();
        let key = rumqttc_v4::SessionStoreKey::new("scope", "client");
        let session = PersistedSession {
            format_version: 2,
            client_id: "client".into(),
            clean_session: false,
            max_inflight: 10,
            ack_mode: PersistedAckMode::Automatic,
            replay: vec![],
            incoming_qos2: vec![],
        };
        native.save(&key, &session).unwrap();
        let path = native.checkpoint_path(&key).unwrap();
        native.close().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        match change {
            0 => bytes[18] ^= 1,
            1 => bytes[26] = 42,
            _ => bytes[25] = 5,
        }
        if change != 0 {
            let n = bytes.len() - 4;
            let checksum = crc32c::crc32c(&bytes[..n]);
            bytes[n..].copy_from_slice(&checksum.to_be_bytes());
        }
        std::fs::write(&path, &bytes).unwrap();
        let c = context();
        let store = open(&c, root.path(), ProtocolVersion::V4);
        let config = store.configuration("scope".into(), DEADLINE, 1024).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(
            runtime
                .block_on(
                    config
                        .store
                        .load(store.key("scope".into(), "client".into()).unwrap())
                )
                .unwrap_err(),
            expected
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let result = complete(store.quarantine("scope".into(), "client".into()).unwrap());
        assert!(
            matches!(&*result, FileStoreResult::Quarantined { path, .. } if std::fs::read(path).unwrap() == bytes)
        );
        complete(store.close().unwrap());
        stop(&c);
    }
}
#[test]
fn recovery_clear_uses_the_client_authority_while_operator_clear_stays_excluded() {
    let root = tempfile::tempdir().unwrap();
    let c = context();
    let store = open(&c, root.path(), ProtocolVersion::V5);
    let config = store.configuration("scope".into(), DEADLINE, 1024).unwrap();
    let key = store.key("scope".into(), "client".into()).unwrap();
    let lease = store.acquire_client(&key).unwrap();
    assert!(store.clear("scope".into(), "client".into()).is_err());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(config.store.clear(key)).unwrap();
    assert!(store.clear("scope".into(), "client".into()).is_err());
    drop(lease);
    complete(store.close().unwrap());
    stop(&c);
}
#[test]
fn slow_file_execution_yields_to_the_managed_polling_runtime() {
    let root = tempfile::tempdir().unwrap();
    let c = FileStoreContext::new(FileStoreContextOptions {
        worker_threads: 1,
        ..FileStoreContextOptions::default()
    })
    .unwrap();
    let store = open(&c, root.path(), ProtocolVersion::V4);
    let config = store.configuration("scope".into(), DEADLINE, 1024).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let blocked = c
        .operation(0, move || {
            rx.recv_timeout(DEADLINE).unwrap();
            Ok(())
        })
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let start = Instant::now();
    runtime.block_on(async {
        let load = config
            .store
            .load(store.key("scope".into(), "client".into()).unwrap());
        let peer = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send(()).unwrap();
        };
        let (loaded, ()) = tokio::join!(load, peer);
        assert!(loaded.unwrap().is_none());
    });
    assert!(start.elapsed() < Duration::from_secs(1));
    complete(blocked);
    complete(store.close().unwrap());
    stop(&c);
}

#[test]
fn rejected_open_admission_does_not_leave_a_store_or_initialization_reservation() {
    let root = tempfile::tempdir().unwrap();
    let c = FileStoreContext::new(FileStoreContextOptions {
        operation_capacity: 1,
        store_capacity: 1,
        ..FileStoreContextOptions::default()
    })
    .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let blocked = c
        .operation(0, move || {
            rx.recv_timeout(DEADLINE).unwrap();
            Ok(())
        })
        .unwrap();
    for _ in 0..20 {
        assert!(
            matches!(c.open(ProtocolVersion::V4, root.path().to_owned(), FileStoreOptions::default()), Err(e) if e.kind == FileStoreFailure::Resources)
        );
    }
    assert!(lock(&c.0.shared.counts).stores.is_empty());
    tx.send(()).unwrap();
    complete(blocked);
    let store = open(&c, root.path(), ProtocolVersion::V4);
    for _ in 0..20 {
        assert!(
            matches!(c.open(ProtocolVersion::V5, root.path().to_owned(), FileStoreOptions::default()), Err(e) if e.kind == FileStoreFailure::Resources)
        );
    }
    complete(store.close().unwrap());
    stop(&c);
}

#[test]
fn context_shutdown_rejects_configuration_before_its_close_worker_runs() {
    let root = tempfile::tempdir().unwrap();
    let c = FileStoreContext::new(FileStoreContextOptions {
        worker_threads: 1,
        ..FileStoreContextOptions::default()
    })
    .unwrap();
    let store = open(&c, root.path(), ProtocolVersion::V4);
    let (tx, rx) = std::sync::mpsc::channel();
    let blocked = c
        .operation(0, move || {
            rx.recv_timeout(DEADLINE).unwrap();
            Ok(())
        })
        .unwrap();
    c.request_shutdown().unwrap();
    assert!(store.configuration("scope".into(), DEADLINE, 1024).is_err());
    assert!(!c.join(Duration::ZERO).unwrap());
    tx.send(()).unwrap();
    complete(blocked);
    assert!(c.join(DEADLINE).unwrap());
}

#[test]
fn exported_results_keep_bytes_reserved_after_operation_and_worker_exit() {
    let c = FileStoreContext::new(FileStoreContextOptions {
        byte_capacity: 4096,
        ..FileStoreContextOptions::default()
    })
    .unwrap();
    let op = c.operation(4096, || Ok(vec![0u8; 4096])).unwrap();
    let waited = op.wait(DEADLINE).unwrap().unwrap().unwrap();
    let polled = op.poll().unwrap().unwrap();
    let retained = polled.clone();
    drop(op);
    assert!(matches!(
        c.operation(1, || Ok(())),
        Err(e) if e.kind == FileStoreFailure::Resources
    ));
    // Joining removes worker ownership, making reservation lifetime deterministic.
    stop(&c);
    assert_eq!(lock(&c.0.shared.counts).observations, 0);
    assert_eq!(lock(&c.0.shared.counts).bytes, 4096);
    assert_eq!(retained.len(), 4096);
    drop(waited);
    drop(polled);
    assert_eq!(lock(&c.0.shared.counts).bytes, 4096);
    drop(retained);
    assert_eq!(lock(&c.0.shared.counts).bytes, 0);
}

#[test]
fn long_administrative_keys_reserve_the_full_peak_before_execution() {
    for protocol in [ProtocolVersion::V4, ProtocolVersion::V5] {
        let root = tempfile::tempdir().unwrap();
        let c = FileStoreContext::new(FileStoreContextOptions {
            byte_capacity: 5 * 1024 * 1024 + 512 * 1024,
            ..FileStoreContextOptions::default()
        })
        .unwrap();
        let store = open(&c, root.path(), protocol);
        let scope = "s".repeat(1024 * 1024);
        for op in [
            store.clear(scope.clone(), "client".into()),
            store.quarantine(scope.clone(), "client".into()),
        ] {
            assert!(matches!(op, Err(e) if e.kind == FileStoreFailure::Resources));
        }
        // Rejected admission releases exclusive authority; inspection needs only
        // five simultaneous copies and fits the same budget.
        assert!(matches!(
            *complete(store.inspect(scope, "client".into()).unwrap()),
            FileStoreResult::Inspection { present: false, .. }
        ));
        complete(store.close().unwrap());
        stop(&c);
    }
}
