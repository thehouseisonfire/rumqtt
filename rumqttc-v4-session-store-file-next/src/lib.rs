//! Production-oriented Unix and Windows file store for MQTT 3.1.1 sessions.
//!
//! This adapter encodes the complete [`rumqttc::SessionStoreKey`] into a stable
//! byte format and delegates opaque canonical [`rumqttc::PersistedSession`]
//! bytes to the protocol-neutral core store.
//!
//! # Limitations
//!
//! The backend assumes an existing, trusted local-filesystem
//! root. It provides process-local same-key coordination but no cross-process
//! locking or multi-writer protection, encryption, authentication, or tamper
//! resistance, and its synchronization operations are not a universal
//! power-loss guarantee. Unix dependency-private temporary names are ignored;
//! Windows cleanup recognizes only store-owned staging names.
//!
//! The `v4` hashed, versioned layout is incompatible with files written by the
//! repository's earlier example store. When canonical state is absent, exactly
//! the old example filename is detected and reported, but never migrated.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use rumqttc::{
    PersistedSession, SessionDecodeError, SessionEncodeError, SessionStore, SessionStoreKey,
};
pub use rumqttc_session_store_file_core_next::{
    CheckpointInspection, CheckpointState, CleanupReport, QuarantineInfo,
};
use rumqttc_session_store_file_core_next::{
    FileStore, FileStoreError, FileStoreOptions, LegacyLoad, checkpoint_filename,
};

const KEY_FORMAT_VERSION: u8 = 1;
const PROTOCOL_TAG: u8 = 4;
const NAMESPACE: &str = "v4";

type StoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, rumqttc::SessionStoreError>> + Send + 'a>>;

/// Error produced while encoding a stable session-store key.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum KeyEncodeError {
    #[error("session-store key field '{field}' is too large: {length} bytes")]
    FieldTooLarge { field: &'static str, length: usize },
}

/// Error returned by the MQTT 3.1.1 file-store adapter.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum SessionFileStoreError {
    #[error("file store: {0}")]
    FileStore(#[from] FileStoreError),
    #[error("canonical session encoding failed: {0}")]
    SessionEncode(#[from] SessionEncodeError),
    #[error("canonical session decoding failed: {0}")]
    SessionDecode(#[from] SessionDecodeError),
    #[error("session-store key encoding failed: {0}")]
    KeyEncode(#[from] KeyEncodeError),
    #[error(
        "legacy checkpoint detected at {diagnostic_path:?}; explicit operator recovery is required"
    )]
    LegacyCheckpointDetected { diagnostic_path: PathBuf },
}

/// File-backed MQTT 3.1.1 session store.
#[derive(Clone, Debug)]
pub struct SessionFileStore {
    core: FileStore,
}

impl SessionFileStore {
    /// Opens the `v4` namespace below an existing configured root.
    ///
    /// # Errors
    ///
    /// Returns key-neutral core construction errors, including unsupported
    /// platforms and invalid root or namespace filesystem state.
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self, SessionFileStoreError> {
        Self::open_with_options(root, FileStoreOptions::default()).await
    }

    /// Opens the `v4` namespace with explicit size limits.
    ///
    /// # Errors
    ///
    /// Returns an error when options are invalid or the core store cannot be
    /// constructed with the required filesystem guarantees.
    pub async fn open_with_options(
        root: impl Into<PathBuf>,
        options: FileStoreOptions,
    ) -> Result<Self, SessionFileStoreError> {
        Ok(Self {
            core: FileStore::open(root, NAMESPACE, options).await?,
        })
    }

    /// Returns the canonical path used for `key`.
    ///
    /// # Errors
    ///
    /// Returns [`KeyEncodeError`] when a key field exceeds the stable format.
    pub fn checkpoint_path(&self, key: &SessionStoreKey) -> Result<PathBuf, KeyEncodeError> {
        Ok(self.core.checkpoint_path(&encode_session_store_key(key)?))
    }

    /// Inspects the canonical checkpoint without decoding it.
    ///
    /// # Errors
    ///
    /// Returns key-encoding, filesystem, or coordination failures.
    pub async fn inspect(
        &self,
        key: &SessionStoreKey,
    ) -> Result<CheckpointInspection, SessionFileStoreError> {
        Ok(self
            .core
            .inspect_with_legacy_filename(
                &encode_session_store_key(key)?,
                legacy_example_filename(key),
            )
            .await?)
    }

    /// Moves the canonical checkpoint aside for diagnosis.
    ///
    /// # Errors
    ///
    /// Returns an error when the source is missing or the atomic move is not confirmed.
    pub async fn quarantine(
        &self,
        key: &SessionStoreKey,
    ) -> Result<QuarantineInfo, SessionFileStoreError> {
        Ok(self
            .core
            .quarantine(&encode_session_store_key(key)?)
            .await?)
    }

    /// Explicitly clears canonical local state during operator-led recovery.
    ///
    /// # Errors
    ///
    /// Returns key-encoding, filesystem, or coordination failures.
    pub async fn operator_clear(&self, key: &SessionStoreKey) -> Result<(), SessionFileStoreError> {
        Ok(self.core.clear(&encode_session_store_key(key)?).await?)
    }

    /// Cleans stale store-owned Windows staging files behind a global barrier.
    ///
    /// # Errors
    ///
    /// Returns invalid-age, unsupported-platform, filesystem, or coordination failures.
    pub async fn cleanup_stale_temporary_files_before_use(
        &self,
        minimum_age: Duration,
    ) -> Result<CleanupReport, SessionFileStoreError> {
        Ok(self
            .core
            .cleanup_stale_temporary_files_before_use(minimum_age)
            .await?)
    }

    /// Returns the namespace component used by this adapter.
    #[must_use]
    pub fn namespace_name() -> &'static Path {
        Path::new(NAMESPACE)
    }
}

/// Encodes a complete v4 key as:
/// `version:u8 || protocol:u8 || scope_len:u32_be || scope || client_id_len:u32_be || client_id`.
///
/// # Errors
///
/// Returns [`KeyEncodeError`] when a key field exceeds `u32::MAX` bytes.
pub fn encode_session_store_key(key: &SessionStoreKey) -> Result<Vec<u8>, KeyEncodeError> {
    let scope = key.scope().as_bytes();
    let client_id = key.client_id().as_bytes();
    let scope_len = u32::try_from(scope.len()).map_err(|_| KeyEncodeError::FieldTooLarge {
        field: "scope",
        length: scope.len(),
    })?;
    let client_id_len =
        u32::try_from(client_id.len()).map_err(|_| KeyEncodeError::FieldTooLarge {
            field: "client_id",
            length: client_id.len(),
        })?;
    let mut bytes = Vec::with_capacity(10 + scope.len() + client_id.len());
    bytes.push(KEY_FORMAT_VERSION);
    bytes.push(PROTOCOL_TAG);
    bytes.extend_from_slice(&scope_len.to_be_bytes());
    bytes.extend_from_slice(scope);
    bytes.extend_from_slice(&client_id_len.to_be_bytes());
    bytes.extend_from_slice(client_id);
    Ok(bytes)
}

/// Returns the stable hashed filename for a v4 store key.
///
/// # Errors
///
/// Returns [`KeyEncodeError`] when the key cannot be represented.
pub fn session_filename(key: &SessionStoreKey) -> Result<String, KeyEncodeError> {
    Ok(checkpoint_filename(&encode_session_store_key(key)?))
}

/// Returns the one exact filename used by the repository's former example store.
#[must_use]
pub fn legacy_example_filename(key: &SessionStoreKey) -> String {
    format!(
        "{}.{}.session",
        hex(key.scope().as_bytes()),
        hex(key.client_id().as_bytes())
    )
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

impl SessionStore for SessionFileStore {
    fn load<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, Option<PersistedSession>> {
        let legacy_filename = legacy_example_filename(key);
        let key = match encode_session_store_key(key) {
            Ok(key) => key,
            Err(error) => return boxed_error_future(SessionFileStoreError::from(error)),
        };
        let load = self.core.load_with_legacy_filename(&key, legacy_filename);
        Box::pin(async move {
            let payload = match load
                .await
                .map_err(SessionFileStoreError::from)
                .map_err(box_error)?
            {
                LegacyLoad::Absent => None,
                LegacyLoad::Present(payload) => Some(payload),
                LegacyLoad::LegacyDetected { diagnostic_path } => {
                    return Err(box_error(SessionFileStoreError::LegacyCheckpointDetected {
                        diagnostic_path,
                    }));
                }
            };
            payload
                .map(|payload| PersistedSession::decode(&payload))
                .transpose()
                .map_err(SessionFileStoreError::from)
                .map_err(box_error)
        })
    }

    fn save<'a>(
        &'a self,
        key: &'a SessionStoreKey,
        session: &'a PersistedSession,
    ) -> StoreFuture<'a, ()> {
        let key = match encode_session_store_key(key) {
            Ok(key) => key,
            Err(error) => return boxed_error_future(SessionFileStoreError::from(error)),
        };
        let payload = match session.encode() {
            Ok(payload) => payload,
            Err(error) => return boxed_error_future(SessionFileStoreError::from(error)),
        };
        let save = self.core.save(&key, payload);
        Box::pin(async move {
            save.await
                .map_err(SessionFileStoreError::from)
                .map_err(box_error)
        })
    }

    fn clear<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, ()> {
        let key = match encode_session_store_key(key) {
            Ok(key) => key,
            Err(error) => return boxed_error_future(SessionFileStoreError::from(error)),
        };
        let clear = self.core.clear(&key);
        Box::pin(async move {
            clear
                .await
                .map_err(SessionFileStoreError::from)
                .map_err(box_error)
        })
    }
}

fn boxed_error_future<T: Send + 'static>(error: SessionFileStoreError) -> StoreFuture<'static, T> {
    Box::pin(async move { Err(box_error(error)) })
}

fn box_error(error: SessionFileStoreError) -> rumqttc::SessionStoreError {
    Box::new(error)
}

#[cfg(test)]
mod tests;
