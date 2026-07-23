use atomic_blob_store::{
    AtomicBlobStore, AtomicBlobStoreOptions, BlobFormatIdentity, BlobState, ENVELOPE_VERSION_V1,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let format = BlobFormatIdentity::new(b"DIAG0001", ".blob", ENVELOPE_VERSION_V1)?;
    let store = AtomicBlobStore::open(
        root.path(),
        "diagnostics",
        AtomicBlobStoreOptions::new(format),
    )
    .await?;
    let key = b"latest";
    store.save(key, b"healthy".to_vec()).await?;

    let path = store.blob_path(key);
    let mut bytes = std::fs::read(&path)?;
    bytes[18] ^= 1;
    std::fs::write(&path, bytes)?;
    assert!(store.load(key).await.is_err());
    assert_eq!(store.inspect(key).await?.state, BlobState::Present);
    println!("{}", store.quarantine(key).await?.diagnostic_path.display());
    Ok(())
}
