# rumqttc session file-store core

This protocol-neutral crate stores opaque `rumqttc-next` session checkpoints on
ordinary trusted local Unix and Windows filesystems. MQTT adapters own key encoding and the
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

Inspection is metadata-only. Quarantine uses a randomized same-directory
diagnostic name without overwriting. Exact legacy detection checks only the
adapter-supplied filename directly below the root and never scans or migrates.
If the quarantine rename succeeds but synchronizing the namespace fails, the
error includes the committed `QuarantineInfo`, so its diagnostic path is not
lost. The canonical checkpoint has already been moved in that case.
If that generated component is unrepresentable on the filesystem, the legacy
candidate is necessarily absent and canonical loading continues normally.

Canonical filenames are the full 32-byte BLAKE3 hash of adapter-owned canonical
key bytes, rendered as 64 lowercase hexadecimal characters plus `.session`.
Raw key fields never become path components.

## Platform save, clear, and ordering

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

On Windows, `windows-sys` 0.61 uses lossless extended-length wide paths.
`CreateFileW` creates exclusive same-directory staging files with no sharing,
inherited directory ACLs, and write-through; `FlushFileBuffers` precedes
`MoveFileExW`. Replacement is enabled only after an exists error. Clear first
makes the canonical last-write time current and flushes it, then moves canonical
state to a unique owned name and calls `DeleteFileW`. Cleanup age therefore
measures when clear began rather than when the checkpoint payload was saved.
`ReplaceFileW` is deliberately not used. Windows owned-staging cleanup runs
behind a cancellation-safe store-wide FIFO barrier. Unix cleanup returns
`CleanupUnsupported` and does not parse `atomic-write-file` private names.

## Performance characteristics

Configuring and using a file-backed session store adds an awaited durability
barrier whenever the MQTT event loop checkpoints protocol state. Clients that
do not configure persistent storage do not pay this cost.

In one local steady-state characterization with a fast local SSD and a
loopback broker, single-inflight workloads produced these results:

| Workload | Persistence disabled | Persistence enabled | Observed impact |
| --- | ---: | ---: | ---: |
| MQTT 3.1.1 QoS 1 | about 32,500 messages/s | about 11,900 messages/s | about 63% lower throughput |
| MQTT 5 QoS 2 | about 19,400 messages/s | about 7,300 messages/s | about 62% lower throughput |

Individual durable-save barriers were commonly tens of microseconds on that
system. The larger client-level effect came primarily from save frequency: the
measured QoS 1 workload made approximately three full-checkpoint saves per
publish, while QoS 2 made approximately four. Checkpoints are full snapshots,
not deltas, so serialization, bytes submitted, and durable-write cost grow with
payload size and inflight state. A fixture containing 1,000 inflight publishes
with 1 KiB payloads produced a checkpoint of about 1.05 MiB.

Production saves avoid constructing a redundant complete envelope buffer by
writing the stable header, canonical payload, and checksum directly to the
atomic writer. For a 1 MiB replacement this improved the measured median from
about 324 to 311 microseconds, while leaving synchronization, atomic
replacement, checksum validation, ordering, and cancellation behavior intact.

These measurements characterize one system; they are not performance
guarantees. Synchronization latency varies substantially with the operating
system, filesystem, storage device, and workload. Applications with high
message rates, large inflight sessions, or storage constraints should measure
representative traffic on their deployment hardware. This store protects MQTT
protocol recovery state and is not a replacement for an application outbox or
durable business-message queue. See the
[benchmark methodology](../benchmarks/PERSISTENCE.md) and
[recorded results](../benchmarks/PERSISTENCE-RESULTS.md) for details.

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

Other builds compile, but construction returns `UnsupportedPlatform`. The store
is intended for ordinary local filesystems and makes no certification about
filesystems, mounts, controllers, caches, virtual disks, containers, or
arbitrary power loss.
