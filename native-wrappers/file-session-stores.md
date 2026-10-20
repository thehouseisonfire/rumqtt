# Maintained C file session stores

Enable `file-session-store` in the C artifact and check
`RUMQTTC_CAP_FILE_SESSION_STORE`. The default artifact retains application storage
callbacks without this dependency. Both protocols are available when enabled,
using the maintained Rust adapter's separate `v4` and `v5` namespaces and formats.
Cargo feature unification can activate native owned-save support through core alone;
the C capability bit reports callable C support independently.
This persists protocol sessions, not an application outbox: queued requests that
have not entered native protocol state need their own application durability.

## Ownership and execution

Create one explicitly owned file-store context, begin opening a store, observe the
operation, then obtain a retained store handle. The root must be absolute, existing
and trusted, including its ancestors. Paths are copied: Unix encoding 1 uses native
bytes; Windows encoding 2 uses UTF-16 code units. No lossy conversion is performed.
Quarantine path-copy outputs use byte lengths/capacities, exclude a terminator, and
report their encoding. Result entry views borrow the operation that owns them.

Opening resolves the root on owned workers. Independent opens of the same root
and protocol fail `InUse`, including conflicting options. Retain an existing store
instead. Unix device/inode and Windows volume/file identity coordinate supported
aliases. Directory replacement, unusual filesystem aliases and other processes
are outside this authority; applications must exclude independent Rust/C writers.
Cross-open compatibility means sequential interoperability.

Attach using protocol-matching configuration, nonempty scope, timeout and a
checkpoint bound. Scope and client ID identify a key. The attachment bound counts
the native codec plus the eight-byte wrapper envelope. The store option counts
native codec bytes alone; disk inspection counts the actual file including atomic
framing. Attachments exceeding the store bound plus eight are rejected. Disk bytes
remain native Rust checkpoints; the wrapper envelope is never written as a second
file format.

The context owns blocking endpoint and codec workers. The existing atomic store
engine owns filesystem coordination and workers. Native owned-save dispatch moves
snapshots to tracked work; default Rust clients retain ordinary save dispatch.
Neither callback threads nor MQTT workers may perform nonzero blocking waits.
Nonblocking polling remains available.

## Limits and observations

Default context limits are two workers, 64 stores including initialization,
256 accepted operations, 256 distinct observations and 256 MiB of logical buffer
reservations. Store defaults are `16 MiB - 8` native checkpoint bytes, four engine
operations and 4096 administration report entries. Override limits explicitly;
zero limits are rejected. A retained observer shares its original reservation.

Admission bounds queued and active work independently of engine concurrency.
Checkpoint reservations use the owned snapshot footprint or worker-side file
metadata, rather than charging every small session as a maximum-sized checkpoint.
Decoding enforces the smaller of four times the configured serialized bound and
512 times the inspected/actual bytes plus 1 KiB for logical collection and byte
allocations. Collection lengths must also be justified by remaining input before
allocation. Streaming reads are bounded by the preflight size even if an excluded
external writer changes the file. Encoders stop before growing past their bound.
Reservations include all simultaneous copied keys/paths and remain charged while results are held.
Rust operation polling and waiting return `FileStoreValue<T>` handles that borrow the
result through `Deref` and retain its byte reservation independently of the operation.
These are logical storage bounds, excluding allocator, engine and thread overhead.
Worker counts, active engine operations and owned store counts are bounded separately.
Windows cleanup reserves report storage and rejects namespaces whose directory
entry count exceeds the report bound before invoking the engine's mutation.

Poll/wait states are 0 unavailable or rejected, 1 pending, 2 success, 3 terminal
failure. Results are repeatable. A wait deadline limits observation; releasing a
handle or timing out does not cancel accepted work or prove that a mutation did
not commit. An unsettled operation continues retaining its key, inputs and owners.
Typed file error evidence retains classifications, operation codes, portable I/O
kinds and optional signed target OS codes without source strings or checkpoint data.

A client's persistence timeout covers the entire load, including queued worker
decoding, under one deadline. Timeout does not cancel accepted codec work or
release its key exclusion before that work settles.

## Administration

Inspection reports absence/presence, physical size and optional modification time.
It neither validates a session nor guarantees a subsequent unchanged file.
Quarantine returns an owned identifier and native path. Operator clear removes
canonical local state explicitly. Missing quarantine sources have a distinct
`Absent` classification; absent inspection is a successful result.

Client admission and destructive administration acquire the same key authority
atomically. Clear/quarantine reject active clients and outstanding work, including
work that outlives a client timeout or shutdown. Explicit session recovery keeps
its existing managed transaction; operator clear cannot bypass it.

Stale cleanup uses namespace-wide maintenance exclusion, rejects active clients
or pending work, and preserves the engine's barrier. It handles owned Windows
staging files only. Unix cleanup returns `Unsupported`; no private Unix temporary
filename format is parsed. Cleanup results expose removed/skipped identifiers and
typed per-entry failures. Corrupt files are never automatically deleted.

## Close and unload

Close/destroy clients before requesting store close. Close rejects live client
attachments, closes admission synchronously, drains accepted work and joins engine
threads. Repeated close calls observe the same result. Retained configurations and
store handles cannot reopen that owner. Context shutdown rejects new work and
closes remaining stores, including initialization still in progress.

Join the context before unloading. Operation success, client destruction, engine
closure and context worker exit are separate boundaries. Failed opens still belong
to the context's teardown. Timed-out joins can be observed again. A teardown error
is not successful quiescence. Retained immutable results remain readable after join.
Last-owner release requests cleanup without blocking; callers needing an unload
boundary must retain and explicitly shut down/join the context.

The store provides no encryption, authentication, tamper resistance, cross-process
locking or universal hardware power-loss guarantee. Checkpoint bodies and source
error strings are not exposed through automatic logs.

## Packaging and verification

Release updated native client crates first, then the maintained file adapter,
then wrapper-core/C artifacts that enable this feature. Keep the independent
adapter manifest's client versions compatible with the wrapper graph when bumping
versions. No dependency override or sibling-repository change is required.

Run the `file-store-minimal` and `file-store-composed` installed package profiles,
the native C restart fixture and both workspaces' storage regressions. The
[checkpoint harness](wrapper-core/benches/file_store.md) reports conversion and
allocation cost separately from network throughput.
