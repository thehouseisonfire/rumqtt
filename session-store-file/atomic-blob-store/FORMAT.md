# Envelope format and compatibility policy

## Version 1

For a payload of `N` bytes, the file is exactly:

| Offset | Size | Encoding |
| --- | ---: | --- |
| 0 | 8 | configured domain tag, verbatim |
| 8 | 2 | version `1`, big-endian `u16` |
| 10 | 8 | `N`, big-endian `u64` |
| 18 | N | payload bytes |
| 18 + N | 4 | big-endian CRC32C of bytes `0..18+N` |

EOF must immediately follow the checksum. A reader rejects a different domain,
an unlisted version, a length above its configured bound, truncation, checksum
failure, and trailing bytes.

## Paths and owned names

The canonical filename is the 32-byte BLAKE3 digest of the opaque key rendered
as 64 lowercase hexadecimal characters, followed verbatim by the configured
suffix. The domain tag does not enter hashing. Namespace and suffix changes are
path migrations, while the domain tag prevents a colliding envelope from being
accepted silently.

The namespace is exactly one non-empty normal path component. A suffix is 2–32
ASCII bytes matching `\.[a-z0-9_-]+`.

Project-owned diagnostic names append `.quarantine-v1.` and a random
64-character lowercase hexadecimal identifier. Windows staging names append
`.tmp-v1.save.` or `.tmp-v1.clear.` and the same identifier. Unix writer
temporary names belong to the atomic-write dependency and are not parsed.

## Stability policy

1. Readers accept every version explicitly listed in this document.
2. Writers emit version 1 and never rewrite data merely by opening or reading.
3. Future versions fail closed without mutation or automatic quarantine.
4. Corruption is never treated as absence.
5. Breaking envelopes require a new version and an explicit migration API or
   external tool.
6. Hash, suffix, namespace, domain, or key-byte changes are path migrations.
7. Adding opt-in read support is minor-compatible. Changing the version emitted
   for an existing identity requires an explicit opt-in or a major release.

Golden fixtures are immutable compatibility inputs. Tests must not regenerate
them from the production encoder.
