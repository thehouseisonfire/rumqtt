use atomic_blob_store::{
    AtomicBlobStore, AtomicBlobStoreOptions, BlobFormatIdentity, ENVELOPE_VERSION_V1,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let format = BlobFormatIdentity::new(b"APP-CONF", ".config", ENVELOPE_VERSION_V1)?;
    let store = AtomicBlobStore::open(
        root.path(),
        "configuration",
        AtomicBlobStoreOptions::new(format),
    )
    .await?;

    store
        .save(b"active", br#"{"theme":"dark"}"#.to_vec())
        .await?;
    let restored = store.load(b"active").await?.expect("saved configuration");
    println!("{}", String::from_utf8(restored)?);
    Ok(())
}
