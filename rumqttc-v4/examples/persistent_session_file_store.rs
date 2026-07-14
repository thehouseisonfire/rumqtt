//! File-backed `SessionStore` example.
//!
//! This is intentionally application code, not a built-in rumqttc store.
//! rumqttc provides `SessionStore`, `SessionStoreKey`, `PersistedSession`, and
//! canonical session encoding while this example chooses file names and
//! durability policy.

use rumqttc::{
    AsyncClient, Event, MqttOptions, PersistedSession, PublishOptions, QoS, SessionStore,
    SessionStoreError, SessionStoreKey,
};
use std::error::Error;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::task;

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
            task::spawn_blocking(move || save_session(&directory, &path, &bytes)).await?
        })
    }

    fn clear<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, ()> {
        let path = session_path(&self.directory, key);
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
        .set_session_store_scope("localhost-1884")
        .set_keep_alive(5)
        .set_session_store(FileSessionStore::new(store_directory));

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

fn load_session(
    path: &Path,
    client_id: &str,
) -> Result<Option<PersistedSession>, SessionStoreError> {
    let bytes = match std::fs::read(path) {
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
    std::fs::create_dir_all(directory).map_err(box_error)?;
    sync_directory(directory)?;

    let temp_path = temporary_path(path);
    {
        let mut file = std::fs::File::create(&temp_path).map_err(box_error)?;
        file.write_all(bytes).map_err(box_error)?;
        file.sync_all().map_err(box_error)?;
    }

    replace_file(&temp_path, path).inspect_err(|_| {
        drop(std::fs::remove_file(&temp_path));
    })?;
    sync_parent_directory(path)
}

fn clear_session(path: &Path) -> Result<(), SessionStoreError> {
    match std::fs::remove_file(path) {
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
