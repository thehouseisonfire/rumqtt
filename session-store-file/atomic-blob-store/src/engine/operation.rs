#[allow(unused_imports)]
use super::*;
#[cfg(any(unix, windows))]
pub(crate) enum Operation {
    Load {
        sender: Sender<Result<Option<Vec<u8>>, AtomicBlobStoreError>>,
    },
    LoadStream {
        chunks: Option<Sender<Vec<u8>>>,
        acknowledgement: Option<Receiver<()>>,
        sender: Sender<Result<Option<BlobMetadata>, AtomicBlobStoreError>>,
    },
    Save {
        payload: Vec<u8>,
        sender: Sender<Result<(), AtomicBlobStoreError>>,
    },
    SaveStream {
        declared_len: u64,
        chunks: Receiver<SaveStreamMessage>,
        sender: Sender<Result<(), AtomicBlobStoreError>>,
    },
    Clear {
        sender: Sender<Result<(), AtomicBlobStoreError>>,
    },
    Inspect {
        sender: Sender<Result<BlobInspection, AtomicBlobStoreError>>,
    },
    Quarantine {
        sender: Sender<Result<QuarantineInfo, AtomicBlobStoreError>>,
    },
}

#[cfg(any(unix, windows))]
pub(crate) enum BlockingResult {
    Load(Result<Option<Vec<u8>>, AtomicBlobStoreError>),
    LoadStream(Result<Option<BlobMetadata>, AtomicBlobStoreError>),
    Save(Result<(), AtomicBlobStoreError>),
    SaveStream(Result<(), AtomicBlobStoreError>),
    Clear(Result<(), AtomicBlobStoreError>),
    Inspect(Result<BlobInspection, AtomicBlobStoreError>),
    Quarantine(Result<QuarantineInfo, AtomicBlobStoreError>),
}

#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
pub(crate) enum SaveStreamMessage {
    Chunk(Vec<u8>),
    Complete,
}

#[cfg(any(unix, windows))]
pub(crate) fn run_owned_operation(
    config: &StoreConfig,
    path: &Path,
    operation: Operation,
) -> (Operation, BlockingResult) {
    match operation {
        Operation::Load { sender } => {
            let result = load_blob(config, path);
            (Operation::Load { sender }, BlockingResult::Load(result))
        }
        Operation::LoadStream {
            mut chunks,
            mut acknowledgement,
            sender,
        } => {
            let result = load_blob_into_sender(
                config,
                path,
                chunks
                    .take()
                    .expect("a queued streaming load owns its chunk sender"),
                acknowledgement
                    .take()
                    .expect("a queued streaming load owns its acknowledgement"),
            );
            (
                Operation::LoadStream {
                    chunks,
                    acknowledgement,
                    sender,
                },
                BlockingResult::LoadStream(result),
            )
        }
        Operation::Save { payload, sender } => {
            #[cfg(any(unix, windows))]
            let result = save_blob(config, path, &payload);
            #[cfg(not(any(unix, windows)))]
            let result = Err(AtomicBlobStoreError::UnsupportedPlatform {
                platform: std::env::consts::OS,
            });
            (
                Operation::Save { payload, sender },
                BlockingResult::Save(result),
            )
        }
        Operation::SaveStream {
            declared_len,
            mut chunks,
            sender,
        } => {
            let result = save_blob_from_receiver(config, path, declared_len, &mut chunks);
            (
                Operation::SaveStream {
                    declared_len,
                    chunks,
                    sender,
                },
                BlockingResult::SaveStream(result),
            )
        }
        Operation::Clear { sender } => {
            let result = clear_blob(config, path);
            (Operation::Clear { sender }, BlockingResult::Clear(result))
        }
        Operation::Inspect { sender } => (
            Operation::Inspect { sender },
            BlockingResult::Inspect(inspect_blob(config, path)),
        ),
        Operation::Quarantine { sender } => (
            Operation::Quarantine { sender },
            BlockingResult::Quarantine(quarantine_blob(config, path)),
        ),
    }
}

#[cfg(any(unix, windows))]
pub(crate) fn deliver(operation: Operation, result: BlockingResult) {
    match (operation, result) {
        (Operation::Load { sender }, BlockingResult::Load(result)) => {
            let _send_result = sender.send(result);
        }
        (Operation::LoadStream { sender, .. }, BlockingResult::LoadStream(result)) => {
            let _send_result = sender.send(result);
        }
        (Operation::Save { sender, .. }, BlockingResult::Save(result))
        | (Operation::SaveStream { sender, .. }, BlockingResult::SaveStream(result))
        | (Operation::Clear { sender }, BlockingResult::Clear(result)) => {
            let _send_result = sender.send(result);
        }
        (Operation::Inspect { sender }, BlockingResult::Inspect(result)) => {
            let _send_result = sender.send(result);
        }
        (Operation::Quarantine { sender }, BlockingResult::Quarantine(result)) => {
            let _send_result = sender.send(result);
        }
        _ => unreachable!("operation and result kinds must match"),
    }
}

#[cfg(any(unix, windows))]
pub(crate) fn deliver_error(operation: Operation, error: AtomicBlobStoreError) {
    match operation {
        Operation::Load { sender } => {
            let _ = sender.send(Err(error));
        }
        Operation::LoadStream { sender, .. } => {
            let _ = sender.send(Err(error));
        }
        Operation::Save { sender, .. }
        | Operation::SaveStream { sender, .. }
        | Operation::Clear { sender } => {
            let _ = sender.send(Err(error));
        }
        Operation::Inspect { sender } => {
            let _ = sender.send(Err(error));
        }
        Operation::Quarantine { sender } => {
            let _ = sender.send(Err(error));
        }
    }
}
