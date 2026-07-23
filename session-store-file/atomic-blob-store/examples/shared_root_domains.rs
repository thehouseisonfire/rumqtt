use atomic_blob_store::{
    AtomicBlobStore, AtomicBlobStoreOptions, BlobFormatIdentity, ENVELOPE_VERSION_V1,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let alpha = BlobFormatIdentity::new(b"DOMAIN-A", ".blob", ENVELOPE_VERSION_V1)?;
    let beta = BlobFormatIdentity::new(b"DOMAIN-B", ".blob", ENVELOPE_VERSION_V1)?;
    let alpha =
        AtomicBlobStore::open(root.path(), "alpha", AtomicBlobStoreOptions::new(alpha)).await?;
    let beta =
        AtomicBlobStore::open(root.path(), "beta", AtomicBlobStoreOptions::new(beta)).await?;

    alpha.save(b"shared-key", b"alpha".to_vec()).await?;
    beta.save(b"shared-key", b"beta".to_vec()).await?;
    assert_eq!(alpha.load(b"shared-key").await?, Some(b"alpha".to_vec()));
    assert_eq!(beta.load(b"shared-key").await?, Some(b"beta".to_vec()));
    Ok(())
}
