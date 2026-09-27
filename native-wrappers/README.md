# rumqttc native wrappers

This independent workspace contains the host-neutral wrapper infrastructure and
native APIs for the MQTT 3.1.1 and MQTT 5 clients:

- [`wrapper-core`](wrapper-core/README.md) owns the protocol-neutral native
  client driver and wrapper-facing Rust API.
- [`c`](c/README.md) exposes that API as a versioned C ABI and packages shared
  and static native libraries.
- [`js`](js/README.md) exposes one Node-API JavaScript/TypeScript package for
  Node.js, local Deno, and Bun.
- [`python`](python/README.md) exposes one typed `asyncio` package for CPython
  through a private PyO3 extension.

The two crates share one workspace because the C API directly adapts wrapper
core and changes to their command, event, completion, and lifecycle contracts
must remain coordinated. The workspace stays in this repository because
wrapper core tracks both client implementations closely.

From the repository root, run:

```bash
cargo test --manifest-path native-wrappers/Cargo.toml --workspace
```

Wrapper-specific build, packaging, and runtime checks are documented in each
crate's README.

## Session Present compatibility

The MQTT 3.1.1 default rejects a successful clean-session CONNACK with invalid
Session Present=true. The opt-in `AcceptAsClean` policy resolves only this case
as fresh, resets old local protocol state and replay work, and clears the
current scope/client-ID checkpoint before reporting success. Failed or cancelled
clearing remains pending for retry before checkpoint loading. The broker remains
non-conforming and raw connected-event/connection-result flags remain true.

Existing diagnostics expose an optional `connack` observation containing raw
Session Present, effective session resume, and a compatibility reason. Use the
effective session for resubscription decisions. No MQTT 5 clean-start recovery
is introduced; the existing broker-only resume policy retains its restrictions.

Configure `V4Config::session_present_mismatch_policy` with
`SessionPresentMismatchPolicy::{Error, AcceptAsClean}`. `Error` is the default.
`DiagnosticsSnapshot::connack` includes `raw_session_present`, `session_resumed`,
and `ConnAckDiagnostic`. Existing `V5Config::broker_session_resume_policy`
remains flat and maps into the native client's `ProtocolCompatibility`.
The added defaulted Rust fields require updates to exhaustive struct literals;
wrapper-core is private infrastructure and has no stable Rust API promise.
