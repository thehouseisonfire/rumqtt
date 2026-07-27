use super::*;
#[cfg(any(unix, windows))]
#[cfg_attr(not(any(test, feature = "bench-instrumentation")), allow(dead_code))]
pub(crate) fn encode_envelope(
    format: &BlobFormatIdentity,
    payload: &[u8],
    maximum: u64,
) -> Result<Vec<u8>, AtomicBlobStoreError> {
    let (header, checksum) = envelope_parts(format, payload, maximum)?;
    let mut envelope = Vec::with_capacity(header.len() + payload.len() + checksum.len());
    envelope.extend_from_slice(&header);
    envelope.extend_from_slice(payload);
    envelope.extend_from_slice(&checksum);
    Ok(envelope)
}

#[cfg(any(unix, windows))]
pub(crate) fn envelope_parts(
    format: &BlobFormatIdentity,
    payload: &[u8],
    maximum: u64,
) -> Result<([u8; HEADER_LEN], [u8; CHECKSUM_LEN]), AtomicBlobStoreError> {
    let size = u64::try_from(payload.len())
        .map_err(|_| AtomicBlobStoreError::InvalidPayloadLength { declared: u64::MAX })?;
    let header = envelope_header(format, size, maximum)?;
    let checksum = crc32c::crc32c_append(crc32c::crc32c(&header), payload).to_be_bytes();
    Ok((header, checksum))
}

#[cfg(any(unix, windows))]
pub(crate) fn envelope_header(
    format: &BlobFormatIdentity,
    size: u64,
    maximum: u64,
) -> Result<[u8; HEADER_LEN], AtomicBlobStoreError> {
    if size > maximum {
        return Err(AtomicBlobStoreError::BlobTooLarge { size, maximum });
    }
    let mut header = [0; HEADER_LEN];
    header[..DOMAIN_TAG_LEN].copy_from_slice(format.domain_tag());
    header[DOMAIN_TAG_LEN..DOMAIN_TAG_LEN + 2]
        .copy_from_slice(&format.envelope_version().to_be_bytes());
    header[DOMAIN_TAG_LEN + 2..].copy_from_slice(&size.to_be_bytes());
    Ok(header)
}

#[cfg(any(unix, windows))]
pub(crate) fn write_stream_envelope(
    config: &StoreConfig,
    writer: &mut impl Write,
    declared_len: u64,
    chunks: &mut Receiver<SaveStreamMessage>,
) -> Result<(), AtomicBlobStoreError> {
    let header = envelope_header(&config.format, declared_len, config.maximum)?;
    writer
        .write_all(&header)
        .map_err(|source| AtomicBlobStoreError::Io {
            operation: StoreOperation::WriteEnvelope,
            source,
        })?;
    let mut checksum = crc32c::crc32c(&header);
    let mut written = 0_u64;
    #[cfg(feature = "bench-instrumentation")]
    let mut input_starvation_reported = false;
    #[cfg(all(test, any(unix, windows)))]
    let mut during_write_hook_hit = false;

    loop {
        #[cfg(feature = "bench-instrumentation")]
        if !input_starvation_reported && chunks.is_empty() {
            emit_benchmark_event(
                config,
                crate::bench_instrumentation::BenchmarkEvent::SaveStreamInputStarved,
            );
            input_starvation_reported = true;
        }
        let Ok(message) = chunks.recv() else {
            break;
        };
        match message {
            SaveStreamMessage::Chunk(chunk) => {
                let count = u64::try_from(chunk.len()).expect("a chunk length always fits in u64");
                if written
                    .checked_add(count)
                    .is_none_or(|total| total > declared_len)
                {
                    return Err(AtomicBlobStoreError::InputHasTrailingData {
                        declared: declared_len,
                    });
                }
                writer
                    .write_all(&chunk)
                    .map_err(|source| AtomicBlobStoreError::Io {
                        operation: StoreOperation::WriteEnvelope,
                        source,
                    })?;
                checksum = crc32c::crc32c_append(checksum, &chunk);
                written += count;
                #[cfg(all(test, any(unix, windows)))]
                if !during_write_hook_hit {
                    hit_test_stage(
                        config,
                        TestStage::DuringWrite,
                        StoreOperation::WriteEnvelope,
                    )?;
                }
                #[cfg(all(test, any(unix, windows)))]
                {
                    during_write_hook_hit = true;
                }
            }
            SaveStreamMessage::Complete => {
                if written != declared_len {
                    return Err(AtomicBlobStoreError::InputEndedEarly {
                        declared: declared_len,
                        actual: written,
                    });
                }
                writer
                    .write_all(&checksum.to_be_bytes())
                    .map_err(|source| AtomicBlobStoreError::Io {
                        operation: StoreOperation::WriteEnvelope,
                        source,
                    })?;
                return Ok(());
            }
        }
    }
    Err(AtomicBlobStoreError::StreamCancelled)
}

#[cfg(any(unix, windows))]
pub(crate) fn load_blob(
    config: &StoreConfig,
    path: &Path,
) -> Result<Option<Vec<u8>>, AtomicBlobStoreError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_namespace_available(config)?;
            return Ok(None);
        }
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::OpenBlob,
                source,
            });
        }
    };
    decode_reader(&config.format, &mut file, config.maximum).map(Some)
}

#[cfg(any(unix, windows))]
pub(crate) fn load_blob_into_sender(
    config: &StoreConfig,
    path: &Path,
    chunks: Sender<Vec<u8>>,
    acknowledgement: Receiver<()>,
) -> Result<Option<BlobMetadata>, AtomicBlobStoreError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_namespace_available(config)?;
            return Ok(None);
        }
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::OpenBlob,
                source,
            });
        }
    };
    let metadata = validate_envelope_reader(&config.format, &mut file, config.maximum)?;
    file.seek(SeekFrom::Start(
        u64::try_from(HEADER_LEN).expect("the fixed header length fits in u64"),
    ))
    .map_err(|source| AtomicBlobStoreError::Io {
        operation: StoreOperation::ReadEnvelope,
        source,
    })?;

    let mut remaining = metadata.payload_len;
    #[cfg(feature = "bench-instrumentation")]
    let mut output_backpressure_reported = false;
    while remaining != 0 {
        let requested = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(STREAM_CHUNK_SIZE);
        let mut chunk = vec![0; requested];
        read_section(&mut file, &mut chunk, EnvelopeSection::Payload)?;
        remaining -= u64::try_from(requested).expect("a chunk length always fits in u64");
        #[cfg(feature = "bench-instrumentation")]
        if !output_backpressure_reported && chunks.is_full() {
            emit_benchmark_event(
                config,
                crate::bench_instrumentation::BenchmarkEvent::LoadStreamOutputBackpressured,
            );
            output_backpressure_reported = true;
        }
        chunks
            .send(chunk)
            .map_err(|_| AtomicBlobStoreError::StreamCancelled)?;
    }
    drop(chunks);
    acknowledgement
        .recv()
        .map_err(|_| AtomicBlobStoreError::StreamCancelled)?;
    Ok(Some(metadata))
}

#[cfg(any(unix, windows))]
pub(crate) fn validate_envelope_reader(
    format: &BlobFormatIdentity,
    reader: &mut impl Read,
    maximum: u64,
) -> Result<BlobMetadata, AtomicBlobStoreError> {
    let mut header = [0; HEADER_LEN];
    read_section(
        reader,
        &mut header[..DOMAIN_TAG_LEN],
        EnvelopeSection::Magic,
    )?;
    let found = <[u8; DOMAIN_TAG_LEN]>::try_from(&header[..DOMAIN_TAG_LEN])
        .expect("the domain header slice has the required length");
    if &found != format.domain_tag() {
        return Err(AtomicBlobStoreError::InvalidEnvelopeDomain {
            expected: *format.domain_tag(),
            found,
        });
    }

    read_section(
        reader,
        &mut header[DOMAIN_TAG_LEN..DOMAIN_TAG_LEN + 2],
        EnvelopeSection::Version,
    )?;
    let version = u16::from_be_bytes(
        header[DOMAIN_TAG_LEN..DOMAIN_TAG_LEN + 2]
            .try_into()
            .expect("the version slice has two bytes"),
    );
    if version != format.envelope_version() {
        return Err(AtomicBlobStoreError::UnsupportedEnvelopeVersion { found: version });
    }

    read_section(
        reader,
        &mut header[DOMAIN_TAG_LEN + 2..],
        EnvelopeSection::PayloadLength,
    )?;
    let declared = u64::from_be_bytes(
        header[DOMAIN_TAG_LEN + 2..]
            .try_into()
            .expect("the payload-length slice has eight bytes"),
    );
    if declared > maximum {
        return Err(AtomicBlobStoreError::BlobTooLarge {
            size: declared,
            maximum,
        });
    }

    let mut actual = crc32c::crc32c(&header);
    let mut remaining = declared;
    let mut buffer = vec![0; STREAM_CHUNK_SIZE];
    while remaining != 0 {
        let requested = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        read_section(reader, &mut buffer[..requested], EnvelopeSection::Payload)?;
        actual = crc32c::crc32c_append(actual, &buffer[..requested]);
        remaining -= u64::try_from(requested).expect("a chunk length always fits in u64");
    }

    let mut checksum = [0; CHECKSUM_LEN];
    read_section(reader, &mut checksum, EnvelopeSection::Checksum)?;
    let mut trailing = [0; 1];
    match reader.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => return Err(AtomicBlobStoreError::TrailingData),
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::ReadEnvelope,
                source,
            });
        }
    }

    let expected = u32::from_be_bytes(checksum);
    if expected != actual {
        return Err(AtomicBlobStoreError::ChecksumMismatch { expected, actual });
    }
    Ok(BlobMetadata {
        payload_len: declared,
    })
}

#[cfg(any(unix, windows))]
pub(crate) fn decode_reader(
    format: &BlobFormatIdentity,
    reader: &mut impl Read,
    maximum: u64,
) -> Result<Vec<u8>, AtomicBlobStoreError> {
    decode_reader_with_usize_limit(format, reader, maximum, usize::MAX as u64)
}

#[cfg(any(unix, windows))]
pub(crate) fn decode_reader_with_usize_limit(
    format: &BlobFormatIdentity,
    reader: &mut impl Read,
    maximum: u64,
    usize_limit: u64,
) -> Result<Vec<u8>, AtomicBlobStoreError> {
    let mut magic = [0; 8];
    read_section(reader, &mut magic, EnvelopeSection::Magic)?;
    if &magic != format.domain_tag() {
        return Err(AtomicBlobStoreError::InvalidEnvelopeDomain {
            expected: *format.domain_tag(),
            found: magic,
        });
    }

    let mut version = [0; 2];
    read_section(reader, &mut version, EnvelopeSection::Version)?;
    let version = u16::from_be_bytes(version);
    if version != format.envelope_version() {
        return Err(AtomicBlobStoreError::UnsupportedEnvelopeVersion { found: version });
    }

    let mut length = [0; 8];
    read_section(reader, &mut length, EnvelopeSection::PayloadLength)?;
    let declared = u64::from_be_bytes(length);
    if declared > maximum {
        return Err(AtomicBlobStoreError::BlobTooLarge {
            size: declared,
            maximum,
        });
    }
    if declared > usize_limit {
        return Err(AtomicBlobStoreError::InvalidPayloadLength { declared });
    }
    let payload_len = usize::try_from(declared)
        .ok()
        .filter(|size| size.checked_add(HEADER_LEN + CHECKSUM_LEN).is_some())
        .ok_or(AtomicBlobStoreError::InvalidPayloadLength { declared })?;

    let mut payload = vec![0; payload_len];
    read_section(reader, &mut payload, EnvelopeSection::Payload)?;
    let mut checksum = [0; 4];
    read_section(reader, &mut checksum, EnvelopeSection::Checksum)?;

    let mut trailing = [0; 1];
    match reader.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => return Err(AtomicBlobStoreError::TrailingData),
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::ReadEnvelope,
                source,
            });
        }
    }

    let mut actual = crc32c::crc32c(format.domain_tag());
    actual = crc32c::crc32c_append(actual, &format.envelope_version().to_be_bytes());
    actual = crc32c::crc32c_append(actual, &declared.to_be_bytes());
    actual = crc32c::crc32c_append(actual, &payload);
    let expected = u32::from_be_bytes(checksum);
    if expected != actual {
        return Err(AtomicBlobStoreError::ChecksumMismatch { expected, actual });
    }
    Ok(payload)
}

#[cfg(any(unix, windows))]
pub(crate) fn read_section(
    reader: &mut impl Read,
    bytes: &mut [u8],
    section: EnvelopeSection,
) -> Result<(), AtomicBlobStoreError> {
    match reader.read_exact(bytes) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Err(AtomicBlobStoreError::TruncatedEnvelope { section })
        }
        Err(source) => Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::ReadEnvelope,
            source,
        }),
    }
}
