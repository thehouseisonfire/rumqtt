//! File-backed `SessionStore` example.
//!
//! This is intentionally application code, not a built-in rumqttc store.
//! rumqttc provides `SessionStore` and `PersistedSession`, while this example
//! chooses JSON, file names, and durability policy.

use rumqttc::{
    AsyncClient, Event, MqttOptions, PersistedAckMode, PersistedFilter, PersistedIncomingQos2,
    PersistedPubRel, PersistedPublish, PersistedQoS, PersistedRequest, PersistedSession,
    PersistedSubscribe, PersistedUnsubscribe, PublishOptions, QoS, SessionStore, SessionStoreError,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;
use tokio::task;

type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SessionStoreError>> + Send + 'a>>;

#[derive(Clone, Debug)]
struct JsonFileSessionStore {
    directory: PathBuf,
}

impl JsonFileSessionStore {
    fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
}

impl SessionStore for JsonFileSessionStore {
    fn load<'a>(&'a self, client_id: &'a str) -> StoreFuture<'a, Option<PersistedSession>> {
        let path = session_path(&self.directory, client_id);
        Box::pin(async move { task::spawn_blocking(move || load_session(&path)).await? })
    }

    fn save<'a>(&'a self, session: &'a PersistedSession) -> StoreFuture<'a, ()> {
        let directory = self.directory.clone();
        let stored = StoredFile {
            session: StoredSession::from_persisted(session),
        };
        Box::pin(
            async move { task::spawn_blocking(move || save_session(&directory, &stored)).await? },
        )
    }

    fn clear<'a>(&'a self, client_id: &'a str) -> StoreFuture<'a, ()> {
        let path = session_path(&self.directory, client_id);
        Box::pin(async move { task::spawn_blocking(move || clear_session(&path)).await? })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let store_directory = std::env::temp_dir().join("rumqtt-v4-persistent-session");
    println!("Using persistent session store at {store_directory:?}");

    let mut mqttoptions = MqttOptions::new("rumqtt-persistent-session-v4", ("localhost", 1884));
    mqttoptions
        .set_clean_session(false)
        .set_keep_alive(5)
        .set_session_store(JsonFileSessionStore::new(store_directory));

    let (client, mut eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();

    client
        .subscribe("hello/persistent-session", QoS::AtLeastOnce)
        .await?;

    let publisher = client.clone();
    tokio::spawn(async move {
        let mut i = 0u64;
        loop {
            if let Err(error) = publisher
                .publish(
                    "hello/persistent-session",
                    format!("persistent message {i}"),
                    PublishOptions::new(QoS::AtLeastOnce),
                )
                .await
            {
                eprintln!("publish failed: {error}");
            }
            i += 1;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(incoming)) => println!("incoming: {incoming:?}"),
            Ok(Event::Outgoing(outgoing)) => println!("outgoing: {outgoing:?}"),
            Err(error) => {
                eprintln!("eventloop error: {error}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

fn load_session(path: &Path) -> Result<Option<PersistedSession>, SessionStoreError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let stored: StoredFile = serde_json::from_slice(&bytes).map_err(box_error)?;
            Ok(Some(stored.session.into_persisted()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(box_error(error)),
    }
}

fn save_session(directory: &Path, stored: &StoredFile) -> Result<(), SessionStoreError> {
    std::fs::create_dir_all(directory).map_err(box_error)?;
    sync_directory(directory)?;

    let path = session_path(directory, &stored.session.client_id);
    let temp_path = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(stored).map_err(box_error)?;

    {
        let mut file = std::fs::File::create(&temp_path).map_err(box_error)?;
        file.write_all(&bytes).map_err(box_error)?;
        file.sync_all().map_err(box_error)?;
    }

    replace_file(&temp_path, &path)?;
    sync_parent_directory(&path)
}

fn clear_session(path: &Path) -> Result<(), SessionStoreError> {
    match std::fs::remove_file(path) {
        Ok(()) => sync_parent_directory(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(box_error(error)),
    }
}

fn session_path(directory: &Path, client_id: &str) -> PathBuf {
    directory.join(format!("{}.json", hex_key(client_id)))
}

fn hex_key(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(unix)]
fn replace_file(temp_path: &Path, path: &Path) -> Result<(), SessionStoreError> {
    std::fs::rename(temp_path, path).map_err(box_error)
}

#[cfg(not(unix))]
fn replace_file(temp_path: &Path, path: &Path) -> Result<(), SessionStoreError> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(box_error(error)),
    }
    std::fs::rename(temp_path, path).map_err(box_error)
}

fn sync_parent_directory(path: &Path) -> Result<(), SessionStoreError> {
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), SessionStoreError> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(box_error)
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), SessionStoreError> {
    Ok(())
}

fn box_error(error: impl Error + Send + Sync + 'static) -> SessionStoreError {
    Box::new(error)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredFile {
    session: StoredSession,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredSession {
    format_version: u16,
    client_id: String,
    clean_session: bool,
    max_inflight: u16,
    ack_mode: StoredAckMode,
    last_pkid: u16,
    last_puback: u16,
    replay: Vec<StoredRequest>,
    incoming_qos2: Vec<StoredIncomingQos2>,
}

impl StoredSession {
    fn from_persisted(session: &PersistedSession) -> Self {
        Self {
            format_version: session.format_version,
            client_id: session.client_id.clone(),
            clean_session: session.clean_session,
            max_inflight: session.max_inflight,
            ack_mode: StoredAckMode::from_persisted(session.ack_mode),
            last_pkid: session.last_pkid,
            last_puback: session.last_puback,
            replay: session
                .replay
                .iter()
                .map(StoredRequest::from_persisted)
                .collect(),
            incoming_qos2: session
                .incoming_qos2
                .iter()
                .map(StoredIncomingQos2::from_persisted)
                .collect(),
        }
    }

    fn into_persisted(self) -> PersistedSession {
        PersistedSession {
            format_version: self.format_version,
            client_id: self.client_id,
            clean_session: self.clean_session,
            max_inflight: self.max_inflight,
            ack_mode: self.ack_mode.into_persisted(),
            last_pkid: self.last_pkid,
            last_puback: self.last_puback,
            replay: self
                .replay
                .into_iter()
                .map(StoredRequest::into_persisted)
                .collect(),
            incoming_qos2: self
                .incoming_qos2
                .into_iter()
                .map(StoredIncomingQos2::into_persisted)
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum StoredAckMode {
    Automatic,
    Manual,
}

impl StoredAckMode {
    const fn from_persisted(ack_mode: PersistedAckMode) -> Self {
        match ack_mode {
            PersistedAckMode::Automatic => Self::Automatic,
            PersistedAckMode::Manual => Self::Manual,
        }
    }

    const fn into_persisted(self) -> PersistedAckMode {
        match self {
            Self::Automatic => PersistedAckMode::Automatic,
            Self::Manual => PersistedAckMode::Manual,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
enum StoredRequest {
    Publish(StoredPublish),
    PubRel(StoredPubRel),
    Subscribe(StoredSubscribe),
    Unsubscribe(StoredUnsubscribe),
}

impl StoredRequest {
    fn from_persisted(request: &PersistedRequest) -> Self {
        match request {
            PersistedRequest::Publish(publish) => {
                Self::Publish(StoredPublish::from_persisted(publish))
            }
            PersistedRequest::PubRel(pubrel) => Self::PubRel(StoredPubRel::from_persisted(pubrel)),
            PersistedRequest::Subscribe(subscribe) => {
                Self::Subscribe(StoredSubscribe::from_persisted(subscribe))
            }
            PersistedRequest::Unsubscribe(unsubscribe) => {
                Self::Unsubscribe(StoredUnsubscribe::from_persisted(unsubscribe))
            }
        }
    }

    fn into_persisted(self) -> PersistedRequest {
        match self {
            Self::Publish(publish) => PersistedRequest::Publish(publish.into_persisted()),
            Self::PubRel(pubrel) => PersistedRequest::PubRel(pubrel.into_persisted()),
            Self::Subscribe(subscribe) => PersistedRequest::Subscribe(subscribe.into_persisted()),
            Self::Unsubscribe(unsubscribe) => {
                PersistedRequest::Unsubscribe(unsubscribe.into_persisted())
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredPublish {
    dup: bool,
    qos: StoredQoS,
    retain: bool,
    topic: Vec<u8>,
    pkid: u16,
    payload: Vec<u8>,
}

impl StoredPublish {
    fn from_persisted(publish: &PersistedPublish) -> Self {
        Self {
            dup: publish.dup,
            qos: StoredQoS::from_persisted(publish.qos),
            retain: publish.retain,
            topic: publish.topic.clone(),
            pkid: publish.pkid,
            payload: publish.payload.clone(),
        }
    }

    fn into_persisted(self) -> PersistedPublish {
        PersistedPublish {
            dup: self.dup,
            qos: self.qos.into_persisted(),
            retain: self.retain,
            topic: self.topic,
            pkid: self.pkid,
            payload: self.payload,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredPubRel {
    pkid: u16,
}

impl StoredPubRel {
    const fn from_persisted(pubrel: &PersistedPubRel) -> Self {
        Self { pkid: pubrel.pkid }
    }

    const fn into_persisted(self) -> PersistedPubRel {
        PersistedPubRel { pkid: self.pkid }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredSubscribe {
    pkid: u16,
    filters: Vec<StoredFilter>,
}

impl StoredSubscribe {
    fn from_persisted(subscribe: &PersistedSubscribe) -> Self {
        Self {
            pkid: subscribe.pkid,
            filters: subscribe
                .filters
                .iter()
                .map(StoredFilter::from_persisted)
                .collect(),
        }
    }

    fn into_persisted(self) -> PersistedSubscribe {
        PersistedSubscribe {
            pkid: self.pkid,
            filters: self
                .filters
                .into_iter()
                .map(StoredFilter::into_persisted)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredUnsubscribe {
    pkid: u16,
    topics: Vec<String>,
}

impl StoredUnsubscribe {
    fn from_persisted(unsubscribe: &PersistedUnsubscribe) -> Self {
        Self {
            pkid: unsubscribe.pkid,
            topics: unsubscribe.topics.clone(),
        }
    }

    fn into_persisted(self) -> PersistedUnsubscribe {
        PersistedUnsubscribe {
            pkid: self.pkid,
            topics: self.topics,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredIncomingQos2 {
    pkid: u16,
}

impl StoredIncomingQos2 {
    const fn from_persisted(incoming: &PersistedIncomingQos2) -> Self {
        Self {
            pkid: incoming.pkid,
        }
    }

    const fn into_persisted(self) -> PersistedIncomingQos2 {
        PersistedIncomingQos2 { pkid: self.pkid }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredFilter {
    path: String,
    qos: StoredQoS,
}

impl StoredFilter {
    fn from_persisted(filter: &PersistedFilter) -> Self {
        Self {
            path: filter.path.clone(),
            qos: StoredQoS::from_persisted(filter.qos),
        }
    }

    fn into_persisted(self) -> PersistedFilter {
        PersistedFilter {
            path: self.path,
            qos: self.qos.into_persisted(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum StoredQoS {
    Qos0,
    Qos1,
    Qos2,
}

impl StoredQoS {
    const fn from_persisted(qos: PersistedQoS) -> Self {
        match qos {
            PersistedQoS::AtMostOnce => Self::Qos0,
            PersistedQoS::AtLeastOnce => Self::Qos1,
            PersistedQoS::ExactlyOnce => Self::Qos2,
        }
    }

    const fn into_persisted(self) -> PersistedQoS {
        match self {
            Self::Qos0 => PersistedQoS::AtMostOnce,
            Self::Qos1 => PersistedQoS::AtLeastOnce,
            Self::Qos2 => PersistedQoS::ExactlyOnce,
        }
    }
}
