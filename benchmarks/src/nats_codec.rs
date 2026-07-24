use anyhow::{Context, bail};
use bytes::{BufMut, Bytes, BytesMut};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Publish {
    pub(crate) subject: Bytes,
    pub(crate) payload: Bytes,
}

pub(crate) fn write_publish(
    subject: &str,
    payload: &[u8],
    output: &mut BytesMut,
) -> anyhow::Result<()> {
    validate_subject(subject)?;
    let mut length = itoa::Buffer::new();
    output.reserve(4 + subject.len() + length.format(payload.len()).len() + payload.len() + 4);
    output.put_slice(b"PUB ");
    output.put_slice(subject.as_bytes());
    output.put_u8(b' ');
    output.put_slice(length.format(payload.len()).as_bytes());
    output.put_slice(b"\r\n");
    output.put_slice(payload);
    output.put_slice(b"\r\n");
    Ok(())
}

pub(crate) fn read_publish(input: &mut BytesMut) -> anyhow::Result<Publish> {
    let header_end = input
        .windows(2)
        .position(|window| window == b"\r\n")
        .context("incomplete NATS PUB header")?;
    let header =
        std::str::from_utf8(&input[..header_end]).context("NATS PUB header is not UTF-8")?;
    let rest = header
        .strip_prefix("PUB ")
        .context("NATS frame is not a PUB operation")?;
    let (subject, payload_len) = rest
        .split_once(' ')
        .context("NATS PUB header must contain a subject and payload length")?;
    if payload_len.contains(' ') {
        bail!("NATS PUB header contains unexpected fields");
    }
    validate_subject(subject)?;
    let payload_len = payload_len
        .parse::<usize>()
        .context("invalid NATS PUB payload length")?;
    let payload_start = header_end
        .checked_add(2)
        .context("NATS PUB frame length overflow")?;
    let payload_end = payload_start
        .checked_add(payload_len)
        .context("NATS PUB frame length overflow")?;
    let frame_len = payload_end
        .checked_add(2)
        .context("NATS PUB frame length overflow")?;
    if input.len() < frame_len {
        bail!("incomplete NATS PUB payload");
    }
    if &input[payload_end..frame_len] != b"\r\n" {
        bail!("NATS PUB payload is not terminated by CRLF");
    }

    let subject_start = 4;
    let subject_end = subject_start + subject.len();
    let frame = input.split_to(frame_len).freeze();
    Ok(Publish {
        subject: frame.slice(subject_start..subject_end),
        payload: frame.slice(payload_start..payload_end),
    })
}

fn validate_subject(subject: &str) -> anyhow::Result<()> {
    if subject.is_empty()
        || subject
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'*' | b'>'))
    {
        bail!("NATS PUB subject must be non-empty and contain no whitespace or wildcards");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_roundtrip_preserves_subject_payload_and_following_frame() {
        let mut bytes = BytesMut::new();
        write_publish("bench.codec", b"hello", &mut bytes).unwrap();
        write_publish("next", b"", &mut bytes).unwrap();

        let publish = read_publish(&mut bytes).unwrap();

        assert_eq!(publish.subject, "bench.codec");
        assert_eq!(publish.payload, "hello");
        assert_eq!(read_publish(&mut bytes).unwrap().subject, "next");
        assert!(bytes.is_empty());
    }

    #[test]
    fn rejects_truncated_or_malformed_frames() {
        for frame in [
            &b"PUB subject 4\r\ndata\r"[..],
            &b"PUB subject nope\r\ndata\r\n"[..],
            &b"MSG subject 4\r\ndata\r\n"[..],
            &b"PUB bad.* 0\r\n\r\n"[..],
        ] {
            assert!(
                read_publish(&mut BytesMut::from(frame)).is_err(),
                "{frame:?}"
            );
        }
    }
}
