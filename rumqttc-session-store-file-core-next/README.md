# rumqttc session file-store core

This protocol-neutral crate stores opaque `rumqttc-next` session checkpoints on
ordinary trusted local Unix filesystems. MQTT adapters own key encoding and the
canonical session codec; the event loop continues to own client and
configuration compatibility validation.

## On-disk format

Version 1 checkpoints use this stable envelope:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | ASCII `RUMQSESS` |
| 8 | 2 | envelope version `1`, big-endian `u16` |
| 10 | 8 | payload length, big-endian `u64` |
| 18 | N | opaque canonical session payload |
| 18 + N | 4 | big-endian CRC32C of every preceding byte |

CRC32C detects accidental corruption; it is not authentication or
cryptographic integrity. There is no encryption or tamper resistance. The
default maximum payload is 64 MiB. Loading is strictly bounded by the validated
declared length and reads only the fixed checksum plus one trailing-data probe.
Corruption, unsupported versions, oversized files, trailing bytes, and adapter
decoder failures fail closed without changing the file. Only absence of the
canonical `.session` path is a cache miss. Temporary files are ignored.

The store does not discover, promote, migrate, quarantine, or remove stale
dependency-owned temporary files. Ordinary drop-time cleanup performed by
`atomic-write-file` is dependency behavior and is not part of this store's
correctness contract.

Canonical filenames are the full 32-byte BLAKE3 hash of adapter-owned canonical
key bytes, rendered as 64 lowercase hexadecimal characters plus `.session`.
Raw key fields never become path components.

## Unix save, clear, and ordering

The resolved atomic-save dependency is `atomic-write-file` 0.3.0, with its
normal named same-directory temporary files. Every save uses
`mode(0o600)`, `preserve_mode(false)`, and `preserve_owner(false)`. The complete
open, write, and commit sequence runs in a store-owned Tokio blocking pool. On
successful
`commit()`, the audited dependency implementation has synchronized the
temporary file, atomically renamed it over the canonical destination, and
synchronized the namespace directory. The store deliberately does not repeat
that directory synchronization.

Clear removes only the canonical path. An already absent checkpoint succeeds;
after an actual removal, the project opens and synchronizes the namespace
directory before returning success. Namespace creation likewise synchronizes
the configured root.

One coordinator is shared by all clones and runs independently of caller-owned
Tokio runtimes. A store may be constructed on one runtime and used on another
after the construction runtime has been dropped. Its successful channel enqueue
is the submission linearization point. It maintains FIFO queues only for active
keys, runs at most one blocking filesystem operation per key, allows different
keys to progress concurrently, and removes idle queues. Dropping a caller's
future discards only the result: submitted work continues and later same-key
work cannot overtake it.

After process interruption, initial creation exposes either absence or the
complete new checkpoint; replacement exposes the complete old or complete new
checkpoint; clear exposes the complete old checkpoint or absence. Dependency
temporary files may remain. These are canonical-path process-interruption
invariants, not universal power-loss guarantees.

`atomic-write-file::commit()` is a composite operation. If it returns an error,
the canonical checkpoint may still contain the previous complete value or may
already contain the new complete value. Reload the canonical path to determine
observable state; the error cannot identify the failed internal stage.

## Trust and support boundary

The configured root must already exist and be a directory. Its ancestors and
the root are trusted and application-controlled. Hash-derived filenames prevent
session keys from constructing paths outside that root, but the crate does not
defend against another writer, hostile root access, symlink/directory
replacement, reparse points, or cross-process races. It provides no locking,
leases, fencing, or distributed/multi-writer coordination.

Coordination is process-local. Applications must ensure that only one process
and one active session owner writes a given key.

Project-owned behavior includes envelope construction, size enforcement, path
derivation, namespace setup, FIFO coordination, cancellation handling, blocking
isolation, error propagation, load, and clear. `atomic-write-file` owns exclusive
same-directory temporary creation, temporary-file synchronization, Unix rename,
containing-directory synchronization, and ordinary drop cleanup. Tests observe
the public commit boundary; they do not claim failpoint coverage inside private
dependency stages.

Non-Unix builds compile, but construction returns `UnsupportedPlatform`.
Windows persistence is not implemented. The store is intended for ordinary
local filesystems and makes no certification about filesystems, mounts,
controllers, caches, virtual disks, containers, or arbitrary power loss.
