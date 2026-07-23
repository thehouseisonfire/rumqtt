# atomic-blob-store

`atomic-blob-store` saves one complete, bounded byte blob per opaque key on a
trusted local Unix or Windows filesystem. It supports atomic save, complete
load, metadata inspection, quarantine, clear, and explicit maintenance.

This crate is not a database, queue, object service, log, cache-coherence
protocol, or multi-process coordination mechanism.

## Contract

- Keys are arbitrary bytes. Canonical filenames contain the lowercase full
  BLAKE3 digest of the key followed by the configured safe suffix.
- Payloads are complete snapshots. Save owns one `Vec<u8>` and replacement
  rewrites the complete envelope; load returns one complete allocation.
- The default payload limit is 64 MiB. Length and allocation bounds are checked
  before payload allocation. Memory and I/O costs are linear in blob size.
- One owned coordinator and private runtime serve all clones. Same-key work is
  FIFO, different keys run concurrently up to the configured bound, and
  blocking filesystem work stays off caller async workers.
- Submission happens when an operation method is called. Dropping its future
  discards only the result. `flush` waits for all earlier submissions.
- Independent stores, even at the same path, do not coordinate with each other.
  When all handles are dropped, already submitted work drains before the owned
  runtime and coordinator exit.

Streaming was deliberately rejected for this API generation. A sound streaming
load cannot expose bytes before checksum and trailing-data validation, while a
streaming save must reconcile declared length, trailing input, cancellation,
and moving a reader into a `'static` coordinator job. Methods that secretly
buffer the whole value would be misleading. A future streaming API requires a
separate ownership and executor contract.

## Trust and durability

The configured root and its ancestors are trusted and application-controlled.
The crate does not defend against hostile path replacement, symlinks, reparse
points, another writer, network filesystems, or storage hardware that violates
filesystem synchronization semantics.

CRC32C detects accidental corruption only. There is no authentication,
encryption, tamper resistance, compare-and-swap, transaction support, locking,
lease, fencing, or cross-process guarantee.

Successful replacement provides an old-or-new complete canonical file under
process interruption. Successful clear provides an old-or-absent state.
Hardware power-loss behavior still depends on the operating system,
filesystem, device, controller, and mount configuration.

An atomic commit error is ambiguous: the old complete blob or new complete blob
may be canonical. Reload to determine the observable state. Corrupt,
wrong-domain, future-version, oversized, truncated, and trailing-data envelopes
fail closed and are not modified or automatically quarantined.

Unix uses `atomic-write-file` for same-directory replacement and synchronizes
directories after namespace creation and clear. Dependency-owned temporary
names are never parsed or cleaned by this crate. Windows uses exclusive
same-directory staging files and native write-through moves; its explicit
cleanup recognizes only names owned by the configured suffix and store format.

See [FORMAT.md](FORMAT.md) for the byte-level stable format and compatibility
policy.

## Test limitation

The test suite intentionally does not claim that concurrent processes or
independently opened stores are safe writers. That behavior is outside the
supported abstraction; applications must enforce a single active owner for a
root/namespace/key tuple.
