//! File-backed `SessionStore` example.
//!
//! The store code below is intentionally application-owned: rumqttc provides
//! `SessionStore`, `SessionStoreKey`, `PersistedSession`, and canonical session
//! encoding while this example chooses file naming, temp-file writes, and
//! corruption/error handling.

use rumqttc::mqttbytes::QoS;
use rumqttc::{
    AsyncClient, Event, MqttOptions, PersistedSession, PublishOptions, SessionStore,
    SessionStoreError, SessionStoreKey,
};
use std::error::Error;
use std::fs::{self, File};
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{task, time};

type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SessionStoreError>> + Send + 'a>>;

#[derive(Clone, Debug)]
struct FileSessionStore {
    directory: PathBuf,
}

impl FileSessionStore {
    fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
}

impl SessionStore for FileSessionStore {
    fn load<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, Option<PersistedSession>> {
        let path = session_path(&self.directory, key);
        let client_id = key.client_id().to_owned();

        Box::pin(async move {
            task::spawn_blocking(move || load_session(&path, &client_id))
                .await
                .map_err(box_error)?
        })
    }

    fn save<'a>(
        &'a self,
        key: &'a SessionStoreKey,
        session: &'a PersistedSession,
    ) -> StoreFuture<'a, ()> {
        let directory = self.directory.clone();
        let path = session_path(&directory, key);
        let bytes = match session.encode() {
            Ok(bytes) => bytes,
            Err(error) => return Box::pin(async move { Err(box_error(error)) }),
        };

        Box::pin(async move {
            task::spawn_blocking(move || save_session(&directory, &path, &bytes))
                .await
                .map_err(box_error)?
        })
    }

    fn clear<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, ()> {
        let path = session_path(&self.directory, key);

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

    let store_directory = std::env::var_os("RUMQTTC_SESSION_STORE").map_or_else(
        || std::env::temp_dir().join("rumqttc-v5-session-store-example"),
        PathBuf::from,
    );

    println!("Using persistent session store at {store_directory:?}");

    let mut mqttoptions = MqttOptions::new("rumqtt-persistent-session-v5", ("localhost", 1884));
    mqttoptions
        .set_clean_start(false)
        .set_session_expiry_interval(Some(60 * 60))
        .set_session_store_scope("localhost-1884")
        .set_keep_alive(5)
        .set_session_store(FileSessionStore::new(store_directory));

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

    let session = PersistedSession::decode(&bytes).map_err(box_error)?;
    if session.client_id != client_id {
        return Err(invalid_data(format!(
            "stored session belongs to client id {:?}, not {:?}",
            session.client_id, client_id
        )));
    }

    Ok(Some(session))
}

fn save_session(directory: &Path, path: &Path, bytes: &[u8]) -> Result<(), SessionStoreError> {
    fs::create_dir_all(directory)?;

    let temp_path = temporary_path(path);
    let mut file = File::create(&temp_path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);

    replace_file(&temp_path, path).inspect_err(|_| {
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

fn session_path(directory: &Path, key: &SessionStoreKey) -> PathBuf {
    let scope = hex_encode(key.scope().as_bytes());
    let client_id = hex_encode(key.client_id().as_bytes());
    directory.join(format!("{scope}.{client_id}.session"))
}

fn temporary_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session");

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
