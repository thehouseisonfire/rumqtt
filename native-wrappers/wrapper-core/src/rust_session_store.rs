//! Opt-in adapters for Rust wrapper authors with an existing native store.
//!
//! This module deliberately depends on protocol crate traits; it is not the
//! host-neutral storage contract. Reuse the returned `Arc` for all clients using
//! the same store so wrapper key leases share one identity. The native store must
//! not also be used by an independently driven event loop for the same key.
//! Calls inherit the deadlines, panic containment, and ownership contract of
//! [`SessionStore`]. Native error text is discarded to avoid exposing secrets.

use std::sync::Arc;

use crate::backend::session::{envelope, payload};
use crate::{
    ProtocolVersion, SessionCheckpoint, SessionStore, SessionStoreKey, StoreFailure, StoreFuture,
};

// This ceiling also applies when an adapter is used outside NativeClient. The
// client's configured (usually smaller) bound is enforced by its outer adapter.
const MAX_CHECKPOINT_SIZE: usize = 256 * 1024 * 1024;

macro_rules! adapter {
    ($constructor:ident, $name:ident, $native:ident, $version:ident, $tag:literal) => {
        /// Wraps a native store for use with `SessionStoreConfig::new`.
        pub fn $constructor(store: Arc<dyn $native::SessionStore>) -> Arc<dyn SessionStore> {
            Arc::new($name(store))
        }

        struct $name(Arc<dyn $native::SessionStore>);

        impl SessionStore for $name {
            fn load(&self, key: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
                let store = Arc::clone(&self.0);
                Box::pin(async move {
                    if key.protocol != ProtocolVersion::$version {
                        return Err(StoreFailure::Protocol);
                    }
                    let key = $native::SessionStoreKey::new(key.scope, key.client_id);
                    store
                        .load(&key)
                        .await
                        .map_err(|_| StoreFailure::Load)?
                        .map(|session| {
                            let bytes = session.encode().map_err(|_| StoreFailure::Oversized)?;
                            envelope(bytes, $tag, MAX_CHECKPOINT_SIZE)
                        })
                        .transpose()
                })
            }

            fn save(&self, key: SessionStoreKey, checkpoint: SessionCheckpoint) -> StoreFuture<()> {
                let store = Arc::clone(&self.0);
                Box::pin(async move {
                    if key.protocol != ProtocolVersion::$version {
                        return Err(StoreFailure::Protocol);
                    }
                    let bytes = payload(checkpoint, $tag, MAX_CHECKPOINT_SIZE)?;
                    let session =
                        $native::PersistedSession::decode(&bytes).map_err(|error| match error {
                            $native::SessionDecodeError::WrongProtocol { .. } => {
                                StoreFailure::Protocol
                            }
                            $native::SessionDecodeError::UnsupportedCodecVersion { .. } => {
                                StoreFailure::Version
                            }
                            $native::SessionDecodeError::LimitExceeded { .. } => {
                                StoreFailure::Oversized
                            }
                            _ => StoreFailure::Corrupt,
                        })?;
                    let key = $native::SessionStoreKey::new(key.scope, key.client_id);
                    store
                        .save(&key, &session)
                        .await
                        .map_err(|_| StoreFailure::Save)
                })
            }

            fn clear(&self, key: SessionStoreKey) -> StoreFuture<()> {
                let store = Arc::clone(&self.0);
                Box::pin(async move {
                    if key.protocol != ProtocolVersion::$version {
                        return Err(StoreFailure::Protocol);
                    }
                    let key = $native::SessionStoreKey::new(key.scope, key.client_id);
                    store.clear(&key).await.map_err(|_| StoreFailure::Clear)
                })
            }
        }
    };
}

adapter!(from_v4, V4Store, rumqttc_v4, V4, 4);
adapter!(from_v5, V5Store, rumqttc_v5, V5, 5);
