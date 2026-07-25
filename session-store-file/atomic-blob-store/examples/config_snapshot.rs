use atomic_blob_store::{
    AtomicBlobStoreOptions, BlobFormatIdentity, ENVELOPE_VERSION_V1, tokio::AtomicBlobStore,
};
use std::io::Cursor;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let format = BlobFormatIdentity::new(b"APP-CONF", ".config", ENVELOPE_VERSION_V1)?;
    let store = AtomicBlobStore::open(
        root.path(),
        "configuration",
        AtomicBlobStoreOptions::new(format),
    )
    .await?;

    let document = br#"{"theme":"dark"}"#;
    store
        .save_from(
            b"active",
            &mut Cursor::new(document),
            u64::try_from(document.len())?,
        )
        .await?;
    let mut restored = Vec::new();
    store
        .load_into(b"active", &mut restored)
        .await?
        .expect("saved configuration");
    println!("{}", String::from_utf8(restored)?);
    Ok(())
}
