//! MQTT 3.1.1 file-backed session store.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use rumqttc_session_store_file_core_next::FileStoreError;
use rumqttc_v4::{
    PersistedSession, SessionDecodeError, SessionEncodeError, SessionStore, SessionStoreKey,
};

use crate::shared::{self, Adapter, AdapterError, Protocol, Sealed};
pub use crate::{
    CheckpointInspection, CheckpointState, CleanupReport, FileStoreOptions, KeyEncodeError,
    QuarantineInfo,
};

type StoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, rumqttc_v4::SessionStoreError>> + Send + 'a>>;

struct V4;

impl Sealed for V4 {}

impl Protocol for V4 {
    type Session = PersistedSession;
    type EncodeError = SessionEncodeError;
    type DecodeError = SessionDecodeError;

    const TAG: u8 = 4;
    const NAMESPACE: &'static str = "v4";

    fn encode(session: &Self::Session) -> Result<Vec<u8>, Self::EncodeError> {
        session.encode()
    }

    fn decode(payload: &[u8]) -> Result<Self::Session, Self::DecodeError> {
        PersistedSession::decode(payload)
    }
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

impl From<AdapterError<V4>> for SessionFileStoreError {
    fn from(error: AdapterError<V4>) -> Self {
        match error {
            AdapterError::FileStore(error) => Self::FileStore(error),
            AdapterError::SessionEncode(error) => Self::SessionEncode(error),
            AdapterError::SessionDecode(error) => Self::SessionDecode(error),
            AdapterError::KeyEncode(error) => Self::KeyEncode(error),
            AdapterError::LegacyCheckpointDetected { diagnostic_path } => {
                Self::LegacyCheckpointDetected { diagnostic_path }
            }
        }
    }
}

/// File-backed MQTT 3.1.1 session store using the stable `v4` namespace.
#[derive(Clone, Debug)]
pub struct SessionFileStore {
    inner: Adapter<V4>,
}

impl SessionFileStore {
    /// Opens the `v4` namespace below an existing configured root.
    ///
    /// # Errors
    /// Returns an error if the root or namespace cannot be opened safely.
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self, SessionFileStoreError> {
        Self::open_with_options(root, FileStoreOptions::default()).await
    }

    /// Opens the `v4` namespace with explicit size limits.
    ///
    /// # Errors
    /// Returns an error for invalid options or filesystem state.
    pub async fn open_with_options(
        root: impl Into<PathBuf>,
        options: FileStoreOptions,
    ) -> Result<Self, SessionFileStoreError> {
        Ok(Self {
            inner: Adapter::open(root, options)
                .await
                .map_err(SessionFileStoreError::from)?,
        })
    }

    /// Returns the canonical path used for `key`.
    ///
    /// # Errors
    /// Returns an error if a key field exceeds the stable encoding limit.
    pub fn checkpoint_path(&self, key: &SessionStoreKey) -> Result<PathBuf, KeyEncodeError> {
        self.inner.checkpoint_path(key.scope(), key.client_id())
    }

    /// Inspects the canonical checkpoint without decoding it.
    ///
    /// # Errors
    /// Returns an error for key encoding, filesystem, or coordination failures.
    pub async fn inspect(
        &self,
        key: &SessionStoreKey,
    ) -> Result<CheckpointInspection, SessionFileStoreError> {
        self.inner
            .inspect(key.scope(), key.client_id())
            .await
            .map_err(SessionFileStoreError::from)
    }

    /// Moves the canonical checkpoint aside for diagnosis.
    ///
    /// # Errors
    /// Returns an error if the checkpoint cannot be moved atomically.
    pub async fn quarantine(
        &self,
        key: &SessionStoreKey,
    ) -> Result<QuarantineInfo, SessionFileStoreError> {
        self.inner
            .quarantine(key.scope(), key.client_id())
            .await
            .map_err(SessionFileStoreError::from)
    }

    /// Explicitly clears canonical local state during operator-led recovery.
    ///
    /// # Errors
    /// Returns an error for key encoding, filesystem, or coordination failures.
    pub async fn operator_clear(&self, key: &SessionStoreKey) -> Result<(), SessionFileStoreError> {
        self.inner
            .clear(key.scope(), key.client_id())
            .await
            .map_err(SessionFileStoreError::from)
    }

    /// Cleans stale store-owned Windows staging files behind a global barrier.
    ///
    /// # Errors
    /// Returns an error for an invalid age, unsupported platform, or filesystem failure.
    pub async fn cleanup_stale_temporary_files_before_use(
        &self,
        minimum_age: Duration,
    ) -> Result<CleanupReport, SessionFileStoreError> {
        self.inner
            .cleanup(minimum_age)
            .await
            .map_err(SessionFileStoreError::from)
    }

    /// Returns the namespace component used by this adapter.
    #[must_use]
    pub fn namespace_name() -> &'static Path {
        shared::namespace::<V4>()
    }
}

/// Encodes a complete MQTT 3.1.1 session-store key in the stable version 1 format.
///
/// # Errors
/// Returns an error if a key field exceeds `u32::MAX` bytes.
pub fn encode_session_store_key(key: &SessionStoreKey) -> Result<Vec<u8>, KeyEncodeError> {
    shared::encode_key::<V4>(key.scope(), key.client_id())
}

/// Returns the stable hashed filename for an MQTT 3.1.1 store key.
///
/// # Errors
/// Returns an error if the key cannot be represented.
pub fn session_filename(key: &SessionStoreKey) -> Result<String, KeyEncodeError> {
    shared::filename::<V4>(key.scope(), key.client_id())
}

/// Returns the one exact filename used by the repository's former example store.
#[must_use]
pub fn legacy_example_filename(key: &SessionStoreKey) -> String {
    shared::legacy_filename(key.scope(), key.client_id())
}

impl SessionStore for SessionFileStore {
    fn load<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, Option<PersistedSession>> {
        Box::pin(async move {
            self.inner
                .load(key.scope(), key.client_id())
                .await
                .map_err(SessionFileStoreError::from)
                .map_err(box_error)
        })
    }

    fn save<'a>(
        &'a self,
        key: &'a SessionStoreKey,
        session: &'a PersistedSession,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.inner
                .save(key.scope(), key.client_id(), session)
                .await
                .map_err(SessionFileStoreError::from)
                .map_err(box_error)
        })
    }

    fn clear<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.inner
                .clear(key.scope(), key.client_id())
                .await
                .map_err(SessionFileStoreError::from)
                .map_err(box_error)
        })
    }
}

fn box_error(error: SessionFileStoreError) -> rumqttc_v4::SessionStoreError {
    Box::new(error)
}

#[cfg(test)]
mod tests;
