//! Production-oriented Unix file store for MQTT 5 `rumqttc-next` sessions.
//!
//! This adapter encodes the complete [`rumqttc::SessionStoreKey`] into a stable
//! byte format and delegates opaque canonical [`rumqttc::PersistedSession`]
//! bytes to the protocol-neutral core store.
//!
//! # Limitations
//!
//! The backend is Unix-only and assumes an existing, trusted local-filesystem
//! root. It provides process-local same-key coordination but no cross-process
//! locking or multi-writer protection, encryption, authentication, or tamper
//! resistance. It ignores and does not clean up stale dependency temporary
//! files, and its synchronization operations are not a universal power-loss
//! guarantee.
//!
//! The `v5` hashed, versioned layout is incompatible with files written by the
//! repository's earlier example store. No legacy detection or migration is
//! performed; use a dedicated new root and deliberately reconcile broker-held
//! state before abandoning old local persistence.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use rumqttc::{
    PersistedSession, SessionDecodeError, SessionEncodeError, SessionStore, SessionStoreKey,
};
use rumqttc_session_store_file_core_next::{
    FileStore, FileStoreError, FileStoreOptions, checkpoint_filename,
};

const KEY_FORMAT_VERSION: u8 = 1;
const PROTOCOL_TAG: u8 = 5;
const NAMESPACE: &str = "v5";

type StoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, rumqttc::SessionStoreError>> + Send + 'a>>;

/// Error produced while encoding a stable session-store key.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum KeyEncodeError {
    #[error("session-store key field '{field}' is too large: {length} bytes")]
    FieldTooLarge { field: &'static str, length: usize },
}

/// Error returned by the MQTT 5 file-store adapter.
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
}

/// File-backed MQTT 5 session store.
#[derive(Clone, Debug)]
pub struct SessionFileStore {
    core: FileStore,
}

impl SessionFileStore {
    /// Opens the `v5` namespace below an existing configured root.
    ///
    /// # Errors
    ///
    /// Returns key-neutral core construction errors, including unsupported
    /// platforms and invalid root or namespace filesystem state.
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self, SessionFileStoreError> {
        Self::open_with_options(root, FileStoreOptions::default()).await
    }

    /// Opens the `v5` namespace with explicit size limits.
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

    /// Returns the namespace component used by this adapter.
    #[must_use]
    pub fn namespace_name() -> &'static Path {
        Path::new(NAMESPACE)
    }
}

/// Encodes a complete v5 key as:
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

/// Returns the stable hashed filename for a v5 store key.
///
/// # Errors
///
/// Returns [`KeyEncodeError`] when the key cannot be represented.
pub fn session_filename(key: &SessionStoreKey) -> Result<String, KeyEncodeError> {
    Ok(checkpoint_filename(&encode_session_store_key(key)?))
}

impl SessionStore for SessionFileStore {
    fn load<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, Option<PersistedSession>> {
        let key = match encode_session_store_key(key) {
            Ok(key) => key,
            Err(error) => return boxed_error_future(SessionFileStoreError::from(error)),
        };
        let load = self.core.load(&key);
        Box::pin(async move {
            let payload = load
                .await
                .map_err(SessionFileStoreError::from)
                .map_err(box_error)?;
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
