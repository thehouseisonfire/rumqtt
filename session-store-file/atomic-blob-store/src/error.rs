/// Observable state of a canonical blob.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlobState {
    Absent,
    Present,
}

/// Metadata returned without opening or decoding blob contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobInspection {
    pub state: BlobState,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
}

/// Metadata for a successfully validated blob payload.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlobMetadata {
    /// Exact number of payload bytes in the envelope.
    pub payload_len: u64,
}

/// Location assigned to a quarantined blob.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuarantineInfo {
    pub identifier: String,
    pub diagnostic_path: PathBuf,
}

/// One cleanup entry that could not be inspected or removed.
#[derive(Debug)]
pub struct CleanupFailure {
    pub identifier: String,
    pub source: io::Error,
}

/// Outcome of a store-wide stale temporary-file cleanup.
#[derive(Debug, Default)]
pub struct CleanupReport {
    pub removed: Vec<String>,
    pub skipped: Vec<String>,
    pub failures: Vec<CleanupFailure>,
}

/// Filesystem operation associated with an ordinary I/O error.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreOperation {
    ResolveCurrentDirectory,
    NormalizeRoot,
    StartInitialization,
    StartCoordinator,
    InspectRoot,
    InspectNamespace,
    CreateNamespace,
    OpenRootDirectory,
    SyncRootDirectory,
    OpenBlob,
    ReadEnvelope,
    OpenAtomicWriter,
    WriteEnvelope,
    RemoveBlob,
    OpenNamespaceDirectory,
    SyncNamespaceDirectory,
    InspectBlob,
    QuarantineBlob,
    GenerateIdentifier,
    EnumerateTemporaryFiles,
    RemoveTemporaryFile,
    RefreshClearStagingAge,
}

impl std::fmt::Display for StoreOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ResolveCurrentDirectory => "resolve the current directory",
            Self::NormalizeRoot => "normalize the configured root path",
            Self::StartInitialization => "start store initialization",
            Self::StartCoordinator => "start the blob-store coordinator",
            Self::InspectRoot => "inspect configured root",
            Self::InspectNamespace => "inspect namespace directory",
            Self::CreateNamespace => "create namespace directory",
            Self::OpenRootDirectory => "open configured root directory",
            Self::SyncRootDirectory => "synchronize configured root directory",
            Self::OpenBlob => "open blob",
            Self::ReadEnvelope => "read blob envelope",
            Self::OpenAtomicWriter => "open atomic blob writer",
            Self::WriteEnvelope => "write blob envelope",
            Self::RemoveBlob => "remove blob",
            Self::OpenNamespaceDirectory => "open namespace directory",
            Self::SyncNamespaceDirectory => "synchronize namespace directory",
            Self::InspectBlob => "inspect blob",
            Self::QuarantineBlob => "quarantine blob",
            Self::GenerateIdentifier => "generate a diagnostic identifier",
            Self::EnumerateTemporaryFiles => "enumerate owned temporary files",
            Self::RemoveTemporaryFile => "remove an owned temporary file",
            Self::RefreshClearStagingAge => "refresh clear-staging age",
        })
    }
}

/// Envelope section that ended prematurely.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvelopeSection {
    Magic,
    Version,
    PayloadLength,
    Payload,
    Checksum,
}

impl std::fmt::Display for EnvelopeSection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Magic => "magic",
            Self::Version => "version",
            Self::PayloadLength => "payload length",
            Self::Payload => "payload",
            Self::Checksum => "checksum",
        })
    }
}

/// Protocol-neutral failure from the blob store.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum AtomicBlobStoreError {
    #[error("invalid store configuration: {0}")]
    Configuration(#[from] AtomicBlobStoreConfigError),
    #[error("I/O failure while attempting to {operation}: {source}")]
    Io {
        operation: StoreOperation,
        #[source]
        source: io::Error,
    },
    #[error("atomic blob commit failed: {source}")]
    AtomicCommit {
        #[source]
        source: io::Error,
    },
    #[error("configured root does not exist")]
    RootDoesNotExist,
    #[error("configured root is not a directory")]
    RootIsNotDirectory,
    #[error("namespace path exists but is not a directory")]
    NamespacePathIsNotDirectory,
    #[error("blob envelope belongs to a different domain")]
    InvalidEnvelopeDomain {
        expected: [u8; DOMAIN_TAG_LEN],
        found: [u8; DOMAIN_TAG_LEN],
    },
    #[error("unsupported blob envelope version {found}")]
    UnsupportedEnvelopeVersion { found: u16 },
    #[error("blob envelope ended while reading {section}")]
    TruncatedEnvelope { section: EnvelopeSection },
    #[error("blob payload size {size} exceeds configured maximum {maximum}")]
    BlobTooLarge { size: u64, maximum: u64 },
    #[error("declared blob payload length {declared} cannot be represented safely")]
    InvalidPayloadLength { declared: u64 },
    #[error("blob envelope contains trailing data")]
    TrailingData,
    #[error("blob checksum mismatch: stored {expected:#010x}, calculated {actual:#010x}")]
    ChecksumMismatch { expected: u32, actual: u32 },
    #[error("streaming input ended after {actual} bytes; {declared} bytes were declared")]
    InputEndedEarly { declared: u64, actual: u64 },
    #[error("streaming input contains data after the declared {declared} bytes")]
    InputHasTrailingData { declared: u64 },
    #[error("failed to read streaming input: {source}")]
    InputIo {
        #[source]
        source: io::Error,
    },
    #[error("failed to write streaming output: {source}")]
    OutputIo {
        #[source]
        source: io::Error,
    },
    #[error("streaming transfer was cancelled before completion")]
    StreamCancelled,
    #[error("file-backed blob storage is unsupported on {platform}")]
    UnsupportedPlatform { platform: &'static str },
    #[error("blob-store operation coordination failed")]
    CoordinationFailure,
    #[error("blob store is closed")]
    StoreClosed,
    #[error("blob-store execution engine failed")]
    EngineFailed,
    #[error("blob-store worker facility is unavailable")]
    WorkerUnavailable,
    #[error("blob-store shutdown failed")]
    ShutdownFailure,
    #[error("canonical blob to quarantine does not exist")]
    QuarantineSourceMissing,
    #[error("blob quarantine failed: {source}")]
    QuarantineCommit {
        #[source]
        source: io::Error,
    },
    #[error("blob was moved to quarantine, but namespace synchronization failed: {source}")]
    QuarantineNamespaceSync {
        quarantine: QuarantineInfo,
        #[source]
        source: io::Error,
    },
    #[error("stale temporary-file cleanup is unsupported on {platform}")]
    CleanupUnsupported { platform: &'static str },
    #[error("cleanup minimum age must be greater than zero")]
    InvalidCleanupAge,
    #[cfg(any(unix, windows))]
    #[error("failed to obtain a random diagnostic identifier: {source}")]
    IdentifierGeneration {
        #[source]
        source: getrandom::Error,
    },
    #[error("blob path has an unexpected file type")]
    UnexpectedFileType,
    #[error("store-wide maintenance coordination failed")]
    MaintenanceCoordinationFailure,
}
use std::io;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::{AtomicBlobStoreConfigError, DOMAIN_TAG_LEN};
