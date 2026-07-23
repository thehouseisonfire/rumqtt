#![cfg_attr(
    not(any(feature = "v4", feature = "v5")),
    allow(dead_code, unused_imports)
)]

use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rumqttc_session_store_file_core_next::{
    CheckpointInspection, CleanupReport, FileStore, FileStoreError, FileStoreOptions, LegacyLoad,
    QuarantineInfo, checkpoint_filename,
};

const KEY_FORMAT_VERSION: u8 = 1;

mod private {
    pub trait Sealed {}
}

pub trait Protocol: private::Sealed + Send + Sync + 'static {
    type Session: Send + Sync + 'static;
    type EncodeError: Error + Send + Sync + 'static;
    type DecodeError: Error + Send + Sync + 'static;

    const TAG: u8;
    const NAMESPACE: &'static str;

    fn encode(session: &Self::Session) -> Result<Vec<u8>, Self::EncodeError>;
    fn decode(payload: &[u8]) -> Result<Self::Session, Self::DecodeError>;
}

pub use private::Sealed;

#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum KeyEncodeError {
    #[error("session-store key field '{field}' is too large: {length} bytes")]
    FieldTooLarge { field: &'static str, length: usize },
}

pub enum AdapterError<P: Protocol> {
    FileStore(FileStoreError),
    SessionEncode(P::EncodeError),
    SessionDecode(P::DecodeError),
    KeyEncode(KeyEncodeError),
    LegacyCheckpointDetected { diagnostic_path: PathBuf },
}

impl<P: Protocol> fmt::Debug for AdapterError<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileStore(error) => formatter.debug_tuple("FileStore").field(error).finish(),
            Self::SessionEncode(error) => {
                formatter.debug_tuple("SessionEncode").field(error).finish()
            }
            Self::SessionDecode(error) => {
                formatter.debug_tuple("SessionDecode").field(error).finish()
            }
            Self::KeyEncode(error) => formatter.debug_tuple("KeyEncode").field(error).finish(),
            Self::LegacyCheckpointDetected { diagnostic_path } => formatter
                .debug_struct("LegacyCheckpointDetected")
                .field("diagnostic_path", diagnostic_path)
                .finish(),
        }
    }
}

impl<P: Protocol> From<FileStoreError> for AdapterError<P> {
    fn from(error: FileStoreError) -> Self {
        Self::FileStore(error)
    }
}

impl<P: Protocol> From<KeyEncodeError> for AdapterError<P> {
    fn from(error: KeyEncodeError) -> Self {
        Self::KeyEncode(error)
    }
}

pub struct Adapter<P: Protocol> {
    core: FileStore,
    protocol: PhantomData<fn() -> P>,
}

impl<P: Protocol> Clone for Adapter<P> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            protocol: PhantomData,
        }
    }
}

impl<P: Protocol> fmt::Debug for Adapter<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Adapter")
            .field("core", &self.core)
            .field("namespace", &P::NAMESPACE)
            .finish()
    }
}

impl<P: Protocol> Adapter<P> {
    pub(crate) async fn open(
        root: impl Into<PathBuf>,
        options: FileStoreOptions,
    ) -> Result<Self, AdapterError<P>> {
        Ok(Self {
            core: FileStore::open(root, P::NAMESPACE, options).await?,
            protocol: PhantomData,
        })
    }

    pub(crate) fn checkpoint_path(
        &self,
        scope: &str,
        client_id: &str,
    ) -> Result<PathBuf, KeyEncodeError> {
        Ok(self
            .core
            .checkpoint_path(&encode_key::<P>(scope, client_id)?))
    }

    pub(crate) async fn inspect(
        &self,
        scope: &str,
        client_id: &str,
    ) -> Result<CheckpointInspection, AdapterError<P>> {
        Ok(self
            .core
            .inspect_with_legacy_filename(
                &encode_key::<P>(scope, client_id)?,
                legacy_filename(scope, client_id),
            )
            .await?)
    }

    pub(crate) async fn quarantine(
        &self,
        scope: &str,
        client_id: &str,
    ) -> Result<QuarantineInfo, AdapterError<P>> {
        Ok(self
            .core
            .quarantine(&encode_key::<P>(scope, client_id)?)
            .await?)
    }

    pub(crate) async fn clear(&self, scope: &str, client_id: &str) -> Result<(), AdapterError<P>> {
        Ok(self.core.clear(&encode_key::<P>(scope, client_id)?).await?)
    }

    pub(crate) async fn cleanup(
        &self,
        minimum_age: Duration,
    ) -> Result<CleanupReport, AdapterError<P>> {
        Ok(self
            .core
            .cleanup_stale_temporary_files_before_use(minimum_age)
            .await?)
    }

    pub(crate) async fn load(
        &self,
        scope: &str,
        client_id: &str,
    ) -> Result<Option<P::Session>, AdapterError<P>> {
        let encoded_key = encode_key::<P>(scope, client_id)?;
        let payload = match self
            .core
            .load_with_legacy_filename(&encoded_key, legacy_filename(scope, client_id))
            .await?
        {
            LegacyLoad::Absent => return Ok(None),
            LegacyLoad::Present(payload) => payload,
            LegacyLoad::LegacyDetected { diagnostic_path } => {
                return Err(AdapterError::LegacyCheckpointDetected { diagnostic_path });
            }
        };
        P::decode(&payload)
            .map(Some)
            .map_err(AdapterError::SessionDecode)
    }

    pub(crate) async fn save(
        &self,
        scope: &str,
        client_id: &str,
        session: &P::Session,
    ) -> Result<(), AdapterError<P>> {
        let encoded_key = encode_key::<P>(scope, client_id)?;
        let payload = P::encode(session).map_err(AdapterError::SessionEncode)?;
        self.core.save(&encoded_key, payload).await?;
        Ok(())
    }
}

pub fn encode_key<P: Protocol>(scope: &str, client_id: &str) -> Result<Vec<u8>, KeyEncodeError> {
    let scope = scope.as_bytes();
    let client_id = client_id.as_bytes();
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
    bytes.push(P::TAG);
    bytes.extend_from_slice(&scope_len.to_be_bytes());
    bytes.extend_from_slice(scope);
    bytes.extend_from_slice(&client_id_len.to_be_bytes());
    bytes.extend_from_slice(client_id);
    Ok(bytes)
}

pub fn filename<P: Protocol>(scope: &str, client_id: &str) -> Result<String, KeyEncodeError> {
    Ok(checkpoint_filename(&encode_key::<P>(scope, client_id)?))
}

pub fn legacy_filename(scope: &str, client_id: &str) -> String {
    format!(
        "{}.{}.session",
        hex(scope.as_bytes()),
        hex(client_id.as_bytes())
    )
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

pub fn namespace<P: Protocol>() -> &'static Path {
    Path::new(P::NAMESPACE)
}
