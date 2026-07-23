use atomic_blob_store::{
    AtomicBlobStore, AtomicBlobStoreOptions, BlobFormatIdentity, ENVELOPE_VERSION_V1,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let format = BlobFormatIdentity::new(b"WORKER01", ".state", ENVELOPE_VERSION_V1)?;
    let store =
        AtomicBlobStore::open(root.path(), "workers", AtomicBlobStoreOptions::new(format)).await?;

    let first = store.save(b"worker-a", 41_u64.to_be_bytes().to_vec());
    let second = store.save(b"worker-b", 99_u64.to_be_bytes().to_vec());
    let (first, second) = tokio::join!(first, second);
    first?;
    second?;
    store.flush().await?;
    Ok(())
}
