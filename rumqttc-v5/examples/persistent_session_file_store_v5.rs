//! File-backed `SessionStore` example.
//!
//! The store code below is intentionally application-owned: rumqttc provides
//! `SessionStore` and `PersistedSession`, while this example chooses JSON,
//! file naming, temp-file writes, and corruption/error handling.

use rumqttc::mqttbytes::QoS;
use rumqttc::{
    AsyncClient, Event, MqttOptions, PersistedAckMode, PersistedFilter, PersistedIncomingQos2,
    PersistedPubRel, PersistedPublish, PersistedPublishProperties, PersistedQoS, PersistedRequest,
    PersistedRetainForwardRule, PersistedSession, PersistedSubscribe, PersistedSubscribeProperties,
    PersistedUnsubscribe, PersistedUnsubscribeProperties, PublishOptions, SessionStore,
    SessionStoreError,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs::{self, File};
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{task, time};

const STORE_SCHEMA_VERSION: u16 = 1;

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
        let client_id = client_id.to_owned();

        Box::pin(async move {
            task::spawn_blocking(move || load_session(&path, &client_id))
                .await
                .map_err(box_error)?
        })
    }

    fn save<'a>(&'a self, session: &'a PersistedSession) -> StoreFuture<'a, ()> {
        let directory = self.directory.clone();
        let session = StoredFile::from_session(session);

        Box::pin(async move {
            task::spawn_blocking(move || save_session(&directory, &session))
                .await
                .map_err(box_error)?
        })
    }

    fn clear<'a>(&'a self, client_id: &'a str) -> StoreFuture<'a, ()> {
        let path = session_path(&self.directory, client_id);

        Box::pin(async move {
            task::spawn_blocking(move || clear_session(&path))
                .await
                .map_err(box_error)?
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let store_directory = std::env::var_os("RUMQTTC_SESSION_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rumqttc-v5-session-store-example"));

    println!("Using persistent session store at {store_directory:?}");

    let mut mqttoptions = MqttOptions::new("rumqtt-persistent-session-v5", ("localhost", 1884));
    mqttoptions
        .set_clean_start(false)
        .set_session_expiry_interval(Some(60 * 60))
        .set_keep_alive(5)
        .set_session_store(JsonFileSessionStore::new(store_directory));

    let (client, mut eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();
    task::spawn(requests(client));

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(packet)) => println!("Incoming = {packet:?}"),
            Ok(Event::Outgoing(packet)) => println!("Outgoing = {packet:?}"),
            Ok(Event::Auth(event)) => println!("Auth = {event:?}"),
            Err(error) => {
                println!("Event loop error = {error:?}");
                return Ok(());
            }
        }
    }
}

async fn requests(client: AsyncClient) {
    client
        .subscribe("hello/persistent-session", QoS::AtLeastOnce)
        .await
        .expect("subscribe should be queued");

    for i in 0..10_usize {
        client
            .publish(
                "hello/persistent-session",
                format!("persistent message {i}"),
                PublishOptions::new(QoS::AtLeastOnce),
            )
            .await
            .expect("publish should be queued");

        time::sleep(Duration::from_secs(1)).await;
    }
}

fn load_session(
    path: &Path,
    client_id: &str,
) -> Result<Option<PersistedSession>, SessionStoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(box_error(error)),
    };

    let stored: StoredFile = serde_json::from_slice(&bytes)?;
    if stored.schema_version != STORE_SCHEMA_VERSION {
        return Err(invalid_data(format!(
            "unsupported file store schema version {}",
            stored.schema_version
        )));
    }

    let session = stored.session.into_persisted();
    if session.client_id != client_id {
        return Err(invalid_data(format!(
            "stored session belongs to client id {:?}, not {:?}",
            session.client_id, client_id
        )));
    }

    Ok(Some(session))
}

fn save_session(directory: &Path, stored: &StoredFile) -> Result<(), SessionStoreError> {
    fs::create_dir_all(directory)?;

    let path = session_path(directory, &stored.session.client_id);
    let temp_path = temporary_path(&path);
    let bytes = serde_json::to_vec_pretty(stored)?;

    let mut file = File::create(&temp_path)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);

    replace_file(&temp_path, &path).inspect_err(|_| {
        drop(fs::remove_file(&temp_path));
    })?;
    sync_directory(directory)?;
    Ok(())
}

fn clear_session(path: &Path) -> Result<(), SessionStoreError> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent_directory(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(box_error(error)),
    }
}

fn session_path(directory: &Path, client_id: &str) -> PathBuf {
    directory.join(format!("{}.json", hex_encode(client_id.as_bytes())))
}

fn temporary_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session.json");

    path.with_file_name(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce))
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, path: &Path) -> Result<(), SessionStoreError> {
    if let Err(error) = fs::rename(temp_path, path) {
        if path.exists() {
            fs::remove_file(path)?;
            fs::rename(temp_path, path)?;
            return Ok(());
        }

        return Err(box_error(error));
    }

    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, path: &Path) -> Result<(), SessionStoreError> {
    fs::rename(temp_path, path)?;
    Ok(())
}

fn sync_parent_directory(path: &Path) -> Result<(), SessionStoreError> {
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), SessionStoreError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), SessionStoreError> {
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn invalid_data(message: String) -> SessionStoreError {
    box_error(io::Error::new(io::ErrorKind::InvalidData, message))
}

fn box_error(error: impl Error + Send + Sync + 'static) -> SessionStoreError {
    Box::new(error)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredFile {
    schema_version: u16,
    session: StoredSession,
}

impl StoredFile {
    fn from_session(session: &PersistedSession) -> Self {
        Self {
            schema_version: STORE_SCHEMA_VERSION,
            session: StoredSession::from(session),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredSession {
    format_version: u16,
    client_id: String,
    clean_start: bool,
    session_expiry_interval: Option<u32>,
    outgoing_inflight_upper_limit: Option<u16>,
    ack_mode: StoredAckMode,
    last_pkid: u16,
    last_puback: u16,
    replay: Vec<StoredRequest>,
    incoming_qos2: Vec<StoredIncomingQos2>,
}

impl From<&PersistedSession> for StoredSession {
    fn from(session: &PersistedSession) -> Self {
        Self {
            format_version: session.format_version,
            client_id: session.client_id.clone(),
            clean_start: session.clean_start,
            session_expiry_interval: session.session_expiry_interval,
            outgoing_inflight_upper_limit: session.outgoing_inflight_upper_limit,
            ack_mode: StoredAckMode::from(session.ack_mode),
            last_pkid: session.last_pkid,
            last_puback: session.last_puback,
            replay: session.replay.iter().map(StoredRequest::from).collect(),
            incoming_qos2: session
                .incoming_qos2
                .iter()
                .map(StoredIncomingQos2::from)
                .collect(),
        }
    }
}

impl StoredSession {
    fn into_persisted(self) -> PersistedSession {
        PersistedSession {
            format_version: self.format_version,
            client_id: self.client_id,
            clean_start: self.clean_start,
            session_expiry_interval: self.session_expiry_interval,
            outgoing_inflight_upper_limit: self.outgoing_inflight_upper_limit,
            ack_mode: self.ack_mode.into(),
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

impl From<PersistedAckMode> for StoredAckMode {
    fn from(ack_mode: PersistedAckMode) -> Self {
        match ack_mode {
            PersistedAckMode::Automatic => Self::Automatic,
            PersistedAckMode::Manual => Self::Manual,
        }
    }
}

impl From<StoredAckMode> for PersistedAckMode {
    fn from(ack_mode: StoredAckMode) -> Self {
        match ack_mode {
            StoredAckMode::Automatic => Self::Automatic,
            StoredAckMode::Manual => Self::Manual,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "packet")]
enum StoredRequest {
    Publish(StoredPublish),
    PubRel(StoredPubRel),
    Subscribe(StoredSubscribe),
    Unsubscribe(StoredUnsubscribe),
}

impl From<&PersistedRequest> for StoredRequest {
    fn from(request: &PersistedRequest) -> Self {
        match request {
            PersistedRequest::Publish(publish) => Self::Publish(StoredPublish::from(publish)),
            PersistedRequest::PubRel(pubrel) => Self::PubRel(StoredPubRel::from(pubrel)),
            PersistedRequest::Subscribe(subscribe) => {
                Self::Subscribe(StoredSubscribe::from(subscribe))
            }
            PersistedRequest::Unsubscribe(unsubscribe) => {
                Self::Unsubscribe(StoredUnsubscribe::from(unsubscribe))
            }
        }
    }
}

impl StoredRequest {
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
    properties: Option<StoredPublishProperties>,
}

impl From<&PersistedPublish> for StoredPublish {
    fn from(publish: &PersistedPublish) -> Self {
        Self {
            dup: publish.dup,
            qos: StoredQoS::from(publish.qos),
            retain: publish.retain,
            topic: publish.topic.clone(),
            pkid: publish.pkid,
            payload: publish.payload.clone(),
            properties: publish
                .properties
                .as_ref()
                .map(StoredPublishProperties::from),
        }
    }
}

impl StoredPublish {
    fn into_persisted(self) -> PersistedPublish {
        PersistedPublish {
            dup: self.dup,
            qos: self.qos.into(),
            retain: self.retain,
            topic: self.topic,
            pkid: self.pkid,
            payload: self.payload,
            properties: self.properties.map(StoredPublishProperties::into_persisted),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredPubRel {
    pkid: u16,
}

impl From<&PersistedPubRel> for StoredPubRel {
    fn from(pubrel: &PersistedPubRel) -> Self {
        Self { pkid: pubrel.pkid }
    }
}

impl StoredPubRel {
    fn into_persisted(self) -> PersistedPubRel {
        PersistedPubRel { pkid: self.pkid }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredSubscribe {
    pkid: u16,
    filters: Vec<StoredFilter>,
    properties: Option<StoredSubscribeProperties>,
}

impl From<&PersistedSubscribe> for StoredSubscribe {
    fn from(subscribe: &PersistedSubscribe) -> Self {
        Self {
            pkid: subscribe.pkid,
            filters: subscribe.filters.iter().map(StoredFilter::from).collect(),
            properties: subscribe
                .properties
                .as_ref()
                .map(StoredSubscribeProperties::from),
        }
    }
}

impl StoredSubscribe {
    fn into_persisted(self) -> PersistedSubscribe {
        PersistedSubscribe {
            pkid: self.pkid,
            filters: self
                .filters
                .into_iter()
                .map(StoredFilter::into_persisted)
                .collect(),
            properties: self
                .properties
                .map(StoredSubscribeProperties::into_persisted),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredUnsubscribe {
    pkid: u16,
    filters: Vec<String>,
    properties: Option<StoredUnsubscribeProperties>,
}

impl From<&PersistedUnsubscribe> for StoredUnsubscribe {
    fn from(unsubscribe: &PersistedUnsubscribe) -> Self {
        Self {
            pkid: unsubscribe.pkid,
            filters: unsubscribe.filters.clone(),
            properties: unsubscribe
                .properties
                .as_ref()
                .map(StoredUnsubscribeProperties::from),
        }
    }
}

impl StoredUnsubscribe {
    fn into_persisted(self) -> PersistedUnsubscribe {
        PersistedUnsubscribe {
            pkid: self.pkid,
            filters: self.filters,
            properties: self
                .properties
                .map(StoredUnsubscribeProperties::into_persisted),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredIncomingQos2 {
    pkid: u16,
}

impl From<&PersistedIncomingQos2> for StoredIncomingQos2 {
    fn from(incoming: &PersistedIncomingQos2) -> Self {
        Self {
            pkid: incoming.pkid,
        }
    }
}

impl StoredIncomingQos2 {
    fn into_persisted(self) -> PersistedIncomingQos2 {
        PersistedIncomingQos2 { pkid: self.pkid }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum StoredQoS {
    Qos0,
    Qos1,
    Qos2,
}

impl From<PersistedQoS> for StoredQoS {
    fn from(qos: PersistedQoS) -> Self {
        match qos {
            PersistedQoS::AtMostOnce => Self::Qos0,
            PersistedQoS::AtLeastOnce => Self::Qos1,
            PersistedQoS::ExactlyOnce => Self::Qos2,
        }
    }
}

impl From<StoredQoS> for PersistedQoS {
    fn from(qos: StoredQoS) -> Self {
        match qos {
            StoredQoS::Qos0 => Self::AtMostOnce,
            StoredQoS::Qos1 => Self::AtLeastOnce,
            StoredQoS::Qos2 => Self::ExactlyOnce,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredPublishProperties {
    payload_format_indicator: Option<u8>,
    message_expiry_interval: Option<u32>,
    response_topic: Option<String>,
    correlation_data: Option<Vec<u8>>,
    user_properties: Vec<(String, String)>,
    subscription_identifiers: Vec<usize>,
    content_type: Option<String>,
}

impl From<&PersistedPublishProperties> for StoredPublishProperties {
    fn from(properties: &PersistedPublishProperties) -> Self {
        Self {
            payload_format_indicator: properties.payload_format_indicator,
            message_expiry_interval: properties.message_expiry_interval,
            response_topic: properties.response_topic.clone(),
            correlation_data: properties.correlation_data.clone(),
            user_properties: properties.user_properties.clone(),
            subscription_identifiers: properties.subscription_identifiers.clone(),
            content_type: properties.content_type.clone(),
        }
    }
}

impl StoredPublishProperties {
    fn into_persisted(self) -> PersistedPublishProperties {
        PersistedPublishProperties {
            payload_format_indicator: self.payload_format_indicator,
            message_expiry_interval: self.message_expiry_interval,
            response_topic: self.response_topic,
            correlation_data: self.correlation_data,
            user_properties: self.user_properties,
            subscription_identifiers: self.subscription_identifiers,
            content_type: self.content_type,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredFilter {
    path: String,
    qos: StoredQoS,
    nolocal: bool,
    preserve_retain: bool,
    retain_forward_rule: StoredRetainForwardRule,
}

impl From<&PersistedFilter> for StoredFilter {
    fn from(filter: &PersistedFilter) -> Self {
        Self {
            path: filter.path.clone(),
            qos: StoredQoS::from(filter.qos),
            nolocal: filter.nolocal,
            preserve_retain: filter.preserve_retain,
            retain_forward_rule: StoredRetainForwardRule::from(filter.retain_forward_rule),
        }
    }
}

impl StoredFilter {
    fn into_persisted(self) -> PersistedFilter {
        PersistedFilter {
            path: self.path,
            qos: self.qos.into(),
            nolocal: self.nolocal,
            preserve_retain: self.preserve_retain,
            retain_forward_rule: self.retain_forward_rule.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum StoredRetainForwardRule {
    OnEverySubscribe,
    OnNewSubscribe,
    Never,
}

impl From<PersistedRetainForwardRule> for StoredRetainForwardRule {
    fn from(rule: PersistedRetainForwardRule) -> Self {
        match rule {
            PersistedRetainForwardRule::OnEverySubscribe => Self::OnEverySubscribe,
            PersistedRetainForwardRule::OnNewSubscribe => Self::OnNewSubscribe,
            PersistedRetainForwardRule::Never => Self::Never,
        }
    }
}

impl From<StoredRetainForwardRule> for PersistedRetainForwardRule {
    fn from(rule: StoredRetainForwardRule) -> Self {
        match rule {
            StoredRetainForwardRule::OnEverySubscribe => Self::OnEverySubscribe,
            StoredRetainForwardRule::OnNewSubscribe => Self::OnNewSubscribe,
            StoredRetainForwardRule::Never => Self::Never,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredSubscribeProperties {
    id: Option<usize>,
    user_properties: Vec<(String, String)>,
}

impl From<&PersistedSubscribeProperties> for StoredSubscribeProperties {
    fn from(properties: &PersistedSubscribeProperties) -> Self {
        Self {
            id: properties.id,
            user_properties: properties.user_properties.clone(),
        }
    }
}

impl StoredSubscribeProperties {
    fn into_persisted(self) -> PersistedSubscribeProperties {
        PersistedSubscribeProperties {
            id: self.id,
            user_properties: self.user_properties,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredUnsubscribeProperties {
    user_properties: Vec<(String, String)>,
}

impl From<&PersistedUnsubscribeProperties> for StoredUnsubscribeProperties {
    fn from(properties: &PersistedUnsubscribeProperties) -> Self {
        Self {
            user_properties: properties.user_properties.clone(),
        }
    }
}

impl StoredUnsubscribeProperties {
    fn into_persisted(self) -> PersistedUnsubscribeProperties {
        PersistedUnsubscribeProperties {
            user_properties: self.user_properties,
        }
    }
}
