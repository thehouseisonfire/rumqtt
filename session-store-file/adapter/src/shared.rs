#![cfg_attr(
    not(any(feature = "v4", feature = "v5")),
    allow(dead_code, unused_imports)
)]

use std::error::Error;
use std::fmt;
use std::io::Cursor;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{CheckpointInspection, CheckpointState, FileStoreOptions};
use atomic_blob_store::{
    AtomicBlobStoreError, AtomicBlobStoreOptions, BlobFormatIdentity, BlobState, CleanupReport,
    QuarantineInfo, blob_filename, tokio::AtomicBlobStore,
};

const KEY_FORMAT_VERSION: u8 = 1;
const ENVELOPE_DOMAIN: &[u8; 8] = b"RUMQSESS";
const ENVELOPE_SUFFIX: &str = ".session";

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
    FileStore(AtomicBlobStoreError),
    SessionEncode(P::EncodeError),
    SessionDecode(P::DecodeError),
    KeyEncode(KeyEncodeError),
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
        }
    }
}

impl<P: Protocol> From<AtomicBlobStoreError> for AdapterError<P> {
    fn from(error: AtomicBlobStoreError) -> Self {
        Self::FileStore(error)
    }
}

impl<P: Protocol> From<KeyEncodeError> for AdapterError<P> {
    fn from(error: KeyEncodeError) -> Self {
        Self::KeyEncode(error)
    }
}

pub struct Adapter<P: Protocol> {
    core: AtomicBlobStore,
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
            .finish_non_exhaustive()
    }
}

impl<P: Protocol> Adapter<P> {
    pub(crate) async fn open(
        root: impl Into<PathBuf>,
        options: FileStoreOptions,
    ) -> Result<Self, AdapterError<P>> {
        let root = std::path::absolute(root.into()).map_err(|source| {
            AdapterError::FileStore(AtomicBlobStoreError::Io {
                operation: atomic_blob_store::StoreOperation::NormalizeRoot,
                source,
            })
        })?;
        let format = BlobFormatIdentity::new(
            ENVELOPE_DOMAIN,
            ENVELOPE_SUFFIX,
            atomic_blob_store::ENVELOPE_VERSION_V1,
        )
        .expect("the adapter format identity is valid");
        let core_options = AtomicBlobStoreOptions::new(format)
            .with_max_blob_size(options.max_checkpoint_size)
            .with_max_concurrent_operations(options.max_concurrent_operations);
        Ok(Self {
            core: AtomicBlobStore::open(&root, P::NAMESPACE, core_options).await?,
            protocol: PhantomData,
        })
    }

    pub(crate) fn checkpoint_path(
        &self,
        scope: &str,
        client_id: &str,
    ) -> Result<PathBuf, KeyEncodeError> {
        Ok(self.core.blob_path(&encode_key::<P>(scope, client_id)?))
    }

    pub(crate) async fn inspect(
        &self,
        scope: &str,
        client_id: &str,
    ) -> Result<CheckpointInspection, AdapterError<P>> {
        let canonical = self
            .core
            .inspect(&encode_key::<P>(scope, client_id)?)
            .await?;
        Ok(CheckpointInspection {
            state: match canonical.state {
                BlobState::Absent => CheckpointState::Absent,
                BlobState::Present => CheckpointState::Present,
            },
            size: canonical.size,
            modified: canonical.modified,
        })
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
        Ok(self.core.cleanup_stale_temporary_files(minimum_age).await?)
    }

    pub(crate) async fn load(
        &self,
        scope: &str,
        client_id: &str,
    ) -> Result<Option<P::Session>, AdapterError<P>> {
        let encoded_key = encode_key::<P>(scope, client_id)?;
        let mut payload = Vec::new();
        let Some(metadata) = self.core.load_into(&encoded_key, &mut payload).await? else {
            return Ok(None);
        };
        debug_assert_eq!(metadata.payload_len, payload.len() as u64);
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
        let payload_len = u64::try_from(payload.len()).map_err(|_| {
            AdapterError::FileStore(AtomicBlobStoreError::InvalidPayloadLength {
                declared: u64::MAX,
            })
        })?;
        self.core
            .save_from(&encoded_key, &mut Cursor::new(payload), payload_len)
            .await?;
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
    let format = BlobFormatIdentity::new(
        ENVELOPE_DOMAIN,
        ENVELOPE_SUFFIX,
        atomic_blob_store::ENVELOPE_VERSION_V1,
    )
    .expect("the adapter format identity is valid");
    Ok(blob_filename(&format, &encode_key::<P>(scope, client_id)?))
}

pub fn namespace<P: Protocol>() -> &'static Path {
    Path::new(P::NAMESPACE)
}
