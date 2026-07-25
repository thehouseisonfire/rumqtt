use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::{AtomicBlobStoreConfigError, AtomicBlobStoreError, BlobFormatIdentity};

/// Returns the stable full-BLAKE3 blob filename for canonical key bytes.
#[must_use]
pub fn blob_filename(format: &BlobFormatIdentity, canonical_key: &[u8]) -> String {
    format!(
        "{}{}",
        blake3::hash(canonical_key).to_hex(),
        format.filename_suffix
    )
}

#[cfg(any(unix, windows))]
pub(crate) fn key_hash(canonical_key: &[u8]) -> [u8; 32] {
    *blake3::hash(canonical_key).as_bytes()
}

pub(crate) fn validate_namespace(namespace: &OsStr) -> Result<PathBuf, AtomicBlobStoreError> {
    let path = Path::new(namespace);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if !component.is_empty() => {
            Ok(PathBuf::from(component))
        }
        _ => Err(AtomicBlobStoreConfigError::InvalidNamespace.into()),
    }
}
