use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use bytes::{Bytes, BytesMut};
use futures_util::FutureExt;

use crate::{
    ProtocolVersion, SessionCheckpoint, SessionStoreConfig, SessionStoreKey, StoreFailure,
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
    #[cfg(feature = "file-session-store")]
    _file_lease: Option<crate::FileStoreClientLease>,
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
        #[cfg(feature = "file-session-store")]
        let file_lease = config
            .store
            .file_store()
            .map(|store| store.acquire_client(&key))
            .transpose()?;
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
        Ok(Self {
            config,
            lease,
            #[cfg(feature = "file-session-store")]
            _file_lease: file_lease,
        })
    }

    async fn call<T, F>(&self, invoke: impl FnOnce() -> F) -> Result<T, StoreFailure>
    where
        F: Future<Output = Result<T, StoreFailure>>,
    {
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

    fn envelope(&self, bytes: &[u8]) -> Result<SessionCheckpoint, StoreFailure> {
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

/// Per-driver store factory; weak entries never retain an obsolete lease.
#[derive(Debug)]
pub(super) struct AdapterFactory {
    config: SessionStoreConfig,
    adapters: Mutex<HashMap<SessionStoreKey, Weak<Adapter>>>,
}
impl AdapterFactory {
    pub(super) fn new(config: SessionStoreConfig) -> Self {
        Self {
            config,
            adapters: Mutex::new(HashMap::new()),
        }
    }
    pub(super) fn prepare(&self, scope: &str, client_id: &str) -> crate::Result<Arc<Adapter>> {
        let key = SessionStoreKey {
            protocol: ProtocolVersion::V5,
            scope: scope.into(),
            client_id: client_id.into(),
        };
        let mut adapters = self
            .adapters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        adapters.retain(|_, adapter| adapter.strong_count() != 0);
        if let Some(adapter) = adapters.get(&key).and_then(Weak::upgrade) {
            return Ok(adapter);
        }
        let mut config = self.config.clone();
        config.scope = scope.into();
        config.validate()?;
        let adapter = Arc::new(Adapter::new(config, ProtocolVersion::V5, client_id)?);
        adapters.insert(key, Arc::downgrade(&adapter));
        drop(adapters);
        Ok(adapter)
    }
}

pub fn envelope(
    bytes: &[u8],
    protocol: u8,
    limit: usize,
) -> Result<SessionCheckpoint, StoreFailure> {
    if bytes.len().checked_add(8).is_none_or(|size| size > limit) {
        return Err(StoreFailure::Oversized);
    }
    let mut result = BytesMut::with_capacity(bytes.len() + 8);
    result.extend_from_slice(b"RMWC");
    result.extend_from_slice(&[0, 1, protocol, 0]);
    result.extend_from_slice(bytes);
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
        let mut leases = LEASES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        leases.remove(&self.lease);
        if leases.is_empty() {
            // Release process-wide bookkeeping when the final store client exits.
            leases.shrink_to_fit();
        }
    }
}

macro_rules! implement_store {
    ($backend:ident, $protocol:ident) => {
        impl $backend::SessionStore for Adapter {
            fn load<'a>(
                &'a self,
                key: &'a $backend::SessionStoreKey,
            ) -> NativeFuture<'a, Option<$backend::PersistedSession>> {
                Box::pin(async move {
                    if self.lease.1.protocol != ProtocolVersion::$protocol
                        || key.scope() != self.lease.1.scope
                        || key.client_id() != self.lease.1.client_id
                    {
                        return Err(
                            Box::new(StoreFailure::KeyMismatch) as $backend::SessionStoreError
                        );
                    }
                    self.call(|| async {
                        // Loading and queued decoding share one persistence deadline.
                        let checkpoint = self.config.store.load_with_limit(
                            self.lease.1.clone(),
                            self.config.max_checkpoint_size,
                        ).await?;
                        #[cfg(feature = "file-session-store")]
                        if let Some(store) = self.config.store.file_store() {
                            let protocol = self.protocol_byte();
                            let limit = self.config.max_checkpoint_size;
                            let bytes = checkpoint.as_ref().map_or(0, |c| c.0.len());
                            let memory = crate::file_store::decode_budget(limit, bytes);
                            return store.codec(
                                self.lease.1.clone(),
                                bytes.saturating_mul(4).saturating_add(memory),
                                move || {
                                    checkpoint.map(|checkpoint| {
                                        let bytes = payload(checkpoint, protocol, limit)?;
                                        $backend::PersistedSession::decode_with_memory_limit(&bytes, memory)
                                            .map_err(|error| match error {
                                                $backend::SessionDecodeError::WrongProtocol { .. } => {
                                                    StoreFailure::Protocol
                                                }
                                                $backend::SessionDecodeError::UnsupportedCodecVersion { .. } => {
                                                    StoreFailure::Version
                                                }
                                                $backend::SessionDecodeError::LimitExceeded { .. } => {
                                                    StoreFailure::Oversized
                                                }
                                                _ => StoreFailure::Corrupt,
                                            })
                                    }).transpose()
                                },
                            ).await;
                        }
                        checkpoint.map(|checkpoint| {
                            let bytes = self.payload(checkpoint)?;
                            $backend::PersistedSession::decode(&bytes).map_err(|error| match error {
                                $backend::SessionDecodeError::WrongProtocol { .. } => {
                                    StoreFailure::Protocol
                                }
                                $backend::SessionDecodeError::UnsupportedCodecVersion { .. } => {
                                    StoreFailure::Version
                                }
                                $backend::SessionDecodeError::LimitExceeded { .. } => {
                                    StoreFailure::Oversized
                                }
                                _ => StoreFailure::Corrupt,
                            })
                        }).transpose()
                    }).await.map_err(|e| Box::new(e) as $backend::SessionStoreError)
                })
            }

            fn save<'a>(
                &'a self,
                key: &'a $backend::SessionStoreKey,
                session: &'a $backend::PersistedSession,
            ) -> NativeFuture<'a, ()> {
                Box::pin(async move {
                    if self.lease.1.protocol != ProtocolVersion::$protocol
                        || key.scope() != self.lease.1.scope
                        || key.client_id() != self.lease.1.client_id
                    {
                        return Err(
                            Box::new(StoreFailure::KeyMismatch) as $backend::SessionStoreError
                        );
                    }
                    let bytes = session.encode().map_err(|_| StoreFailure::Oversized)?;
                    let checkpoint = self.envelope(&bytes)?;
                    self.call(|| self.config.store.save(self.lease.1.clone(), checkpoint))
                        .await
                        .map_err(|e| Box::new(e) as $backend::SessionStoreError)
                })
            }

            fn save_owned<'a>(
                &'a self, key: &'a $backend::SessionStoreKey, session: $backend::PersistedSession,
            ) -> NativeFuture<'a, ()> {
                Box::pin(async move {
                    #[cfg(feature = "file-session-store")]
                    if let Some(store) = self.config.store.file_store() {
                        if self.lease.1.protocol != ProtocolVersion::$protocol || key.scope() != self.lease.1.scope || key.client_id() != self.lease.1.client_id {
                            return Err(Box::new(StoreFailure::KeyMismatch) as $backend::SessionStoreError);
                        }
                        let protocol = self.protocol_byte(); let limit = self.config.max_checkpoint_size;
                        let input_bytes = session.allocated_size();
                        if input_bytes > limit.saturating_mul(4) { return Err(Box::new(StoreFailure::Oversized) as $backend::SessionStoreError); }
                        let key = self.lease.1.clone();
                        let target = self.config.store.clone();
                        return self.call(|| Box::pin(async move {
                            let encoded_bound = input_bytes.saturating_add(64).min(limit.saturating_sub(8));
                            let checkpoint = store.codec(key.clone(), input_bytes.saturating_add(encoded_bound.saturating_mul(2)).saturating_add(8), move || {
                                let bytes = session.encode_with_limit(encoded_bound).map_err(|_| StoreFailure::Oversized)?;
                                envelope(&bytes, protocol, limit)
                            }).await?;
                            target.save(key, checkpoint).await
                        })).await.map_err(|e| Box::new(e) as $backend::SessionStoreError);
                    }
                    self.save(key, &session).await
                })
            }

            fn clear<'a>(&'a self, key: &'a $backend::SessionStoreKey) -> NativeFuture<'a, ()> {
                Box::pin(async move {
                    if self.lease.1.protocol != ProtocolVersion::$protocol
                        || key.scope() != self.lease.1.scope
                        || key.client_id() != self.lease.1.client_id
                    {
                        return Err(
                            Box::new(StoreFailure::KeyMismatch) as $backend::SessionStoreError
                        );
                    }
                    self.call(|| self.config.store.clear(self.lease.1.clone()))
                        .await
                        .map_err(|e| Box::new(e) as $backend::SessionStoreError)
                })
            }
        }
    };
}

implement_store!(rumqttc_v4, V4);
implement_store!(rumqttc_v5, V5);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionStore, StoreFuture};

    #[cfg(feature = "file-session-store")]
    struct ContendedLoad {
        file: crate::FileStore,
        checkpoint: SessionCheckpoint,
        ready: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    #[cfg(feature = "file-session-store")]
    impl SessionStore for ContendedLoad {
        fn file_store(&self) -> Option<crate::FileStore> {
            Some(self.file.clone())
        }

        fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
            let ready = self.ready.lock().unwrap().take().unwrap();
            let checkpoint = self.checkpoint.clone();
            Box::pin(async move {
                ready.await.unwrap();
                Ok(Some(checkpoint))
            })
        }

        fn save(&self, _: SessionStoreKey, _: SessionCheckpoint) -> StoreFuture<()> {
            panic!("load deadline test must not save")
        }

        fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
            panic!("load deadline test must not clear")
        }
    }

    #[cfg(feature = "file-session-store")]
    fn load_codec_deadline(protocol: ProtocolVersion) {
        use std::time::Duration;

        let deadline = Duration::from_secs(5);
        let root = tempfile::tempdir().unwrap();
        let context = crate::FileStoreContext::new(crate::FileStoreContextOptions {
            worker_threads: 1,
            ..crate::FileStoreContextOptions::default()
        })
        .unwrap();
        let store = context
            .open(
                protocol,
                root.path().to_owned(),
                crate::FileStoreOptions::default(),
            )
            .unwrap()
            .wait(deadline)
            .unwrap()
            .unwrap()
            .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, running) = std::sync::mpsc::channel();
        let blocker = store.codec(
            SessionStoreKey {
                protocol,
                scope: "scope".into(),
                client_id: "blocker".into(),
            },
            0,
            move || {
                started.send(()).unwrap();
                blocked.recv_timeout(deadline).unwrap();
                Ok(())
            },
        );
        assert!(runtime.block_on(async { blocker.now_or_never() }).is_none());
        running.recv_timeout(deadline).unwrap();

        let (loaded, ready) = tokio::sync::oneshot::channel();
        // A valid envelope reaches the queued codec stage. The stalled worker
        // prevents decoding until after the caller's deadline has expired.
        let checkpoint = envelope(
            b"checkpoint",
            if protocol == ProtocolVersion::V4 {
                4
            } else {
                5
            },
            1024,
        )
        .unwrap();
        let mut config = SessionStoreConfig::new(
            Arc::new(ContendedLoad {
                file: (*store).clone(),
                checkpoint,
                ready: Mutex::new(Some(ready)),
            }),
            "scope",
        );
        config.timeout = Duration::from_millis(100);
        let adapter = Adapter::new(config.clone(), protocol, "client").unwrap();
        runtime.block_on(async {
            let load = async {
                match protocol {
                    ProtocolVersion::V4 => rumqttc_v4::SessionStore::load(
                        &adapter,
                        &rumqttc_v4::SessionStoreKey::new("scope", "client"),
                    )
                    .await
                    .map(|_| ()),
                    ProtocolVersion::V5 => rumqttc_v5::SessionStore::load(
                        &adapter,
                        &rumqttc_v5::SessionStoreKey::new("scope", "client"),
                    )
                    .await
                    .map(|_| ()),
                }
            };
            tokio::pin!(load);
            assert!(load.as_mut().now_or_never().is_none());
            tokio::time::advance(Duration::from_millis(60)).await;
            loaded.send(()).unwrap();
            assert!(load.as_mut().now_or_never().is_none());
            tokio::time::advance(Duration::from_millis(39)).await;
            assert!(load.as_mut().now_or_never().is_none());
            tokio::time::advance(Duration::from_millis(1)).await;
            let error = load
                .as_mut()
                .now_or_never()
                .expect("queued decoding must honor the original load deadline")
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<StoreFailure>(),
                Some(&StoreFailure::Timeout)
            );
        });
        drop(adapter);
        assert_eq!(
            Adapter::new(config.clone(), protocol, "client")
                .unwrap_err()
                .store_failure(),
            Some(StoreFailure::InUse)
        );
        assert!(
            matches!(store.clear("scope".into(), "client".into()), Err(e) if e.kind == crate::FileStoreFailure::InUse)
        );
        release.send(()).unwrap();
        // On the sole worker, inspection follows the abandoned codec job and
        // therefore observes its settlement before checking replacement admission.
        store
            .inspect("scope".into(), "client".into())
            .unwrap()
            .wait(deadline)
            .unwrap()
            .unwrap()
            .unwrap();
        drop(Adapter::new(config, protocol, "client").unwrap());
        context.request_shutdown().unwrap();
        assert!(context.join(deadline).unwrap());
    }

    #[cfg(feature = "file-session-store")]
    #[test]
    fn v4_load_codec_uses_the_original_deadline_and_retains_pending_work() {
        load_codec_deadline(ProtocolVersion::V4);
    }

    #[cfg(feature = "file-session-store")]
    #[test]
    fn v5_load_codec_uses_the_original_deadline_and_retains_pending_work() {
        load_codec_deadline(ProtocolVersion::V5);
    }

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

    #[cfg(feature = "file-session-store")]
    #[test]
    fn file_store_redirect_targets_share_administration_authority() {
        let root = tempfile::tempdir().unwrap();
        let context =
            crate::FileStoreContext::new(crate::FileStoreContextOptions::default()).unwrap();
        let store = context
            .open(
                ProtocolVersion::V5,
                root.path().to_owned(),
                crate::FileStoreOptions::default(),
            )
            .unwrap()
            .wait(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .unwrap();
        let config = store
            .configuration("origin".into(), std::time::Duration::from_secs(1), 1024)
            .unwrap();
        let factory = AdapterFactory::new(config);
        let origin = factory.prepare("origin", "client").unwrap();
        let target = factory.prepare("target", "client").unwrap();
        assert!(store.clear("target".into(), "client".into()).is_err());
        drop(origin);
        drop(target);
        drop(factory);
        store
            .clear("target".into(), "client".into())
            .unwrap()
            .wait(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .unwrap();
        context.request_shutdown().unwrap();
        assert!(context.join(std::time::Duration::from_secs(5)).unwrap());
    }

    #[tokio::test]
    async fn mismatched_native_keys_are_rejected_before_host_io() {
        let memory = Arc::new(Memory::default());
        let adapter = Adapter::new(
            SessionStoreConfig::new(memory.clone(), "scope"),
            ProtocolVersion::V5,
            "client",
        )
        .unwrap();
        for key in [
            rumqttc_v5::SessionStoreKey::new("other", "client"),
            rumqttc_v5::SessionStoreKey::new("scope", "other"),
        ] {
            let error = rumqttc_v5::SessionStore::load(&adapter, &key)
                .await
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<StoreFailure>(),
                Some(&StoreFailure::KeyMismatch)
            );
            let error = rumqttc_v5::SessionStore::clear(&adapter, &key)
                .await
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<StoreFailure>(),
                Some(&StoreFailure::KeyMismatch)
            );
        }
        let error = rumqttc_v4::SessionStore::load(
            &adapter,
            &rumqttc_v4::SessionStoreKey::new("scope", "client"),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<StoreFailure>(),
            Some(&StoreFailure::KeyMismatch)
        );
        assert!(memory.0.lock().unwrap().is_none());
    }

    #[test]
    fn scoped_factory_reuses_live_adapters_and_releases_obsolete_leases() {
        let config = SessionStoreConfig::new(Arc::new(Memory::default()), "origin");
        let factory = AdapterFactory::new(config.clone());
        let origin = factory.prepare("origin", "client").unwrap();
        assert!(Arc::ptr_eq(
            &origin,
            &factory.prepare("origin", "client").unwrap()
        ));
        let competing = AdapterFactory::new(config);
        let target = competing.prepare("target", "replacement").unwrap();
        assert_eq!(
            factory
                .prepare("target", "replacement")
                .unwrap_err()
                .store_failure(),
            Some(StoreFailure::InUse)
        );
        assert!(Arc::ptr_eq(
            &origin,
            &factory.prepare("origin", "client").unwrap()
        ));
        drop(target);
        let prepared = factory.prepare("target", "replacement").unwrap();
        drop(prepared);
        assert!(competing.prepare("target", "replacement").is_ok());
        // Weak cache entries are pruned on every preparation and cannot retain a lease.
        assert_eq!(factory.adapters.lock().unwrap().len(), 2);
        factory.prepare("origin", "client").unwrap();
        assert_eq!(factory.adapters.lock().unwrap().len(), 1);
        drop(origin);
        assert!(competing.prepare("origin", "client").is_ok());
    }

    #[tokio::test]
    async fn load_receives_each_clients_checkpoint_limit() {
        struct LimitAwareStore(Mutex<Vec<usize>>);

        impl SessionStore for LimitAwareStore {
            fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
                panic!("adapter must pass the checkpoint limit")
            }

            fn load_with_limit(
                &self,
                _: SessionStoreKey,
                max_checkpoint_size: usize,
            ) -> StoreFuture<Option<SessionCheckpoint>> {
                self.0.lock().unwrap().push(max_checkpoint_size);
                Box::pin(async { Ok(None) })
            }

            fn save(&self, _: SessionStoreKey, _: SessionCheckpoint) -> StoreFuture<()> {
                Box::pin(async { Ok(()) })
            }

            fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
                Box::pin(async { Ok(()) })
            }
        }

        let store = Arc::new(LimitAwareStore(Mutex::new(Vec::new())));
        for limit in [8, 32] {
            let mut config = SessionStoreConfig::new(store.clone(), "scope");
            config.max_checkpoint_size = limit;
            let adapter = Adapter::new(config, ProtocolVersion::V4, "client").unwrap();
            let key = rumqttc_v4::SessionStoreKey::new("scope", "client");
            assert_eq!(
                rumqttc_v4::SessionStore::load(&adapter, &key)
                    .await
                    .unwrap(),
                None
            );
        }
        assert_eq!(*store.0.lock().unwrap(), [8, 32]);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "Keep the scenario setup, actions, and assertions together"
    )]
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
