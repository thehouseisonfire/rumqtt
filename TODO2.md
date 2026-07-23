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

Provide a concise report, and recommendations for each instance.
