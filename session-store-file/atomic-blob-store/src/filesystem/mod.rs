use super::*;
#[cfg(windows)]
use std::time::SystemTime;
#[cfg(any(unix, windows))]
pub(crate) fn initialize_platform(
    mut root: PathBuf,
    namespace_component: PathBuf,
    format: BlobFormatIdentity,
    maximum: u64,
    max_concurrent_operations: usize,
) -> Result<StoreConfig, AtomicBlobStoreError> {
    #[cfg(windows)]
    {
        root = std::path::absolute(&root).map_err(|source| AtomicBlobStoreError::Io {
            operation: StoreOperation::NormalizeRoot,
            source,
        })?;
    }

    #[cfg(unix)]
    if root.is_relative() {
        let current_directory =
            std::env::current_dir().map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::ResolveCurrentDirectory,
                source,
            })?;
        root = current_directory.join(root);
    }

    let metadata = match std::fs::metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(AtomicBlobStoreError::RootDoesNotExist);
        }
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::InspectRoot,
                source,
            });
        }
    };
    if !metadata.is_dir() {
        return Err(AtomicBlobStoreError::RootIsNotDirectory);
    }

    let namespace = root.join(namespace_component);
    match std::fs::create_dir(&namespace) {
        Ok(()) => {
            #[cfg(unix)]
            sync_directory(
                &root,
                StoreOperation::OpenRootDirectory,
                StoreOperation::SyncRootDirectory,
            )?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata =
                std::fs::metadata(&namespace).map_err(|source| AtomicBlobStoreError::Io {
                    operation: StoreOperation::InspectNamespace,
                    source,
                })?;
            if !metadata.is_dir() {
                return Err(AtomicBlobStoreError::NamespacePathIsNotDirectory);
            }
        }
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::CreateNamespace,
                source,
            });
        }
    }

    Ok(StoreConfig {
        namespace,
        format,
        maximum,
        max_concurrent_operations,
        #[cfg(all(test, any(unix, windows)))]
        hook: None,
        #[cfg(feature = "bench-instrumentation")]
        benchmark_events: None,
    })
}
#[cfg(any(unix, windows))]
pub(crate) fn ensure_namespace_available(config: &StoreConfig) -> Result<(), AtomicBlobStoreError> {
    match std::fs::metadata(&config.namespace) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(AtomicBlobStoreError::NamespacePathIsNotDirectory),
        Err(source) => Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::InspectNamespace,
            source,
        }),
    }
}

#[cfg(unix)]
pub(crate) fn save_blob(
    config: &StoreConfig,
    path: &Path,
    payload: &[u8],
) -> Result<(), AtomicBlobStoreError> {
    use atomic_write_file::unix::OpenOptionsExt as AtomicOpenOptionsExt;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt as StdOpenOptionsExt;

    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeEnvelope,
        StoreOperation::WriteEnvelope,
    )?;
    #[cfg(test)]
    let envelope = encode_envelope(&config.format, payload, config.maximum)?;
    #[cfg(not(test))]
    let (header, checksum) = envelope_parts(&config.format, payload, config.maximum)?;
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::AfterEnvelope,
        StoreOperation::WriteEnvelope,
    )?;
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeAtomicOpen,
        StoreOperation::OpenAtomicWriter,
    )?;
    let mut options = atomic_write_file::OpenOptions::new();
    StdOpenOptionsExt::mode(&mut options, 0o600);
    AtomicOpenOptionsExt::preserve_mode(&mut options, false);
    AtomicOpenOptionsExt::preserve_owner(&mut options, false);
    let mut writer = options
        .open(path)
        .map_err(|source| AtomicBlobStoreError::Io {
            operation: StoreOperation::OpenAtomicWriter,
            source,
        })?;
    #[cfg(all(test, unix))]
    {
        let midpoint = envelope.len() / 2;
        writer
            .write_all(&envelope[..midpoint])
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::WriteEnvelope,
                source,
            })?;
        hit_test_stage(
            config,
            TestStage::DuringWrite,
            StoreOperation::WriteEnvelope,
        )?;
        writer
            .write_all(&envelope[midpoint..])
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::WriteEnvelope,
                source,
            })?;
    }
    #[cfg(not(test))]
    for section in [&header[..], payload, &checksum[..]] {
        writer
            .write_all(section)
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::WriteEnvelope,
                source,
            })?;
    }
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeCommit,
        StoreOperation::WriteEnvelope,
    )?;
    #[cfg(all(test, unix))]
    if let Err(source) = config
        .hook
        .as_ref()
        .map_or(Ok(()), |hook| hook(TestStage::CommitError))
    {
        return Err(AtomicBlobStoreError::AtomicCommit { source });
    }
    writer
        .commit()
        .map_err(|source| AtomicBlobStoreError::AtomicCommit { source })?;
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::AfterCommit,
        StoreOperation::WriteEnvelope,
    )?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn save_blob_from_receiver(
    config: &StoreConfig,
    path: &Path,
    declared_len: u64,
    chunks: &mut Receiver<SaveStreamMessage>,
) -> Result<(), AtomicBlobStoreError> {
    use atomic_write_file::unix::OpenOptionsExt as AtomicOpenOptionsExt;
    use std::os::unix::fs::OpenOptionsExt as StdOpenOptionsExt;

    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeEnvelope,
        StoreOperation::WriteEnvelope,
    )?;
    envelope_header(&config.format, declared_len, config.maximum)?;
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::AfterEnvelope,
        StoreOperation::WriteEnvelope,
    )?;
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeAtomicOpen,
        StoreOperation::OpenAtomicWriter,
    )?;

    let mut options = atomic_write_file::OpenOptions::new();
    StdOpenOptionsExt::mode(&mut options, 0o600);
    AtomicOpenOptionsExt::preserve_mode(&mut options, false);
    AtomicOpenOptionsExt::preserve_owner(&mut options, false);
    let mut writer = options
        .open(path)
        .map_err(|source| AtomicBlobStoreError::Io {
            operation: StoreOperation::OpenAtomicWriter,
            source,
        })?;
    write_stream_envelope(config, &mut writer, declared_len, chunks)?;

    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeCommit,
        StoreOperation::WriteEnvelope,
    )?;
    #[cfg(all(test, unix))]
    if let Err(source) = config
        .hook
        .as_ref()
        .map_or(Ok(()), |hook| hook(TestStage::CommitError))
    {
        return Err(AtomicBlobStoreError::AtomicCommit { source });
    }
    writer
        .commit()
        .map_err(|source| AtomicBlobStoreError::AtomicCommit { source })?;
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::AfterCommit,
        StoreOperation::WriteEnvelope,
    )?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn clear_blob(config: &StoreConfig, path: &Path) -> Result<(), AtomicBlobStoreError> {
    #[cfg(all(test, unix))]
    hit_test_stage(config, TestStage::BeforeRemove, StoreOperation::RemoveBlob)?;
    match std::fs::remove_file(path) {
        Ok(()) => {
            #[cfg(all(test, unix))]
            hit_test_stage(config, TestStage::AfterRemove, StoreOperation::RemoveBlob)?;
            #[cfg(all(test, unix))]
            hit_test_stage(
                config,
                TestStage::BeforeDirectorySync,
                StoreOperation::SyncNamespaceDirectory,
            )?;
            sync_directory(
                &config.namespace,
                StoreOperation::OpenNamespaceDirectory,
                StoreOperation::SyncNamespaceDirectory,
            )?;
            #[cfg(all(test, unix))]
            hit_test_stage(
                config,
                TestStage::AfterDirectorySync,
                StoreOperation::SyncNamespaceDirectory,
            )?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => ensure_namespace_available(config),
        Err(source) => Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::RemoveBlob,
            source,
        }),
    }
}

#[cfg(any(unix, windows))]
pub(crate) fn inspect_blob(
    config: &StoreConfig,
    path: &Path,
) -> Result<BlobInspection, AtomicBlobStoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(BlobInspection {
            state: BlobState::Present,
            size: Some(metadata.len()),
            modified: metadata.modified().ok(),
        }),
        Ok(_) => Err(AtomicBlobStoreError::UnexpectedFileType),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_namespace_available(config)?;
            Ok(BlobInspection {
                state: BlobState::Absent,
                size: None,
                modified: None,
            })
        }
        Err(source) => Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::InspectBlob,
            source,
        }),
    }
}

#[cfg(any(unix, windows))]
pub(crate) fn random_identifier() -> Result<String, AtomicBlobStoreError> {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|source| AtomicBlobStoreError::IdentifierGeneration { source })?;
    let mut result = String::with_capacity(64);
    for byte in bytes {
        result.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        result.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    Ok(result)
}

#[cfg(unix)]
pub(crate) fn quarantine_blob(
    config: &StoreConfig,
    path: &Path,
) -> Result<QuarantineInfo, AtomicBlobStoreError> {
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(AtomicBlobStoreError::UnexpectedFileType)?;
    loop {
        let identifier = random_identifier()?;
        let destination = config.namespace.join(format!(
            "{hash}{}.quarantine-v1.{identifier}",
            config.format.filename_suffix()
        ));
        match rustix::fs::renameat_with(
            rustix::fs::CWD,
            path,
            rustix::fs::CWD,
            &destination,
            rustix::fs::RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                let quarantine = QuarantineInfo {
                    identifier,
                    diagnostic_path: destination,
                };
                sync_quarantine_namespace(config, &quarantine)?;
                return Ok(quarantine);
            }
            Err(error) if error == rustix::io::Errno::NOENT => {
                ensure_namespace_available(config)?;
                return Err(AtomicBlobStoreError::QuarantineSourceMissing);
            }
            Err(error) if error == rustix::io::Errno::EXIST => {}
            Err(error) => {
                return Err(AtomicBlobStoreError::QuarantineCommit {
                    source: io::Error::from_raw_os_error(error.raw_os_error()),
                });
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn sync_quarantine_namespace(
    config: &StoreConfig,
    quarantine: &QuarantineInfo,
) -> Result<(), AtomicBlobStoreError> {
    #[cfg(test)]
    if let Some(hook) = &config.hook {
        hook(TestStage::BeforeDirectorySync).map_err(|source| {
            AtomicBlobStoreError::QuarantineNamespaceSync {
                quarantine: quarantine.clone(),
                source,
            }
        })?;
    }

    let directory = std::fs::File::open(&config.namespace).map_err(|source| {
        AtomicBlobStoreError::QuarantineNamespaceSync {
            quarantine: quarantine.clone(),
            source,
        }
    })?;
    directory
        .sync_all()
        .map_err(|source| AtomicBlobStoreError::QuarantineNamespaceSync {
            quarantine: quarantine.clone(),
            source,
        })?;

    #[cfg(test)]
    if let Some(hook) = &config.hook {
        hook(TestStage::AfterDirectorySync).map_err(|source| {
            AtomicBlobStoreError::QuarantineNamespaceSync {
                quarantine: quarantine.clone(),
                source,
            }
        })?;
    }
    Ok(())
}

#[cfg(unix)]
#[allow(clippy::missing_const_for_fn)]
pub(crate) fn cleanup_stale_files(
    config: &StoreConfig,
    _minimum_age: Duration,
) -> Result<CleanupReport, AtomicBlobStoreError> {
    #[cfg(not(test))]
    let _ = config;
    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::BeforeCleanup,
        StoreOperation::EnumerateTemporaryFiles,
    )?;
    Err(AtomicBlobStoreError::CleanupUnsupported {
        platform: std::env::consts::OS,
    })
}

#[cfg(windows)]
pub(crate) fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    const SEPARATOR: u16 = b'\\' as u16;
    const ALTERNATE_SEPARATOR: u16 = b'/' as u16;
    const VERBATIM_PREFIX: &[u16] = &[SEPARATOR, SEPARATOR, b'?' as u16, SEPARATOR];
    const DEVICE_PREFIX: &[u16] = &[SEPARATOR, SEPARATOR, b'.' as u16, SEPARATOR];
    const UNC_PREFIX: &[u16] = &[
        SEPARATOR,
        SEPARATOR,
        b'?' as u16,
        SEPARATOR,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        SEPARATOR,
    ];

    let path: Vec<u16> = path.as_os_str().encode_wide().collect();
    let (prefix, remainder) =
        if path.starts_with(VERBATIM_PREFIX) || path.starts_with(DEVICE_PREFIX) {
            (&[][..], path.as_slice())
        } else if path.starts_with(&[SEPARATOR, SEPARATOR]) {
            (UNC_PREFIX, &path[2..])
        } else {
            (VERBATIM_PREFIX, path.as_slice())
        };

    let mut extended = Vec::with_capacity(prefix.len() + remainder.len() + 1);
    extended.extend_from_slice(prefix);
    extended.extend(remainder.iter().map(|unit| {
        if *unit == ALTERNATE_SEPARATOR {
            SEPARATOR
        } else {
            *unit
        }
    }));
    extended.push(0);
    extended
}

#[cfg(windows)]
pub(crate) fn move_file(source: &Path, destination: &Path, flags: u32) -> Result<(), io::Error> {
    let source = wide_path(source);
    let destination = wide_path(destination);
    // SAFETY: both pointers reference NUL-terminated wide strings for the call.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            flags,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
pub(crate) fn delete_file(path: &Path) -> Result<(), io::Error> {
    let path = wide_path(path);
    // SAFETY: the pointer references a NUL-terminated wide string for the call.
    if unsafe { windows_sys::Win32::Storage::FileSystem::DeleteFileW(path.as_ptr()) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
pub(crate) fn sync_windows_directory(config: &StoreConfig) -> Result<(), io::Error> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FlushFileBuffers, OPEN_EXISTING,
    };

    let path = wide_path(&config.namespace);
    // SAFETY: `path` is NUL-terminated and the returned handle is checked before ownership is
    // transferred to `File`.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is valid and uniquely owned. `File` closes it on every return path.
    let _directory = unsafe { std::fs::File::from_raw_handle(handle) };

    #[cfg(test)]
    if let Some(hook) = &config.hook {
        hook(TestStage::BeforeDirectorySync)?;
    }
    // SAFETY: `directory` owns a valid directory handle opened for synchronization.
    if unsafe { FlushFileBuffers(handle) } == 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(test)]
    if let Some(hook) = &config.hook {
        hook(TestStage::AfterDirectorySync)?;
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn create_windows_staging(
    _config: &StoreConfig,
    path: &Path,
    header: &[u8],
    payload: &[u8],
    checksum: &[u8],
) -> Result<(), AtomicBlobStoreError> {
    use std::io::Write;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_WRITE_THROUGH, FlushFileBuffers,
    };

    #[cfg(test)]
    hit_test_stage(
        _config,
        TestStage::BeforeAtomicOpen,
        StoreOperation::OpenAtomicWriter,
    )?;
    let wide = wide_path(path);
    // SAFETY: `wide` is NUL-terminated; null security attributes deliberately inherit the directory ACL.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            0,
            std::ptr::null(),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::OpenAtomicWriter,
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: the successful CreateFileW call returned an owned handle.
    let mut file = unsafe { std::fs::File::from_raw_handle(handle) };
    let mut write_section = |section| {
        file.write_all(section)
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::WriteEnvelope,
                source,
            })
    };
    write_section(header)?;
    #[cfg(test)]
    hit_test_stage(
        _config,
        TestStage::DuringWrite,
        StoreOperation::WriteEnvelope,
    )?;
    for section in [payload, checksum] {
        write_section(section)?;
    }
    // SAFETY: the file still owns a live handle.
    if unsafe { FlushFileBuffers(handle) } == 0 {
        return Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::WriteEnvelope,
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn create_windows_streaming_staging(
    config: &StoreConfig,
    path: &Path,
    declared_len: u64,
    chunks: &mut Receiver<SaveStreamMessage>,
) -> Result<(), AtomicBlobStoreError> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_WRITE_THROUGH, FlushFileBuffers,
    };

    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::BeforeAtomicOpen,
        StoreOperation::OpenAtomicWriter,
    )?;
    let wide = wide_path(path);
    // SAFETY: `wide` is NUL-terminated; null security attributes deliberately inherit the directory ACL.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            0,
            std::ptr::null(),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::OpenAtomicWriter,
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: the successful CreateFileW call returned an owned handle.
    let mut file = unsafe { std::fs::File::from_raw_handle(handle) };
    write_stream_envelope(config, &mut file, declared_len, chunks)?;
    // SAFETY: the file still owns a live handle.
    if unsafe { FlushFileBuffers(handle) } == 0 {
        return Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::WriteEnvelope,
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn refresh_windows_clear_age(path: &Path) -> Result<(), io::Error> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{FILETIME, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_WRITE_THROUGH, FILE_WRITE_ATTRIBUTES,
        FlushFileBuffers, OPEN_EXISTING, SetFileTime,
    };

    const WINDOWS_EPOCH_OFFSET_100NS: u64 = 116_444_736_000_000_000;
    const HUNDRED_NS_PER_SECOND: u64 = 10_000_000;

    let elapsed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let ticks = elapsed
        .as_secs()
        .checked_mul(HUNDRED_NS_PER_SECOND)
        .and_then(|ticks| ticks.checked_add(u64::from(elapsed.subsec_nanos() / 100)))
        .and_then(|ticks| ticks.checked_add(WINDOWS_EPOCH_OFFSET_100NS))
        .ok_or_else(|| {
            io::Error::other("current time cannot be represented as a Windows file time")
        })?;
    let [b0, b1, b2, b3, b4, b5, b6, b7] = ticks.to_le_bytes();
    let last_write = FILETIME {
        dwLowDateTime: u32::from_le_bytes([b0, b1, b2, b3]),
        dwHighDateTime: u32::from_le_bytes([b4, b5, b6, b7]),
    };

    let wide = wide_path(path);
    // SAFETY: `wide` is NUL-terminated and the returned handle is checked below.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE | FILE_WRITE_ATTRIBUTES,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful CreateFileW call returned an owned handle. Keeping
    // it in a File ensures it is closed before the subsequent rename.
    let _file = unsafe { std::fs::File::from_raw_handle(handle) };
    // SAFETY: `handle` is live and `last_write` remains valid for the call.
    if unsafe {
        SetFileTime(
            handle,
            std::ptr::null(),
            std::ptr::null(),
            &raw const last_write,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is still live and was opened with write access.
    if unsafe { FlushFileBuffers(handle) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn save_blob(
    config: &StoreConfig,
    path: &Path,
    payload: &[u8],
) -> Result<(), AtomicBlobStoreError> {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::BeforeEnvelope,
        StoreOperation::WriteEnvelope,
    )?;
    let (header, checksum) = envelope_parts(&config.format, payload, config.maximum)?;
    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::AfterEnvelope,
        StoreOperation::WriteEnvelope,
    )?;
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(AtomicBlobStoreError::UnexpectedFileType)?;
    loop {
        let identifier = random_identifier()?;
        let staging = config.namespace.join(format!(
            "{hash}{}.tmp-v1.save.{identifier}",
            config.format.filename_suffix()
        ));
        match create_windows_staging(config, &staging, &header, payload, &checksum) {
            Ok(()) => {}
            Err(AtomicBlobStoreError::Io { source, .. })
                if source.kind() == io::ErrorKind::AlreadyExists =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
        #[cfg(test)]
        hit_test_stage(
            config,
            TestStage::BeforeCommit,
            StoreOperation::WriteEnvelope,
        )?;
        #[cfg(test)]
        if let Err(source) = config
            .hook
            .as_ref()
            .map_or(Ok(()), |hook| hook(TestStage::CommitError))
        {
            return Err(AtomicBlobStoreError::AtomicCommit { source });
        }
        let initial = move_file(&staging, path, MOVEFILE_WRITE_THROUGH);
        match initial {
            Ok(()) => {}
            Err(error) if matches!(error.raw_os_error(), Some(code) if code.cast_unsigned() == ERROR_FILE_EXISTS || code.cast_unsigned() == ERROR_ALREADY_EXISTS) =>
            {
                move_file(
                    &staging,
                    path,
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
                .map_err(|source| AtomicBlobStoreError::AtomicCommit { source })?;
            }
            Err(source) => return Err(AtomicBlobStoreError::AtomicCommit { source }),
        }
        #[cfg(test)]
        hit_test_stage(
            config,
            TestStage::AfterCommit,
            StoreOperation::WriteEnvelope,
        )?;
        sync_windows_directory(config).map_err(|source| AtomicBlobStoreError::Io {
            operation: StoreOperation::SyncNamespaceDirectory,
            source,
        })?;
        return Ok(());
    }
}

#[cfg(windows)]
pub(crate) fn save_blob_from_receiver(
    config: &StoreConfig,
    path: &Path,
    declared_len: u64,
    chunks: &mut Receiver<SaveStreamMessage>,
) -> Result<(), AtomicBlobStoreError> {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::BeforeEnvelope,
        StoreOperation::WriteEnvelope,
    )?;
    envelope_header(&config.format, declared_len, config.maximum)?;
    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::AfterEnvelope,
        StoreOperation::WriteEnvelope,
    )?;
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(AtomicBlobStoreError::UnexpectedFileType)?;
    loop {
        let identifier = random_identifier()?;
        let staging = config.namespace.join(format!(
            "{hash}{}.tmp-v1.save.{identifier}",
            config.format.filename_suffix()
        ));
        match create_windows_streaming_staging(config, &staging, declared_len, chunks) {
            Ok(()) => {}
            Err(AtomicBlobStoreError::Io { source, .. })
                if source.kind() == io::ErrorKind::AlreadyExists =>
            {
                continue;
            }
            Err(error) => {
                let _ = delete_file(&staging);
                return Err(error);
            }
        }
        #[cfg(test)]
        hit_test_stage(
            config,
            TestStage::BeforeCommit,
            StoreOperation::WriteEnvelope,
        )?;
        #[cfg(test)]
        if let Err(source) = config
            .hook
            .as_ref()
            .map_or(Ok(()), |hook| hook(TestStage::CommitError))
        {
            return Err(AtomicBlobStoreError::AtomicCommit { source });
        }
        let initial = move_file(&staging, path, MOVEFILE_WRITE_THROUGH);
        match initial {
            Ok(()) => {}
            Err(error) if matches!(error.raw_os_error(), Some(code) if code.cast_unsigned() == ERROR_FILE_EXISTS || code.cast_unsigned() == ERROR_ALREADY_EXISTS) =>
            {
                move_file(
                    &staging,
                    path,
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
                .map_err(|source| AtomicBlobStoreError::AtomicCommit { source })?;
            }
            Err(source) => return Err(AtomicBlobStoreError::AtomicCommit { source }),
        }
        #[cfg(test)]
        hit_test_stage(
            config,
            TestStage::AfterCommit,
            StoreOperation::WriteEnvelope,
        )?;
        sync_windows_directory(config).map_err(|source| AtomicBlobStoreError::Io {
            operation: StoreOperation::SyncNamespaceDirectory,
            source,
        })?;
        return Ok(());
    }
}

#[cfg(windows)]
pub(crate) fn clear_blob(config: &StoreConfig, path: &Path) -> Result<(), AtomicBlobStoreError> {
    use windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
    #[cfg(test)]
    hit_test_stage(config, TestStage::BeforeRemove, StoreOperation::RemoveBlob)?;
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(AtomicBlobStoreError::UnexpectedFileType)?;
    match refresh_windows_clear_age(path) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            ensure_namespace_available(config)?;
            return Ok(());
        }
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::RefreshClearStagingAge,
                source,
            });
        }
    }
    loop {
        let identifier = random_identifier()?;
        let staging = config.namespace.join(format!(
            "{hash}{}.tmp-v1.clear.{identifier}",
            config.format.filename_suffix()
        ));
        match move_file(path, &staging, MOVEFILE_WRITE_THROUGH) {
            Ok(()) => {
                #[cfg(test)]
                hit_test_stage(config, TestStage::AfterRemove, StoreOperation::RemoveBlob)?;
                sync_windows_directory(config).map_err(|source| AtomicBlobStoreError::Io {
                    operation: StoreOperation::SyncNamespaceDirectory,
                    source,
                })?;
                return delete_file(&staging).map_err(|source| AtomicBlobStoreError::Io {
                    operation: StoreOperation::RemoveTemporaryFile,
                    source,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ensure_namespace_available(config)?;
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(AtomicBlobStoreError::Io {
                    operation: StoreOperation::RemoveBlob,
                    source,
                });
            }
        }
    }
}

#[cfg(windows)]
pub(crate) fn quarantine_blob(
    config: &StoreConfig,
    path: &Path,
) -> Result<QuarantineInfo, AtomicBlobStoreError> {
    use windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::BeforeQuarantineRename,
        StoreOperation::QuarantineBlob,
    )?;
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(AtomicBlobStoreError::UnexpectedFileType)?;
    loop {
        let identifier = random_identifier()?;
        let destination = config.namespace.join(format!(
            "{hash}{}.quarantine-v1.{identifier}",
            config.format.filename_suffix()
        ));
        match move_file(path, &destination, MOVEFILE_WRITE_THROUGH) {
            Ok(()) => {
                let quarantine = QuarantineInfo {
                    identifier,
                    diagnostic_path: destination,
                };
                #[cfg(test)]
                if let Some(hook) = &config.hook {
                    hook(TestStage::AfterQuarantineRename).map_err(|source| {
                        AtomicBlobStoreError::QuarantineNamespaceSync {
                            quarantine: quarantine.clone(),
                            source,
                        }
                    })?;
                }
                sync_windows_directory(config).map_err(|source| {
                    AtomicBlobStoreError::QuarantineNamespaceSync {
                        quarantine: quarantine.clone(),
                        source,
                    }
                })?;
                return Ok(quarantine);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ensure_namespace_available(config)?;
                return Err(AtomicBlobStoreError::QuarantineSourceMissing);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(AtomicBlobStoreError::QuarantineCommit { source }),
        }
    }
}

#[cfg(windows)]
pub(crate) fn cleanup_stale_files(
    config: &StoreConfig,
    minimum_age: Duration,
) -> Result<CleanupReport, AtomicBlobStoreError> {
    let now = SystemTime::now();
    let mut report = CleanupReport::default();
    let entries =
        std::fs::read_dir(&config.namespace).map_err(|source| AtomicBlobStoreError::Io {
            operation: StoreOperation::EnumerateTemporaryFiles,
            source,
        })?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                report.failures.push(CleanupFailure {
                    identifier: "<directory-entry>".into(),
                    source,
                });
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_owned_temporary_filename(&name, config.format.filename_suffix()) {
            continue;
        }
        #[cfg(test)]
        hit_test_stage(
            config,
            TestStage::BeforeCleanupMetadata,
            StoreOperation::InspectBlob,
        )?;
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                report.failures.push(CleanupFailure {
                    identifier: name,
                    source: io::Error::other("owned staging name is not a regular file"),
                });
                continue;
            }
            Err(source) => {
                report.failures.push(CleanupFailure {
                    identifier: name,
                    source,
                });
                continue;
            }
        };
        let old_enough = metadata
            .modified()
            .ok()
            .and_then(|time| now.duration_since(time).ok())
            .is_some_and(|age| age >= minimum_age);
        if !old_enough {
            report.skipped.push(name);
            continue;
        }
        #[cfg(test)]
        hit_test_stage(
            config,
            TestStage::BeforeCleanupRemove,
            StoreOperation::RemoveTemporaryFile,
        )?;
        match delete_file(&entry.path()) {
            Ok(()) => report.removed.push(name),
            Err(source) => report.failures.push(CleanupFailure {
                identifier: name,
                source,
            }),
        }
    }
    Ok(report)
}

#[cfg(windows)]
pub(crate) fn is_owned_temporary_filename(name: &str, suffix: &str) -> bool {
    let marker = format!("{suffix}.tmp-v1.");
    let Some((hash, rest)) = name.split_once(&marker) else {
        return false;
    };
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return false;
    }
    let Some((kind, identifier)) = rest.split_once('.') else {
        return false;
    };
    matches!(kind, "save" | "clear")
        && identifier.len() == 64
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(unix)]
pub(crate) fn sync_directory(
    path: &Path,
    open_operation: StoreOperation,
    sync_operation: StoreOperation,
) -> Result<(), AtomicBlobStoreError> {
    let directory = std::fs::File::open(path).map_err(|source| AtomicBlobStoreError::Io {
        operation: open_operation,
        source,
    })?;
    directory
        .sync_all()
        .map_err(|source| AtomicBlobStoreError::Io {
            operation: sync_operation,
            source,
        })
}
