# Optional C File-Backed Session Stores and Administration

Status: planned.

## Goal

Expose the maintained Rust file-store adapters and their operator tools to C,
so durable sessions do not require a new application storage implementation.

## Foundation and scope

`session-store-file/adapter` already provides v4/v5 `SessionFileStore`, inspection,
quarantine, operator clear, and stale-file cleanup. Wrapper-core's
`rust_session_store::{from_v4, from_v5}` adapts native stores to the managed
checkpoint contract. C currently exposes only application store callbacks.

Keep the independent file-store workspace and optional dependency boundary.
Reuse its on-disk format and atomic-blob-store implementation; a second C-only
checkpoint format, generic packet codec API, and editable session-state API are
outside this task.

## Implementation requirements

- [ ] Add an opt-in C Cargo/package feature and capability with an optional
  dependency on `rumqttc-session-store-file-next`. Keep default artifacts and
  wrapper-core's mandatory dependencies unchanged. Enabled profiles must support
  both explicit protocols with their separate namespaces.
- [ ] Add opaque retained store handles and owned open options for trusted root,
  checkpoint-size bounds, and concurrent-operation limits. Use explicit platform
  path encoding and return owned/copyable paths where needed; no implicit lossy
  conversion or automatic creation of untrusted directory trees.
- [ ] Wrap each native store through the existing Rust adapter once and reuse
  its identity for all attachments. Coordinate duplicate opens of the same root
  so separate C handles cannot bypass active-key leases; document path-alias and
  cross-process boundaries rather than claiming universal filesystem exclusion.
- [ ] Define attachment with protocol, scope, timeout, and wrapper checkpoint
  bounds. Audit native versus wrapper envelope size accounting, failed-start
  cleanup, redirect target-key leases, and explicit recovery. Preserve native
  serialization and namespace compatibility with existing Rust store consumers.
- [ ] Expose inspection as metadata, including absence, size, and optional
  modification time; existing inspection does not decode or validate a session.
  Expose quarantine, operator clear, and before-use stale cleanup with typed
  results. Do not expose checkpoint bodies or credentials through automatic logs.
- [ ] Serialize destructive administration against active same-key clients and
  pending store operations using shared lease authority. Reject an in-use key;
  do not bypass the managed recovery transaction to clear a live session. Stale
  cleanup must honor its existing global before-use barrier and platform scope.
- [ ] Run filesystem work on tracked suitable execution, with bounded admission
  and owned operation observations. Define deadlines as observation limits where
  filesystem work cannot be stopped; cancellation is not proof a mutation did
  not commit. No callback/runtime thread may block waiting for its own operation.
- [ ] Retain stores and auxiliary work through terminal completion and teardown;
  provide explicit quiescence observation needed for unload. Distinguish releasing
  a handle, finishing a mutation, and all filesystem work having stopped.
- [ ] Keep callback stores working unchanged. Define typed redacted file errors
  and additive ABI APIs, disabled-feature responses, and initialized outputs.

## Verification and completion

- [ ] Use native C process-restart fixtures to recover subscriptions, outgoing
  QoS 1/2 and incoming QoS 2 for both protocols. Cross-open Rust/C checkpoints.
- [ ] Verify atomic replacement interruption, wrong/corrupt/oversized checkpoints,
  absent inspection, quarantine/clear, cleanup barriers, duplicate opens, and
  active-key rejection. Cover failed starts, redirects, recovery, and shutdown.
- [ ] Test retained operations, timed observation followed by eventual mutation,
  bounded resources, root/path handling, and unload after auxiliary teardown.
- [ ] Update feature/package consumers, C examples, READMEs, `PARITY.md`, and root
  `CHANGELOG.md`. Adapter changes also require `session-store-file/CHANGELOG.md`.
  Run relevant tests in both workspaces, package profiles, and C ABI checks.

Complete when an optional C artifact can reuse the Rust durable store and safely
administer inactive keys without reimplementing persistence or weakening leases.
