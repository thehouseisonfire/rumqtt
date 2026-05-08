# TODO: Panic Audit

Review each `panic!`, `assert!`, `expect`, and `unwrap` in the workspace crates and decide
whether it is truly justified for library code.

Scope:

- `mqttbytes-core/`
- `rumqttc-core/`
- `rumqttc-v4/`
- `rumqttc-v5/`
- `rumqttc-next/`

For each panic site, classify it as one of:

- Internal invariant that cannot be triggered by public API misuse.
- Test-only or example-only code.
- Configuration or user input error that should become a typed `Result` error.
- Protocol decode/encode error that should use the crate's existing error types.
- Temporary compatibility behavior that needs a deprecation path.

Preferred follow-up:

- Replace recoverable panics with typed errors.
- Keep panics only for impossible internal invariants, and document why they are impossible.
- Prefer `debug_assert!` for development-only invariants where release behavior should not abort.
- Avoid `unwrap` / `expect` in library runtime paths unless the preceding code proves the value
  exists and the reason is obvious or documented.
- Add regression tests for any public API path converted from panic to `Result`.
