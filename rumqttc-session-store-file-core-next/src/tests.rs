use std::io::{Cursor, Read};

use super::*;

const TEST_MAXIMUM: u64 = 1024;

fn envelope(payload: &[u8]) -> Vec<u8> {
    encode_envelope(payload, TEST_MAXIMUM).unwrap()
}

#[test]
fn golden_envelope_is_stable_and_big_endian() {
    let actual = envelope(b"abc");
    assert_eq!(
        actual,
        [
            b'R', b'U', b'M', b'Q', b'S', b'E', b'S', b'S', 0, 1, 0, 0, 0, 0, 0, 0, 0, 3, b'a',
            b'b', b'c', 0x36, 0xbd, 0xfa, 0x05,
        ]
    );
}

#[test]
fn envelope_round_trip_and_empty_payload() {
    for payload in [b"payload".as_slice(), b"".as_slice()] {
        let bytes = envelope(payload);
        assert_eq!(
            decode_reader(&mut Cursor::new(bytes), TEST_MAXIMUM).unwrap(),
            payload
        );
    }
}

#[test]
fn envelope_rejects_magic_version_checksum_and_trailing_data() {
    let mut invalid_magic = envelope(b"x");
    invalid_magic[0] ^= 1;
    assert!(matches!(
        decode_reader(&mut Cursor::new(invalid_magic), TEST_MAXIMUM),
        Err(FileStoreError::InvalidEnvelopeMagic)
    ));

    let mut invalid_version = envelope(b"x");
    invalid_version[9] = 2;
    assert!(matches!(
        decode_reader(&mut Cursor::new(invalid_version), TEST_MAXIMUM),
        Err(FileStoreError::UnsupportedEnvelopeVersion { found: 2 })
    ));

    let mut invalid_checksum = envelope(b"x");
    invalid_checksum[18] ^= 1;
    assert!(matches!(
        decode_reader(&mut Cursor::new(invalid_checksum), TEST_MAXIMUM),
        Err(FileStoreError::ChecksumMismatch { .. })
    ));

    for suffix in [vec![1], vec![1, 2, 3]] {
        let mut trailing = envelope(b"x");
        trailing.extend_from_slice(&suffix);
        assert!(matches!(
            decode_reader(&mut Cursor::new(trailing), TEST_MAXIMUM),
            Err(FileStoreError::TrailingData)
        ));
    }
}

#[test]
fn every_envelope_section_reports_truncation() {
    let bytes = envelope(b"abc");
    let cases = [
        (0, EnvelopeSection::Magic),
        (7, EnvelopeSection::Magic),
        (8, EnvelopeSection::Version),
        (9, EnvelopeSection::Version),
        (10, EnvelopeSection::PayloadLength),
        (17, EnvelopeSection::PayloadLength),
        (18, EnvelopeSection::Payload),
        (20, EnvelopeSection::Payload),
        (21, EnvelopeSection::Checksum),
        (24, EnvelopeSection::Checksum),
    ];
    for (length, section) in cases {
        assert!(matches!(
            decode_reader(&mut Cursor::new(&bytes[..length]), TEST_MAXIMUM),
            Err(FileStoreError::TruncatedEnvelope { section: found }) if found == section
        ));
    }
}

#[test]
fn declared_size_is_checked_before_payload_read_or_allocation() {
    let mut bytes = Vec::from(*MAGIC);
    bytes.extend_from_slice(&ENVELOPE_VERSION.to_be_bytes());
    bytes.extend_from_slice(&(TEST_MAXIMUM + 1).to_be_bytes());
    let mut reader = CountingReader::new(bytes);
    assert!(matches!(
        decode_reader(&mut reader, TEST_MAXIMUM),
        Err(FileStoreError::CheckpointTooLarge {
            size: 1025,
            maximum: TEST_MAXIMUM
        })
    ));
    assert_eq!(reader.bytes_read, HEADER_LEN);
}

#[test]
fn declared_size_above_target_usize_is_rejected_before_allocation() {
    let mut bytes = Vec::from(*MAGIC);
    bytes.extend_from_slice(&ENVELOPE_VERSION.to_be_bytes());
    bytes.extend_from_slice(&17_u64.to_be_bytes());
    let mut reader = CountingReader::new(bytes);
    assert!(matches!(
        decode_reader_with_usize_limit(&mut reader, TEST_MAXIMUM, 16),
        Err(FileStoreError::InvalidPayloadLength { declared: 17 })
    ));
    assert_eq!(reader.bytes_read, HEADER_LEN);
}

#[test]
fn maximum_size_boundary_is_accepted_and_save_rejects_above_it() {
    let maximum = usize::try_from(TEST_MAXIMUM).unwrap();
    let payload = vec![7; maximum];
    let bytes = encode_envelope(&payload, TEST_MAXIMUM).unwrap();
    assert_eq!(
        decode_reader(&mut Cursor::new(bytes), TEST_MAXIMUM).unwrap(),
        payload
    );
    assert!(matches!(
        encode_envelope(&vec![0; maximum + 1], TEST_MAXIMUM),
        Err(FileStoreError::CheckpointTooLarge { .. })
    ));
}

#[test]
fn checksum_covers_header_and_payload() {
    for index in 0..21 {
        let mut bytes = envelope(b"abc");
        if index < 8 {
            continue; // Magic has its own more precise error.
        }
        if (8..10).contains(&index) {
            continue; // Version has its own more precise error.
        }
        if (10..18).contains(&index) {
            continue; // Length mutation changes structural interpretation.
        }
        bytes[index] ^= 1;
        assert!(matches!(
            decode_reader(&mut Cursor::new(bytes), TEST_MAXIMUM),
            Err(FileStoreError::ChecksumMismatch { .. })
        ));
    }

    let bytes = envelope(b"abc");
    assert_eq!(
        u32::from_be_bytes(bytes[21..25].try_into().unwrap()),
        crc32c::crc32c(&bytes[..21])
    );
}

#[test]
fn bounded_reader_consumes_only_declared_payload_checksum_and_one_probe() {
    let mut bytes = envelope(b"abc");
    bytes.extend(std::iter::repeat_n(9, 10_000));
    let mut reader = CountingReader::new(bytes);
    assert!(matches!(
        decode_reader(&mut reader, TEST_MAXIMUM),
        Err(FileStoreError::TrailingData)
    ));
    assert_eq!(reader.bytes_read, HEADER_LEN + 3 + CHECKSUM_LEN + 1);
    assert_eq!(reader.largest_request, 8);
}

#[test]
fn configured_maximum_must_fit_an_envelope_allocation() {
    assert!(matches!(
        validate_maximum(u64::MAX),
        Err(FileStoreError::InvalidMaximumCheckpointSize { .. })
    ));
    validate_maximum(0).unwrap();
}

struct CountingReader {
    bytes: Cursor<Vec<u8>>,
    bytes_read: usize,
    largest_request: usize,
}

impl CountingReader {
    const fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Cursor::new(bytes),
            bytes_read: 0,
            largest_request: 0,
        }
    }
}

impl Read for CountingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.largest_request = self.largest_request.max(buffer.len());
        let read = self.bytes.read(buffer)?;
        self.bytes_read += read;
        Ok(read)
    }
}

#[test]
fn filename_is_full_lowercase_blake3_and_contains_no_key_text() {
    let filename = checkpoint_filename(b"../client/name\\CON:");
    assert_eq!(filename.len(), 64 + ".session".len());
    assert!(filename.ends_with(".session"));
    assert!(
        filename[..64]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
    assert!(!filename.contains("client"));
    assert!(!filename.contains('/'));
    assert!(!filename.contains('\\'));
}

#[cfg(windows)]
#[tokio::test]
async fn windows_native_save_replace_inspect_quarantine_clear_and_owned_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let unicode_root = root.path().join("sessões-客户端");
    std::fs::create_dir(&unicode_root).unwrap();
    let store = FileStore::open(&unicode_root, "v5", FileStoreOptions::default())
        .await
        .unwrap();
    let key = "ключ/客户端".as_bytes();
    store.save(key, b"old".to_vec()).await.unwrap();
    store.save(key, b"new".to_vec()).await.unwrap();
    assert_eq!(store.load(key).await.unwrap(), Some(b"new".to_vec()));
    assert_eq!(
        store.inspect(key).await.unwrap().state,
        CheckpointState::Present
    );
    let quarantine = store.quarantine(key).await.unwrap();
    assert!(quarantine.diagnostic_path.is_file());
    assert_eq!(store.load(key).await.unwrap(), None);
    store.clear(key).await.unwrap();

    let hash = "0".repeat(64);
    let owned = unicode_root
        .join("v5")
        .join(format!("{hash}.session.tmp-v1.clear.{}", "1".repeat(64)));
    let unrelated = unicode_root.join("v5").join("unrelated.tmp");
    std::fs::write(&owned, b"owned").unwrap();
    std::fs::write(&unrelated, b"unrelated").unwrap();
    let report = store
        .cleanup_stale_temporary_files_before_use(Duration::from_secs(3600))
        .await
        .unwrap();
    assert_eq!(
        report.skipped,
        vec![owned.file_name().unwrap().to_string_lossy()]
    );
    assert!(unrelated.is_file());
}

#[cfg(windows)]
#[tokio::test]
async fn windows_new_clear_staging_uses_the_clear_time_for_cleanup_age() {
    use windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;

    let root = tempfile::tempdir().unwrap();
    let store = FileStore::open(root.path(), "v4", FileStoreOptions::default())
        .await
        .unwrap();
    let canonical = store.checkpoint_path(b"old-checkpoint");
    std::fs::write(&canonical, b"old checkpoint").unwrap();
    let old = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
    let file = std::fs::File::options()
        .write(true)
        .open(&canonical)
        .unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(old))
        .unwrap();
    drop(file);

    refresh_windows_clear_age(&canonical).unwrap();
    let hash = canonical.file_stem().unwrap().to_string_lossy();
    let staging = root
        .path()
        .join("v4")
        .join(format!("{hash}.session.tmp-v1.clear.{}", "1".repeat(64)));
    move_file(&canonical, &staging, MOVEFILE_WRITE_THROUGH).unwrap();

    let report = store
        .cleanup_stale_temporary_files_before_use(Duration::from_secs(60 * 60))
        .await
        .unwrap();
    assert!(report.removed.is_empty());
    assert_eq!(
        report.skipped,
        vec![staging.file_name().unwrap().to_string_lossy()]
    );
    assert!(staging.is_file());
}

#[cfg(windows)]
#[tokio::test]
async fn windows_quarantine_does_not_report_a_missing_namespace_as_a_missing_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let store = FileStore::open(root.path(), "v4", FileStoreOptions::default())
        .await
        .unwrap();
    assert!(matches!(
        store.quarantine(b"absent-key").await,
        Err(FileStoreError::QuarantineSourceMissing)
    ));
    std::fs::remove_dir(root.path().join("v4")).unwrap();

    assert!(matches!(
        store.quarantine(b"absent-key").await,
        Err(FileStoreError::Io {
            operation: FileOperation::InspectNamespace,
            source,
        }) if source.kind() == io::ErrorKind::NotFound
    ));
}

#[cfg(windows)]
#[test]
fn windows_relative_root_child() {
    let Some(root) = std::env::var_os("RUMQTTC_WINDOWS_RELATIVE_ROOT_CHILD") else {
        return;
    };
    std::env::set_current_dir(root).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let store = FileStore::open(
            Path::new(".").join("unused").join(".."),
            "v4",
            FileStoreOptions::default(),
        )
        .await
        .unwrap();
        let checkpoint = store.checkpoint_path(b"relative-key");
        assert!(checkpoint.is_absolute());
        assert!(
            !checkpoint
                .components()
                .any(|component| { matches!(component, Component::CurDir | Component::ParentDir) })
        );

        store.save(b"relative-key", b"old".to_vec()).await.unwrap();
        store.save(b"relative-key", b"new".to_vec()).await.unwrap();
        assert_eq!(
            store.load(b"relative-key").await.unwrap(),
            Some(b"new".to_vec())
        );
        store.quarantine(b"relative-key").await.unwrap();
        store
            .save(b"relative-key", b"clear-me".to_vec())
            .await
            .unwrap();
        store.clear(b"relative-key").await.unwrap();
        assert_eq!(store.load(b"relative-key").await.unwrap(), None);
    });
}

#[cfg(windows)]
#[test]
fn windows_relative_root_support_is_exercised_in_an_isolated_process() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("unused")).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::windows_relative_root_child",
            "--nocapture",
        ])
        .env("RUMQTTC_WINDOWS_RELATIVE_ROOT_CHILD", root.path())
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(windows)]
#[test]
fn windows_extended_paths_preserve_non_unicode_wide_units() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    const BS: u16 = 92;

    let source = [
        u16::from(b'C'),
        u16::from(b':'),
        u16::from(b'/'),
        0xd800,
        u16::from(b'/'),
        u16::from(b'f'),
    ];
    let path = PathBuf::from(OsString::from_wide(&source));
    assert_eq!(
        wide_path(&path),
        [
            BS,
            BS,
            u16::from(b'?'),
            BS,
            u16::from(b'C'),
            u16::from(b':'),
            BS,
            0xd800,
            BS,
            u16::from(b'f'),
            0,
        ]
    );

    let unc = PathBuf::from(OsString::from_wide(&[BS, BS, u16::from(b's'), 0xdfff]));
    let encoded = wide_path(&unc);
    assert_eq!(
        &encoded[..8],
        &[
            BS,
            BS,
            u16::from(b'?'),
            BS,
            u16::from(b'U'),
            u16::from(b'N'),
            u16::from(b'C'),
            BS,
        ]
    );
    assert_eq!(&encoded[8..], &[u16::from(b's'), 0xdfff, 0]);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn save_load_replace_clear_and_missing_are_complete() {
    let root = tempfile::tempdir().unwrap();
    let store = FileStore::open(root.path(), "v4", FileStoreOptions::default())
        .await
        .unwrap();
    let key = b"key";
    assert_eq!(store.load(key).await.unwrap(), None);

    store.save(key, b"old".to_vec()).await.unwrap();
    assert_eq!(store.load(key).await.unwrap(), Some(b"old".to_vec()));
    store.save(key, b"new".to_vec()).await.unwrap();
    assert_eq!(store.load(key).await.unwrap(), Some(b"new".to_vec()));
    store.clear(key).await.unwrap();
    store.clear(key).await.unwrap();
    assert_eq!(store.load(key).await.unwrap(), None);
}

#[cfg(unix)]
#[tokio::test]
async fn inspection_quarantine_and_exact_legacy_detection_are_non_destructive() {
    let root = tempfile::tempdir().unwrap();
    let store = FileStore::open(root.path(), "v4", FileStoreOptions::default())
        .await
        .unwrap();
    let key = b"inspection-key";
    assert_eq!(
        store.inspect(key).await.unwrap().state,
        CheckpointState::Absent
    );

    std::fs::write(root.path().join("exact.legacy.session"), b"legacy").unwrap();
    assert!(matches!(
        store
            .load_with_legacy_filename(key, "exact.legacy.session")
            .await
            .unwrap(),
        LegacyLoad::LegacyDetected { .. }
    ));
    assert!(matches!(
        store
            .load_with_legacy_filename(key, "../exact.legacy.session")
            .await,
        Err(FileStoreError::InvalidLegacyFilename)
    ));

    store.save(key, b"canonical".to_vec()).await.unwrap();
    let inspection = store.inspect(key).await.unwrap();
    assert_eq!(inspection.state, CheckpointState::Present);
    assert!(inspection.size.unwrap() > b"canonical".len() as u64);
    assert!(matches!(
        store.load_with_legacy_filename(key, "exact.legacy.session").await.unwrap(),
        LegacyLoad::Present(payload) if payload == b"canonical"
    ));

    let quarantine = store.quarantine(key).await.unwrap();
    assert_eq!(quarantine.identifier.len(), 64);
    assert!(quarantine.diagnostic_path.is_file());
    assert_eq!(
        store.inspect(key).await.unwrap().state,
        CheckpointState::Absent
    );
    assert!(matches!(
        store.quarantine(key).await,
        Err(FileStoreError::QuarantineSourceMissing)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn quarantine_sync_failure_preserves_the_committed_destination() {
    let root = tempfile::tempdir().unwrap();
    let initial = FileStore::open(root.path(), "v4", FileStoreOptions::default())
        .await
        .unwrap();
    let key = b"quarantine-sync-failure";
    initial
        .save(key, b"diagnostic payload".to_vec())
        .await
        .unwrap();
    drop(initial);

    let hook = Arc::new(|stage| {
        if stage == TestStage::BeforeDirectorySync {
            Err(io::Error::other("injected quarantine sync failure"))
        } else {
            Ok(())
        }
    });
    let store =
        FileStore::open_with_test_hook(root.path(), "v4", FileStoreOptions::default(), hook)
            .await
            .unwrap();
    let error = store.quarantine(key).await.unwrap_err();
    let FileStoreError::QuarantineNamespaceSync { quarantine, source } = error else {
        panic!("unexpected quarantine error: {error:?}");
    };
    assert_eq!(source.to_string(), "injected quarantine sync failure");
    assert!(quarantine.diagnostic_path.is_file());
    assert_eq!(
        store.inspect(key).await.unwrap().state,
        CheckpointState::Absent
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unix_cleanup_is_validated_and_explicitly_unsupported() {
    let root = tempfile::tempdir().unwrap();
    let store = FileStore::open(root.path(), "v4", FileStoreOptions::default())
        .await
        .unwrap();
    assert!(matches!(
        store
            .cleanup_stale_temporary_files_before_use(Duration::ZERO)
            .await,
        Err(FileStoreError::InvalidCleanupAge)
    ));
    assert!(matches!(
        store
            .cleanup_stale_temporary_files_before_use(Duration::from_secs(1))
            .await,
        Err(FileStoreError::CleanupUnsupported { .. })
    ));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_maintenance_barrier_waits_for_earlier_work_and_blocks_later_dispatch() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let root = tempfile::tempdir().unwrap();
    let (stage_sender, stage_receiver) = std::sync::mpsc::channel();
    let stage_receiver = Arc::new(std::sync::Mutex::new(stage_receiver));
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let release_receiver = std::sync::Mutex::new(release_receiver);
    let envelope_count = AtomicUsize::new(0);
    let hook = Arc::new(move |stage| {
        if stage == TestStage::BeforeEnvelope {
            envelope_count.fetch_add(1, Ordering::SeqCst);
            stage_sender.send(()).unwrap();
        }
        if stage == TestStage::BeforeCommit && envelope_count.load(Ordering::SeqCst) == 1 {
            release_receiver.lock().unwrap().recv().unwrap();
        }
        Ok(())
    });
    let store =
        FileStore::open_with_test_hook(root.path(), "v4", FileStoreOptions::default(), hook)
            .await
            .unwrap();

    let earlier = store.save(b"earlier", b"one".to_vec());
    let first_stage_receiver = Arc::clone(&stage_receiver);
    tokio::task::spawn_blocking(move || first_stage_receiver.lock().unwrap().recv().unwrap())
        .await
        .unwrap();
    let maintenance = store.cleanup_stale_temporary_files_before_use(Duration::from_secs(1));
    drop(maintenance);
    let later = store.save(b"later", b"two".to_vec());

    // The later save cannot reach its first filesystem stage while the barrier
    // waits for the earlier save, even though the maintenance caller cancelled.
    assert!(matches!(
        stage_receiver.lock().unwrap().try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    release_sender.send(()).unwrap();
    earlier.await.unwrap();
    later.await.unwrap();
    assert_eq!(store.load(b"later").await.unwrap(), Some(b"two".to_vec()));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maintenance_barriers_preserve_interleaved_fifo_submission_order() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let root = tempfile::tempdir().unwrap();
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let event_receiver = Arc::new(std::sync::Mutex::new(event_receiver));
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let release_receiver = std::sync::Mutex::new(release_receiver);
    let cleanup_count = AtomicUsize::new(0);
    let save_count = AtomicUsize::new(0);
    let hook = Arc::new(move |stage| {
        match stage {
            TestStage::BeforeCleanup => {
                let index = cleanup_count.fetch_add(1, Ordering::SeqCst);
                event_sender
                    .send(if index == 0 { "M1" } else { "M2" })
                    .unwrap();
                if index == 0 {
                    release_receiver.lock().unwrap().recv().unwrap();
                }
            }
            TestStage::BeforeEnvelope => {
                let index = save_count.fetch_add(1, Ordering::SeqCst);
                event_sender
                    .send(if index == 0 { "B" } else { "C" })
                    .unwrap();
            }
            _ => {}
        }
        Ok(())
    });
    let store =
        FileStore::open_with_test_hook(root.path(), "v4", FileStoreOptions::default(), hook)
            .await
            .unwrap();

    let first_maintenance = store.cleanup_stale_temporary_files_before_use(Duration::from_secs(1));
    let first_event_receiver = Arc::clone(&event_receiver);
    assert_eq!(
        tokio::task::spawn_blocking(move || first_event_receiver.lock().unwrap().recv().unwrap())
            .await
            .unwrap(),
        "M1"
    );

    let before_second = store.save(b"before-second", b"B".to_vec());
    let second_maintenance = store.cleanup_stale_temporary_files_before_use(Duration::from_secs(1));
    let after_second = store.save(b"after-second", b"C".to_vec());
    release_sender.send(()).unwrap();

    let (first_result, before_result, second_result, after_result) = tokio::join!(
        first_maintenance,
        before_second,
        second_maintenance,
        after_second
    );
    assert!(matches!(
        first_result,
        Err(FileStoreError::CleanupUnsupported { .. })
    ));
    before_result.unwrap();
    assert!(matches!(
        second_result,
        Err(FileStoreError::CleanupUnsupported { .. })
    ));
    after_result.unwrap();

    // M2 must not overtake B, and C must not overtake M2.
    let receive_event = || event_receiver.lock().unwrap().recv().unwrap();
    assert_eq!(receive_event(), "B");
    assert_eq!(receive_event(), "M2");
    assert_eq!(receive_event(), "C");
    assert_eq!(
        store.load(b"before-second").await.unwrap(),
        Some(b"B".to_vec())
    );
    assert_eq!(
        store.load(b"after-second").await.unwrap(),
        Some(b"C".to_vec())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn relative_root_is_stable_after_current_directory_changes() {
    struct CurrentDirectoryGuard(PathBuf);

    impl Drop for CurrentDirectoryGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).expect("restore the test process current directory");
        }
    }

    let original_directory = std::env::current_dir().unwrap();
    let _guard = CurrentDirectoryGuard(original_directory);
    let directory = tempfile::tempdir().unwrap();
    let initial_directory = directory.path().join("initial");
    let later_directory = directory.path().join("later");
    let root = initial_directory.join("store-root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir(&later_directory).unwrap();

    std::env::set_current_dir(&initial_directory).unwrap();
    let store = FileStore::open("store-root", "v4", FileStoreOptions::default())
        .await
        .unwrap();
    let checkpoint = store.checkpoint_path(b"key");
    assert!(checkpoint.is_absolute());

    std::env::set_current_dir(&later_directory).unwrap();
    store.save(b"key", b"value".to_vec()).await.unwrap();
    assert_eq!(store.load(b"key").await.unwrap(), Some(b"value".to_vec()));
    assert!(checkpoint.is_file());
    assert!(!later_directory.join("store-root").exists());
}

#[cfg(unix)]
#[test]
fn store_remains_usable_after_construction_runtime_is_dropped() {
    let root = tempfile::tempdir().unwrap();
    let construction_runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let store = construction_runtime
        .block_on(FileStore::open(
            root.path(),
            "v4",
            FileStoreOptions::default(),
        ))
        .unwrap();
    let clone = store.clone();
    drop(construction_runtime);

    let save = store.save(b"key", b"value".to_vec());
    let client_runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    client_runtime.block_on(save).unwrap();
    assert_eq!(
        client_runtime.block_on(clone.load(b"key")).unwrap(),
        Some(b"value".to_vec())
    );
    drop(client_runtime);

    let later_runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    later_runtime.block_on(store.clear(b"key")).unwrap();
    assert_eq!(later_runtime.block_on(store.load(b"key")).unwrap(), None);
}

#[cfg(unix)]
#[tokio::test]
async fn root_and_namespace_types_are_validated() {
    let missing = tempfile::tempdir().unwrap().path().join("missing");
    assert!(matches!(
        FileStore::open(missing, "v4", FileStoreOptions::default()).await,
        Err(FileStoreError::RootDoesNotExist)
    ));

    let directory = tempfile::tempdir().unwrap();
    let root_file = directory.path().join("file");
    std::fs::write(&root_file, b"x").unwrap();
    assert!(matches!(
        FileStore::open(root_file, "v4", FileStoreOptions::default()).await,
        Err(FileStoreError::RootIsNotDirectory)
    ));

    std::fs::write(directory.path().join("v4"), b"x").unwrap();
    assert!(matches!(
        FileStore::open(directory.path(), "v4", FileStoreOptions::default()).await,
        Err(FileStoreError::NamespacePathIsNotDirectory)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn missing_namespace_is_not_reported_as_a_missing_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let store = FileStore::open(root.path(), "v4", FileStoreOptions::default())
        .await
        .unwrap();
    std::fs::remove_dir(root.path().join("v4")).unwrap();
    assert!(matches!(
        store.load(b"key").await,
        Err(FileStoreError::Io {
            operation: FileOperation::InspectNamespace,
            ..
        })
    ));
    assert!(matches!(
        store.clear(b"key").await,
        Err(FileStoreError::Io {
            operation: FileOperation::InspectNamespace,
            ..
        })
    ));
}

#[test]
fn io_and_atomic_commit_errors_preserve_sources() {
    use std::error::Error;

    let io_error = FileStoreError::Io {
        operation: FileOperation::ReadEnvelope,
        source: io::Error::other("read"),
    };
    assert_eq!(io_error.source().unwrap().to_string(), "read");
    let commit_error = FileStoreError::AtomicCommit {
        source: io::Error::other("commit"),
    };
    assert_eq!(commit_error.source().unwrap().to_string(), "commit");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_operations_remain_fifo_and_cannot_resurrect_a_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let store = FileStore::open(root.path(), "v4", FileStoreOptions::default())
        .await
        .unwrap();
    let key = b"cancelled-key";

    let save = store.save(key, vec![1; 4 * 1024 * 1024]);
    drop(save);
    store.clear(key).await.unwrap();
    assert_eq!(store.load(key).await.unwrap(), None);

    let clear = store.clear(key);
    drop(clear);
    store.save(key, b"after-clear".to_vec()).await.unwrap();
    assert_eq!(
        store.load(key).await.unwrap(),
        Some(b"after-clear".to_vec())
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn save_wrapper_failpoints_preserve_complete_old_or_new_values() {
    let before_commit = [
        TestStage::BeforeEnvelope,
        TestStage::AfterEnvelope,
        TestStage::BeforeAtomicOpen,
        TestStage::DuringWrite,
        TestStage::BeforeCommit,
        TestStage::CommitError,
    ];
    for failed_stage in before_commit {
        let root = tempfile::tempdir().unwrap();
        let initial = FileStore::open(root.path(), "v4", FileStoreOptions::default())
            .await
            .unwrap();
        initial.save(b"key", b"old".to_vec()).await.unwrap();
        drop(initial);

        let hook = Arc::new(move |stage| {
            if stage == failed_stage {
                Err(io::Error::other("injected save-stage failure"))
            } else {
                Ok(())
            }
        });
        let store =
            FileStore::open_with_test_hook(root.path(), "v4", FileStoreOptions::default(), hook)
                .await
                .unwrap();
        let error = store.save(b"key", b"new".to_vec()).await.unwrap_err();
        if failed_stage == TestStage::CommitError {
            assert!(matches!(error, FileStoreError::AtomicCommit { .. }));
        }
        assert_eq!(store.load(b"key").await.unwrap(), Some(b"old".to_vec()));
    }

    let root = tempfile::tempdir().unwrap();
    let hook = Arc::new(|stage| {
        if stage == TestStage::AfterCommit {
            Err(io::Error::other("injected post-commit failure"))
        } else {
            Ok(())
        }
    });
    let store =
        FileStore::open_with_test_hook(root.path(), "v4", FileStoreOptions::default(), hook)
            .await
            .unwrap();
    assert!(store.save(b"key", b"new".to_vec()).await.is_err());
    assert_eq!(store.load(b"key").await.unwrap(), Some(b"new".to_vec()));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clear_wrapper_failpoints_expose_only_old_or_absent() {
    for failed_stage in [
        TestStage::BeforeRemove,
        TestStage::AfterRemove,
        TestStage::BeforeDirectorySync,
        TestStage::AfterDirectorySync,
    ] {
        let root = tempfile::tempdir().unwrap();
        let initial = FileStore::open(root.path(), "v4", FileStoreOptions::default())
            .await
            .unwrap();
        initial.save(b"key", b"old".to_vec()).await.unwrap();
        drop(initial);
        let hook = Arc::new(move |stage| {
            if stage == failed_stage {
                Err(io::Error::other("injected clear-stage failure"))
            } else {
                Ok(())
            }
        });
        let store =
            FileStore::open_with_test_hook(root.path(), "v4", FileStoreOptions::default(), hook)
                .await
                .unwrap();
        assert!(store.clear(b"key").await.is_err());
        let loaded = store.load(b"key").await.unwrap();
        if failed_stage == TestStage::BeforeRemove {
            assert_eq!(loaded, Some(b"old".to_vec()));
        } else {
            assert_eq!(loaded, None);
        }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocked_cancelled_work_keeps_same_key_order_but_not_other_keys() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let root = tempfile::tempdir().unwrap();
    let (started_sender, started_receiver) = std::sync::mpsc::channel();
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let release_receiver = std::sync::Mutex::new(release_receiver);
    let first = AtomicBool::new(true);
    let hook = Arc::new(move |stage| {
        if stage == TestStage::BeforeCommit && first.swap(false, Ordering::SeqCst) {
            started_sender.send(()).unwrap();
            release_receiver.lock().unwrap().recv().unwrap();
        }
        Ok(())
    });
    let store =
        FileStore::open_with_test_hook(root.path(), "v4", FileStoreOptions::default(), hook)
            .await
            .unwrap();

    let cancelled = store.save(b"same", b"value".to_vec());
    drop(cancelled);
    tokio::task::spawn_blocking(move || started_receiver.recv().unwrap())
        .await
        .unwrap();

    // Submission is synchronous, so this clear is already queued behind the
    // blocked save before its future is moved into the task.
    let clear = store.clear(b"same");
    let (clear_sender, mut clear_receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        clear.await.unwrap();
        let _ = clear_sender.send(());
    });
    assert!(matches!(
        clear_receiver.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    // A different key dispatches independently while the first key is paused.
    store.save(b"other", b"concurrent".to_vec()).await.unwrap();
    assert_eq!(
        store.load(b"other").await.unwrap(),
        Some(b"concurrent".to_vec())
    );

    release_sender.send(()).unwrap();
    clear_receiver.await.unwrap();
    assert_eq!(store.load(b"same").await.unwrap(), None);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloned_handles_and_many_transient_keys_remain_operational() {
    use std::sync::atomic::Ordering;

    let root = tempfile::tempdir().unwrap();
    let store = FileStore::open(root.path(), "v4", FileStoreOptions::default())
        .await
        .unwrap();
    let clone = store.clone();
    for index in 0_u32..2_000 {
        let key = index.to_be_bytes();
        let operation = clone.save(&key, key.to_vec());
        operation.await.unwrap();
        store.clear(&key).await.unwrap();
        assert_eq!(store.inner.registry_entries.load(Ordering::SeqCst), 0);
    }
    clone.save(b"final", b"ok".to_vec()).await.unwrap();
    assert_eq!(store.load(b"final").await.unwrap(), Some(b"ok".to_vec()));
    assert_eq!(store.inner.registry_entries.load(Ordering::SeqCst), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn save_uses_owner_only_mode_and_does_not_preserve_broader_mode() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = tempfile::tempdir().unwrap();
    let store = FileStore::open(root.path(), "v4", FileStoreOptions::default())
        .await
        .unwrap();
    let key = b"mode-key";
    store.save(key, b"one".to_vec()).await.unwrap();
    let path = store.checkpoint_path(key);
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        let path_bytes = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path_bytes` is a valid NUL-terminated path for this call.
        assert_eq!(unsafe { libc::chown(path_bytes.as_ptr(), 1, 1) }, 0);
    }
    store.save(key, b"two".to_vec()).await.unwrap();
    let metadata = std::fs::metadata(path).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o600);
    // SAFETY: `geteuid` has no preconditions.
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
}

#[cfg(unix)]
#[test]
fn actual_atomic_writer_preserves_old_value_until_commit_and_drop_discards() {
    use atomic_write_file::unix::OpenOptionsExt as AtomicOpenOptionsExt;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt as StdOpenOptionsExt;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("checkpoint");
    std::fs::write(&path, b"old").unwrap();

    let open = || {
        let mut options = atomic_write_file::OpenOptions::new();
        StdOpenOptionsExt::mode(&mut options, 0o600);
        AtomicOpenOptionsExt::preserve_mode(&mut options, false);
        AtomicOpenOptionsExt::preserve_owner(&mut options, false);
        options.open(&path).unwrap()
    };

    let mut writer = open();
    writer.write_all(b"new").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    drop(writer);
    assert_eq!(std::fs::read(&path).unwrap(), b"old");

    let mut writer = open();
    writer.write_all(b"new").unwrap();
    writer.commit().unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"new");
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_load_ignores_unrelated_temporary_files() {
    let root = tempfile::tempdir().unwrap();
    let store = FileStore::open(root.path(), "v4", FileStoreOptions::default())
        .await
        .unwrap();
    let key = b"key";
    std::fs::write(
        root.path().join("v4/.unrelated.temporary"),
        envelope(b"fake"),
    )
    .unwrap();
    assert_eq!(store.load(key).await.unwrap(), None);
}

#[cfg(unix)]
#[test]
fn atomic_child_boundary() {
    use atomic_write_file::unix::OpenOptionsExt as AtomicOpenOptionsExt;
    use std::io::{BufRead, Write};
    use std::os::unix::fs::OpenOptionsExt as StdOpenOptionsExt;

    let Ok(path) = std::env::var("RUMQTTC_ATOMIC_CHILD_PATH") else {
        return;
    };
    let payload = std::env::var("RUMQTTC_ATOMIC_CHILD_PAYLOAD").unwrap();
    let mut options = atomic_write_file::OpenOptions::new();
    StdOpenOptionsExt::mode(&mut options, 0o600);
    AtomicOpenOptionsExt::preserve_mode(&mut options, false);
    AtomicOpenOptionsExt::preserve_owner(&mut options, false);
    let mut writer = options.open(path).unwrap();
    writer.write_all(&envelope(payload.as_bytes())).unwrap();
    println!("READY");

    let mut command = String::new();
    std::io::stdin().lock().read_line(&mut command).unwrap();
    if command.trim() == "commit" {
        writer.commit().unwrap();
        println!("COMMITTED");
    }
}

#[cfg(unix)]
#[test]
fn subprocess_exit_before_commit_and_successful_commit_have_permitted_states() {
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};

    fn spawn_child(path: &Path, payload: &str) -> std::process::Child {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::atomic_child_boundary", "--nocapture"])
            .env("RUMQTTC_ATOMIC_CHILD_PATH", path)
            .env("RUMQTTC_ATOMIC_CHILD_PAYLOAD", payload)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn wait_for(reader: &mut impl BufRead, expected: &str) {
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line.contains(expected) {
                return;
            }
        }
    }

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("checkpoint.session");
    std::fs::write(&path, envelope(b"old")).unwrap();

    let mut interrupted = spawn_child(&path, "new");
    let mut output = std::io::BufReader::new(interrupted.stdout.take().unwrap());
    wait_for(&mut output, "READY");
    interrupted.kill().unwrap();
    interrupted.wait().unwrap();
    assert_eq!(
        decode_reader(
            &mut Cursor::new(std::fs::read(&path).unwrap()),
            TEST_MAXIMUM
        )
        .unwrap(),
        b"old"
    );

    let mut committed = spawn_child(&path, "new");
    let mut output = std::io::BufReader::new(committed.stdout.take().unwrap());
    wait_for(&mut output, "READY");
    committed
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"commit\n")
        .unwrap();
    wait_for(&mut output, "COMMITTED");
    assert!(committed.wait().unwrap().success());
    assert_eq!(
        decode_reader(&mut Cursor::new(std::fs::read(path).unwrap()), TEST_MAXIMUM).unwrap(),
        b"new"
    );
}
