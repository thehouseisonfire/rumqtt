use atomic_blob_store::{
    AtomicBlobStoreOptions, BlobFormatIdentity, ENVELOPE_VERSION_V1, tokio::AtomicBlobStore,
};
use std::io::Cursor;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let format = BlobFormatIdentity::new(b"WORKER01", ".state", ENVELOPE_VERSION_V1)?;
    let store =
        AtomicBlobStore::open(root.path(), "workers", AtomicBlobStoreOptions::new(format)).await?;

    let mut first_state = Cursor::new(41_u64.to_be_bytes());
    let mut second_state = Cursor::new(99_u64.to_be_bytes());
    let first = store.save_from(b"worker-a", &mut first_state, 8);
    let second = store.save_from(b"worker-b", &mut second_state, 8);
    let (first, second) = tokio::join!(first, second);
    first?;
    second?;
    store.flush().await?;
    Ok(())
}
