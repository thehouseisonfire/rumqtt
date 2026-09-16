use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use crate::{Error, ProtocolVersion, Result};

/// Storage identity. Scope is a deployment/tenant boundary, not a network address.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionStoreKey {
    pub protocol: ProtocolVersion,
    pub scope: String,
    pub client_id: String,
}

/// Owned, opaque protocol checkpoint. The envelope is versioned independently
/// of the protocol model. Hosts must store the bytes unchanged.
/// Envelope version 1 wraps the native protocol codec's independently versioned
/// bytes. Patch releases preserve this format. Minor releases may advance either
/// version: unsupported versions fail with [`StoreFailure::Version`] and require
/// explicit application migration or invalidation; they are never replayed.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionCheckpoint(pub Bytes);

impl std::fmt::Debug for SessionCheckpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCheckpoint")
            .field("length", &self.0.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreFailure {
    #[error("session store load failed")]
    Load,
    #[error("session store save failed")]
    Save,
    #[error("session store clear failed")]
    Clear,
    #[error("session checkpoint is corrupt")]
    Corrupt,
    #[error("session checkpoint version is unsupported; migration or invalidation is required")]
    Version,
    #[error("session checkpoint protocol does not match the client")]
    Protocol,
    #[error("session checkpoint exceeds the configured size limit")]
    Oversized,
    #[error("session store callback timed out")]
    Timeout,
    #[error("session store callback panicked")]
    Panic,
    #[error("session store key already has an active client")]
    InUse,
}

pub type StoreFuture<T> =
    Pin<Box<dyn Future<Output = std::result::Result<T, StoreFailure>> + Send + 'static>>;

/// Host-neutral crash-consistent checkpoint storage.
///
/// Calls for one client are serialized on its driver thread. Different clients
/// may call the same owner concurrently. Methods must return promptly; futures
/// must yield instead of blocking and may use the driver's Tokio runtime. No
/// wrapper lifecycle lock is held during a call. Calling the same `ClientHandle`
/// is permitted, but waiting for its MQTT completion would deadlock the driver.
///
/// Each call has [`SessionStoreConfig::timeout`]. Cancellation drops the future;
/// detached host work must own its inputs and ignore late completion. A cancelled
/// save or clear may have committed, but must never leave a torn checkpoint.
/// Panics in method invocation or polling are contained and terminate the client
/// as a typed persistence failure. Panic payloads and host error strings are not
/// exposed. Destructors must not panic or block.
///
/// The driver's owner reference is released on failed start, joined close,
/// abandonment cleanup, or terminal driver failure. Host-held configuration
/// clones retain their own references. The in-process key lease lasts until the
/// driver drops its adapter; cross-process exclusion remains the store's duty.
pub trait SessionStore: Send + Sync + 'static {
    fn load(&self, key: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>>;
    fn save(&self, key: SessionStoreKey, checkpoint: SessionCheckpoint) -> StoreFuture<()>;
    fn clear(&self, key: SessionStoreKey) -> StoreFuture<()>;
}

#[derive(Clone)]
pub struct SessionStoreConfig {
    pub store: Arc<dyn SessionStore>,
    pub scope: String,
    pub timeout: Duration,
    pub max_checkpoint_size: usize,
}

impl SessionStoreConfig {
    pub fn new(store: Arc<dyn SessionStore>, scope: impl Into<String>) -> Self {
        Self {
            store,
            scope: scope.into(),
            timeout: Duration::from_secs(5),
            max_checkpoint_size: 16 * 1024 * 1024,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.timeout.is_zero()
            || std::time::Instant::now()
                .checked_add(self.timeout)
                .is_none()
        {
            return Err(Error::configuration("invalid session store timeout"));
        }
        if self.max_checkpoint_size < 8 || self.max_checkpoint_size > 256 * 1024 * 1024 {
            return Err(Error::configuration(
                "checkpoint size limit must be between 8 bytes and 256 MiB",
            ));
        }
        if self.scope.is_empty() || self.scope.contains('\0') {
            return Err(Error::configuration(
                "session store scope must be nonempty and contain no NUL",
            ));
        }
        Ok(())
    }
}

impl std::fmt::Debug for SessionStoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionStoreConfig")
            .field("scope", &self.scope)
            .field("timeout", &self.timeout)
            .field("max_checkpoint_size", &self.max_checkpoint_size)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SessionStoreConfig {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.store, &other.store)
            && self.scope == other.scope
            && self.timeout == other.timeout
            && self.max_checkpoint_size == other.max_checkpoint_size
    }
}
impl Eq for SessionStoreConfig {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrokerSessionResumePolicy {
    #[default]
    Strict,
    AllowBrokerOnly,
}
