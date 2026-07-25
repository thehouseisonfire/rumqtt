use std::io::Cursor;

use atomic_blob_store::{
    AtomicBlobStoreOptions, BlobFormatIdentity, BlockingAtomicBlobStore, ENVELOPE_VERSION_V1,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let format = BlobFormatIdentity::new(b"APP-CONF", ".config", ENVELOPE_VERSION_V1)?;
    let store = BlockingAtomicBlobStore::open(
        root.path(),
        "configuration",
        AtomicBlobStoreOptions::new(format),
    )?;

    let mut source = Cursor::new(br#"{"workers":4}"#);
    store.save_from(b"current", &mut source, 13)?;
    let mut restored = Vec::new();
    store.load_into(b"current", &mut restored)?;
    store.close()?;
    assert_eq!(restored, br#"{"workers":4}"#);
    Ok(())
}
