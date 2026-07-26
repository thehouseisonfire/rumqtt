#![cfg(any(unix, windows))]

mod common;

use common::test_directory;
use std::io::Cursor;

use atomic_blob_store::{
    AtomicBlobStoreError, AtomicBlobStoreOptions, BlobFormatIdentity, BlobState,
    BlockingAtomicBlobStore, ENVELOPE_VERSION_V1,
};

const CHUNK: usize = 64 * 1024;
const MAXIMUM: u64 = (2 * CHUNK + 17) as u64;
const DEFAULT_CASES: usize = 32;
const DEFAULT_OPERATIONS: usize = 24;
const DEFAULT_SEED: u64 = 0x5eed_cafe_d15c_a11e;

fn options() -> AtomicBlobStoreOptions {
    AtomicBlobStoreOptions::new(
        BlobFormatIdentity::new(b"MODELSEQ", ".blob", ENVELOPE_VERSION_V1).unwrap(),
    )
    .with_max_blob_size(MAXIMUM)
}

trait ModelStore {
    fn save(&mut self, key: &[u8], payload: Vec<u8>) -> Result<(), AtomicBlobStoreError>;
    fn save_stream(&mut self, key: &[u8], payload: &[u8]) -> Result<(), AtomicBlobStoreError>;
    fn load(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, AtomicBlobStoreError>;
    fn load_stream(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, AtomicBlobStoreError>;
    fn inspect(&mut self, key: &[u8]) -> Result<BlobState, AtomicBlobStoreError>;
    fn clear(&mut self, key: &[u8]) -> Result<(), AtomicBlobStoreError>;
    fn quarantine(&mut self, key: &[u8]) -> Result<(), AtomicBlobStoreError>;
    fn flush(&mut self) -> Result<(), AtomicBlobStoreError>;
    fn close(&mut self) -> Result<(), AtomicBlobStoreError>;
}

struct BlockingModelStore(BlockingAtomicBlobStore);

impl ModelStore for BlockingModelStore {
    fn save(&mut self, key: &[u8], payload: Vec<u8>) -> Result<(), AtomicBlobStoreError> {
        self.0.save(key, payload)
    }

    fn save_stream(&mut self, key: &[u8], payload: &[u8]) -> Result<(), AtomicBlobStoreError> {
        self.0
            .save_from(key, &mut Cursor::new(payload), payload.len() as u64)
    }

    fn load(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, AtomicBlobStoreError> {
        self.0.load(key)
    }

    fn load_stream(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, AtomicBlobStoreError> {
        let mut output = Vec::new();
        self.0
            .load_into(key, &mut output)
            .map(|metadata| metadata.map(|_| output))
    }

    fn inspect(&mut self, key: &[u8]) -> Result<BlobState, AtomicBlobStoreError> {
        self.0.inspect(key).map(|inspection| inspection.state)
    }

    fn clear(&mut self, key: &[u8]) -> Result<(), AtomicBlobStoreError> {
        self.0.clear(key)
    }

    fn quarantine(&mut self, key: &[u8]) -> Result<(), AtomicBlobStoreError> {
        self.0.quarantine(key).map(|quarantine| {
            assert!(quarantine.diagnostic_path.is_file());
        })
    }

    fn flush(&mut self) -> Result<(), AtomicBlobStoreError> {
        self.0.flush()
    }

    fn close(&mut self) -> Result<(), AtomicBlobStoreError> {
        self.0.close()
    }
}

#[cfg(feature = "tokio")]
struct TokioModelStore {
    runtime: tokio::runtime::Runtime,
    store: atomic_blob_store::tokio::AtomicBlobStore,
}

#[cfg(feature = "tokio")]
impl ModelStore for TokioModelStore {
    fn save(&mut self, key: &[u8], payload: Vec<u8>) -> Result<(), AtomicBlobStoreError> {
        self.runtime.block_on(self.store.save(key, payload))
    }

    fn save_stream(&mut self, key: &[u8], payload: &[u8]) -> Result<(), AtomicBlobStoreError> {
        self.runtime.block_on(self.store.save_from(
            key,
            &mut Cursor::new(payload),
            payload.len() as u64,
        ))
    }

    fn load(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, AtomicBlobStoreError> {
        self.runtime.block_on(self.store.load(key))
    }

    fn load_stream(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, AtomicBlobStoreError> {
        let mut output = Vec::new();
        self.runtime
            .block_on(self.store.load_into(key, &mut output))
            .map(|metadata| metadata.map(|_| output))
    }

    fn inspect(&mut self, key: &[u8]) -> Result<BlobState, AtomicBlobStoreError> {
        self.runtime
            .block_on(self.store.inspect(key))
            .map(|inspection| inspection.state)
    }

    fn clear(&mut self, key: &[u8]) -> Result<(), AtomicBlobStoreError> {
        self.runtime.block_on(self.store.clear(key))
    }

    fn quarantine(&mut self, key: &[u8]) -> Result<(), AtomicBlobStoreError> {
        self.runtime
            .block_on(self.store.quarantine(key))
            .map(|quarantine| {
                assert!(quarantine.diagnostic_path.is_file());
            })
    }

    fn flush(&mut self) -> Result<(), AtomicBlobStoreError> {
        self.runtime.block_on(self.store.flush())
    }

    fn close(&mut self) -> Result<(), AtomicBlobStoreError> {
        self.runtime.block_on(self.store.close())
    }
}

struct Generator(u64);

impl Generator {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn index(&mut self, length: usize) -> usize {
        usize::try_from(self.next() % length as u64).unwrap()
    }
}

fn payload(generator: &mut Generator) -> Vec<u8> {
    let sizes = [0, 1, CHUNK, CHUNK + 1, 2 * CHUNK + 17];
    let size = sizes[generator.index(sizes.len())];
    let byte = generator.next().to_le_bytes()[0];
    vec![byte; size]
}

fn run_model(mut store: impl ModelStore, seed: u64, operation_count: usize) {
    let keys: [&[u8]; 3] = [b"alpha", b"beta", b"gamma"];
    let mut model = [None, None, None];
    let mut generator = Generator(seed.max(1));
    println!("atomic-blob-store model seed={seed:#018x}");

    for _ in 0..operation_count {
        let key_index = generator.index(keys.len());
        let key = keys[key_index];
        match generator.index(8) {
            0 => {
                let value = payload(&mut generator);
                store.save(key, value.clone()).unwrap();
                model[key_index] = Some(value);
            }
            1 => {
                let value = payload(&mut generator);
                store.save_stream(key, &value).unwrap();
                model[key_index] = Some(value);
            }
            2 => assert_eq!(store.load(key).unwrap(), model[key_index]),
            3 => assert_eq!(store.load_stream(key).unwrap(), model[key_index]),
            4 => assert_eq!(
                store.inspect(key).unwrap(),
                if model[key_index].is_some() {
                    BlobState::Present
                } else {
                    BlobState::Absent
                }
            ),
            5 => {
                store.clear(key).unwrap();
                model[key_index] = None;
            }
            6 => {
                let result = store.quarantine(key);
                if model[key_index].take().is_some() {
                    result.unwrap();
                } else {
                    assert!(matches!(
                        result,
                        Err(AtomicBlobStoreError::QuarantineSourceMissing)
                    ));
                }
            }
            7 => store.flush().unwrap(),
            _ => unreachable!(),
        }
        for (index, key) in keys.iter().enumerate() {
            assert_eq!(store.load(key).unwrap(), model[index]);
        }
    }

    store.flush().unwrap();
    store.close().unwrap();
    store.close().unwrap();
    assert!(matches!(
        store.load(keys[0]),
        Err(AtomicBlobStoreError::StoreClosed)
    ));
}

fn configured_cases() -> (usize, u64) {
    let cases = std::env::var("ATOMIC_BLOB_MODEL_CASES")
        .ok()
        .map(|value| {
            value
                .parse()
                .expect("ATOMIC_BLOB_MODEL_CASES must be an integer")
        })
        .unwrap_or(DEFAULT_CASES);
    let seed = std::env::var("ATOMIC_BLOB_MODEL_SEED")
        .ok()
        .map(|value| {
            let value = value.trim_start_matches("0x");
            u64::from_str_radix(value, 16).expect("ATOMIC_BLOB_MODEL_SEED must be hexadecimal")
        })
        .unwrap_or(DEFAULT_SEED);
    (cases, seed)
}

#[test]
fn blocking_sequences_match_the_reference_model() {
    let (cases, base_seed) = configured_cases();
    for case in 0..cases {
        let root = test_directory();
        let store = BlockingAtomicBlobStore::open(root.path(), "model", options()).unwrap();
        run_model(
            BlockingModelStore(store),
            base_seed.wrapping_add(case as u64),
            DEFAULT_OPERATIONS,
        );
    }
}

#[cfg(feature = "tokio")]
#[test]
fn tokio_sequences_match_the_reference_model() {
    let (cases, base_seed) = configured_cases();
    for case in 0..cases {
        let root = test_directory();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let store = runtime
            .block_on(atomic_blob_store::tokio::AtomicBlobStore::open(
                root.path(),
                "model",
                options(),
            ))
            .unwrap();
        run_model(
            TokioModelStore { runtime, store },
            base_seed.wrapping_add(case as u64),
            DEFAULT_OPERATIONS,
        );
    }
}
