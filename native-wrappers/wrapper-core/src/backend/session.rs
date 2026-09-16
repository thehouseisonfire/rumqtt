use std::collections::HashSet;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, OnceLock};

use bytes::{Bytes, BytesMut};
use futures_util::FutureExt;

use crate::{
    ProtocolVersion, SessionCheckpoint, SessionStoreConfig, SessionStoreKey, StoreFailure,
    StoreFuture,
};

type LeaseKey = (usize, SessionStoreKey);
type NativeFuture<'a, T> = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<T, Box<dyn std::error::Error + Send + Sync>>>
            + Send
            + 'a,
    >,
>;
static LEASES: OnceLock<Mutex<HashSet<LeaseKey>>> = OnceLock::new();

/// One adapter belongs to one driver and holds a lease until the backend drops it.
#[derive(Debug)]
pub(super) struct Adapter {
    config: SessionStoreConfig,
    lease: LeaseKey,
}

impl Adapter {
    pub(super) fn new(
        config: SessionStoreConfig,
        protocol: ProtocolVersion,
        client_id: &str,
    ) -> crate::Result<Self> {
        let key = SessionStoreKey {
            protocol,
            scope: config.scope.clone(),
            client_id: client_id.into(),
        };
        let identity = Arc::as_ptr(&config.store).cast::<()>() as usize;
        let lease = (identity, key);
        if !LEASES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(lease.clone())
        {
            return Err(crate::Error::store(StoreFailure::InUse));
        }
        Ok(Self { config, lease })
    }

    async fn call<T>(&self, invoke: impl FnOnce() -> StoreFuture<T>) -> Result<T, StoreFailure> {
        // Catch both method invocation and future polling. No app callback runs
        // under LEASES or wrapper admission/lifecycle locks.
        let work = async {
            tokio::time::timeout(self.config.timeout, invoke())
                .await
                .map_err(|_| StoreFailure::Timeout)?
        };
        tokio::pin!(work);
        let guarded = std::future::poll_fn(|cx| {
            crate::runtime::with_host_callback(|| work.as_mut().poll(cx))
        });
        match AssertUnwindSafe(guarded).catch_unwind().await {
            Ok(result) => result,
            Err(payload) => {
                // A host panic payload can have a panicking destructor.
                std::mem::forget(payload);
                Err(StoreFailure::Panic)
            }
        }
    }

    fn envelope(&self, bytes: Vec<u8>) -> Result<SessionCheckpoint, StoreFailure> {
        envelope(bytes, self.protocol_byte(), self.config.max_checkpoint_size)
    }

    fn payload(&self, checkpoint: SessionCheckpoint) -> Result<Bytes, StoreFailure> {
        payload(
            checkpoint,
            self.protocol_byte(),
            self.config.max_checkpoint_size,
        )
    }

    const fn protocol_byte(&self) -> u8 {
        match self.lease.1.protocol {
            ProtocolVersion::V4 => 4,
            ProtocolVersion::V5 => 5,
        }
    }
}

pub fn envelope(
    bytes: Vec<u8>,
    protocol: u8,
    limit: usize,
) -> Result<SessionCheckpoint, StoreFailure> {
    if bytes.len().checked_add(8).is_none_or(|size| size > limit) {
        return Err(StoreFailure::Oversized);
    }
    let mut result = BytesMut::with_capacity(bytes.len() + 8);
    result.extend_from_slice(b"RMWC");
    result.extend_from_slice(&[0, 1, protocol, 0]);
    result.extend_from_slice(&bytes);
    Ok(SessionCheckpoint(result.freeze()))
}

pub fn payload(
    checkpoint: SessionCheckpoint,
    protocol: u8,
    limit: usize,
) -> Result<Bytes, StoreFailure> {
    let bytes = checkpoint.0;
    if bytes.len() > limit {
        return Err(StoreFailure::Oversized);
    }
    if bytes.len() < 8 || &bytes[..4] != b"RMWC" {
        return Err(StoreFailure::Corrupt);
    }
    if bytes[4..6] != [0, 1] || bytes[7] != 0 {
        return Err(StoreFailure::Version);
    }
    if bytes[6] != protocol {
        return Err(StoreFailure::Protocol);
    }
    Ok(bytes.slice(8..))
}

impl Drop for Adapter {
    fn drop(&mut self) {
        LEASES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.lease);
    }
}

macro_rules! implement_store {
    ($backend:ident) => {
        impl $backend::SessionStore for Adapter {
            fn load<'a>(
                &'a self,
                _: &'a $backend::SessionStoreKey,
            ) -> NativeFuture<'a, Option<$backend::PersistedSession>> {
                Box::pin(async move {
                    let checkpoint = self
                        .call(|| self.config.store.load(self.lease.1.clone()))
                        .await?;
                    checkpoint
                        .map(|checkpoint| {
                            let bytes = self.payload(checkpoint)?;
                            $backend::PersistedSession::decode(&bytes).map_err(
                                |error| match error {
                                    $backend::SessionDecodeError::WrongProtocol { .. } => {
                                        StoreFailure::Protocol
                                    }
                                    $backend::SessionDecodeError::UnsupportedCodecVersion {
                                        ..
                                    } => StoreFailure::Version,
                                    $backend::SessionDecodeError::LimitExceeded { .. } => {
                                        StoreFailure::Oversized
                                    }
                                    _ => StoreFailure::Corrupt,
                                },
                            )
                        })
                        .transpose()
                        .map_err(|e| Box::new(e) as $backend::SessionStoreError)
                })
            }

            fn save<'a>(
                &'a self,
                _: &'a $backend::SessionStoreKey,
                session: &'a $backend::PersistedSession,
            ) -> NativeFuture<'a, ()> {
                Box::pin(async move {
                    let bytes = session.encode().map_err(|_| StoreFailure::Oversized)?;
                    let checkpoint = self.envelope(bytes)?;
                    self.call(|| self.config.store.save(self.lease.1.clone(), checkpoint))
                        .await
                        .map_err(|e| Box::new(e) as $backend::SessionStoreError)
                })
            }

            fn clear<'a>(&'a self, _: &'a $backend::SessionStoreKey) -> NativeFuture<'a, ()> {
                Box::pin(async move {
                    self.call(|| self.config.store.clear(self.lease.1.clone()))
                        .await
                        .map_err(|e| Box::new(e) as $backend::SessionStoreError)
                })
            }
        }
    };
}

implement_store!(rumqttc_v4);
implement_store!(rumqttc_v5);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionStore;

    #[derive(Default)]
    struct Memory(Mutex<Option<SessionCheckpoint>>);
    impl SessionStore for Memory {
        fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
            let value = self.0.lock().unwrap().clone();
            Box::pin(async move { Ok(value) })
        }
        fn save(&self, _: SessionStoreKey, checkpoint: SessionCheckpoint) -> StoreFuture<()> {
            *self.0.lock().unwrap() = Some(checkpoint);
            Box::pin(async { Ok(()) })
        }
        fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
            *self.0.lock().unwrap() = None;
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn checkpoints_round_trip_full_protocol_models_across_adapter_recreation() {
        use rumqttc_v4 as v4;
        use rumqttc_v5 as v5;
        let memory = Arc::new(Memory::default());
        let config = SessionStoreConfig::new(memory, "scope");
        let session = v4::PersistedSession {
            format_version: 2,
            client_id: "client".into(),
            clean_session: false,
            max_inflight: 100,
            ack_mode: v4::PersistedAckMode::Automatic,
            replay: vec![
                v4::PersistedRequest::Publish(v4::PersistedPublish {
                    dup: true,
                    qos: v4::PersistedQoS::AtLeastOnce,
                    retain: true,
                    topic: b"topic".to_vec(),
                    pkid: 11,
                    payload: vec![0, 255],
                }),
                v4::PersistedRequest::PubRel(v4::PersistedPubRel { pkid: 12 }),
                v4::PersistedRequest::Subscribe(v4::PersistedSubscribe {
                    pkid: 13,
                    filters: vec![v4::PersistedFilter {
                        path: "filter/+".into(),
                        qos: v4::PersistedQoS::ExactlyOnce,
                    }],
                }),
                v4::PersistedRequest::Unsubscribe(v4::PersistedUnsubscribe {
                    pkid: 14,
                    topics: vec!["other/#".into()],
                }),
            ],
            incoming_qos2: vec![v4::PersistedIncomingQos2 { pkid: 15 }],
        };
        let key = v4::SessionStoreKey::new("scope", "client");
        let adapter = Adapter::new(config.clone(), ProtocolVersion::V4, "client").unwrap();
        v4::SessionStore::save(&adapter, &key, &session)
            .await
            .unwrap();
        drop(adapter);
        let adapter = Adapter::new(config.clone(), ProtocolVersion::V4, "client").unwrap();
        assert_eq!(
            v4::SessionStore::load(&adapter, &key).await.unwrap(),
            Some(session.clone())
        );
        let adapter = crate::rust_session_store::from_v4(Arc::new(adapter));
        let owned_key = SessionStoreKey {
            protocol: ProtocolVersion::V4,
            scope: "scope".into(),
            client_id: "client".into(),
        };
        let checkpoint = adapter.load(owned_key.clone()).await.unwrap().unwrap();
        adapter.clear(owned_key.clone()).await.unwrap();
        assert!(adapter.load(owned_key.clone()).await.unwrap().is_none());
        adapter.save(owned_key.clone(), checkpoint).await.unwrap();
        assert!(adapter.load(owned_key).await.unwrap().is_some());
        drop(adapter);
        let adapter = Adapter::new(config.clone(), ProtocolVersion::V4, "client").unwrap();
        v4::SessionStore::clear(&adapter, &key).await.unwrap();
        assert_eq!(v4::SessionStore::load(&adapter, &key).await.unwrap(), None);
        drop(adapter);

        let session = v5::PersistedSession {
            format_version: 2,
            client_id: "client".into(),
            clean_start: false,
            session_expiry_interval: Some(60),
            outgoing_inflight_upper_limit: None,
            ack_mode: v5::PersistedAckMode::Automatic,
            replay: vec![
                v5::PersistedRequest::Publish(v5::PersistedPublish {
                    dup: true,
                    qos: v5::PersistedQoS::ExactlyOnce,
                    retain: true,
                    topic: b"topic".to_vec(),
                    pkid: 11,
                    payload: vec![0, 255],
                    properties: None,
                }),
                v5::PersistedRequest::PubRel(v5::PersistedPubRel { pkid: 12 }),
                v5::PersistedRequest::Subscribe(v5::PersistedSubscribe {
                    pkid: 13,
                    filters: vec![v5::PersistedFilter {
                        path: "filter/+".into(),
                        qos: v5::PersistedQoS::ExactlyOnce,
                        nolocal: true,
                        preserve_retain: true,
                        retain_forward_rule: v5::PersistedRetainForwardRule::Never,
                    }],
                    properties: Some(v5::PersistedSubscribeProperties {
                        id: Some(7),
                        user_properties: vec![("key".into(), "value".into())],
                    }),
                }),
                v5::PersistedRequest::Unsubscribe(v5::PersistedUnsubscribe {
                    pkid: 14,
                    filters: vec!["other/#".into()],
                    properties: Some(v5::PersistedUnsubscribeProperties {
                        user_properties: vec![("key".into(), String::new())],
                    }),
                }),
            ],
            incoming_qos2: vec![v5::PersistedIncomingQos2 { pkid: 15 }],
        };
        let key = v5::SessionStoreKey::new("scope", "client");
        let adapter = Adapter::new(config.clone(), ProtocolVersion::V5, "client").unwrap();
        v5::SessionStore::save(&adapter, &key, &session)
            .await
            .unwrap();
        drop(adapter);
        let adapter = Adapter::new(config, ProtocolVersion::V5, "client").unwrap();
        assert_eq!(
            v5::SessionStore::load(&adapter, &key).await.unwrap(),
            Some(session)
        );
        let adapter = crate::rust_session_store::from_v5(Arc::new(adapter));
        let key = SessionStoreKey {
            protocol: ProtocolVersion::V5,
            scope: "scope".into(),
            client_id: "client".into(),
        };
        let checkpoint = adapter.load(key.clone()).await.unwrap().unwrap();
        adapter.clear(key.clone()).await.unwrap();
        assert!(adapter.load(key.clone()).await.unwrap().is_none());
        adapter.save(key.clone(), checkpoint).await.unwrap();
        assert!(adapter.load(key.clone()).await.unwrap().is_some());
        assert_eq!(
            adapter
                .load(SessionStoreKey {
                    protocol: ProtocolVersion::V4,
                    ..key
                })
                .await,
            Err(StoreFailure::Protocol)
        );
    }

    struct FailingStore(StoreFailure);

    impl SessionStore for FailingStore {
        fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
            Box::pin(async { Ok(None) })
        }

        fn save(&self, _: SessionStoreKey, _: SessionCheckpoint) -> StoreFuture<()> {
            let failure = self.0;
            Box::pin(async move { Err(failure) })
        }

        fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
            let failure = self.0;
            Box::pin(async move { Err(failure) })
        }
    }

    #[tokio::test]
    async fn mutations_preserve_typed_host_failures_for_both_protocols() {
        for protocol in [ProtocolVersion::V4, ProtocolVersion::V5] {
            for failure in [StoreFailure::Save, StoreFailure::Clear] {
                let config = SessionStoreConfig::new(Arc::new(FailingStore(failure)), "scope");
                let adapter = Adapter::new(config, protocol, "client").unwrap();
                // Exercise the same callback boundary used by native save/clear,
                // without requiring an unrelated network exchange.
                let result = if failure == StoreFailure::Save {
                    adapter
                        .call(|| {
                            adapter
                                .config
                                .store
                                .save(adapter.lease.1.clone(), SessionCheckpoint(Bytes::new()))
                        })
                        .await
                } else {
                    adapter
                        .call(|| adapter.config.store.clear(adapter.lease.1.clone()))
                        .await
                };
                assert_eq!(result, Err(failure));
            }
        }
    }
}
