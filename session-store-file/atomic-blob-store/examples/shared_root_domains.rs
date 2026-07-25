use atomic_blob_store::{
    AtomicBlobStoreOptions, BlobFormatIdentity, ENVELOPE_VERSION_V1, tokio::AtomicBlobStore,
};
use std::io::Cursor;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let alpha = BlobFormatIdentity::new(b"DOMAIN-A", ".blob", ENVELOPE_VERSION_V1)?;
    let beta = BlobFormatIdentity::new(b"DOMAIN-B", ".blob", ENVELOPE_VERSION_V1)?;
    let alpha =
        AtomicBlobStore::open(root.path(), "alpha", AtomicBlobStoreOptions::new(alpha)).await?;
    let beta =
        AtomicBlobStore::open(root.path(), "beta", AtomicBlobStoreOptions::new(beta)).await?;

    alpha
        .save_from(b"shared-key", &mut Cursor::new(b"alpha"), 5)
        .await?;
    beta.save_from(b"shared-key", &mut Cursor::new(b"beta"), 4)
        .await?;
    let mut alpha_value = Vec::new();
    let mut beta_value = Vec::new();
    alpha.load_into(b"shared-key", &mut alpha_value).await?;
    beta.load_into(b"shared-key", &mut beta_value).await?;
    assert_eq!(alpha_value, b"alpha");
    assert_eq!(beta_value, b"beta");
    Ok(())
}
