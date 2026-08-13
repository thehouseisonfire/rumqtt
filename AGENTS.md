# Repository Guidelines

## Project Structure & Module Organization
This repo contains three Rust workspaces. Main client-workspace members are:
- `mqttbytes-core/`: protocol-neutral MQTT codec primitives.
- `mqttbytes-v4/`: standalone MQTT 3.1.1 packet codec crate.
- `mqttbytes-v5/`: standalone MQTT 5 packet codec crate.
- `rumqttc-v4/`: MQTT 3.1.1 client crate.
- `rumqttc-v5/`: MQTT 5 client crate.
- `benchmarks/`: maintained client, codec, and options performance harness.
- `docs/`: design notes and contributor conduct docs.

The independent `native-wrappers/` workspace contains the protocol-neutral
wrapper core and C API. Run its Cargo commands with
`--manifest-path native-wrappers/Cargo.toml`.

The independent `session-store-file/` workspace contains the optional MQTT
v4/v5 file-store adapters and MQTT persistence benchmarks. Its protocol-neutral
`atomic-blob-store` dependency is maintained in a separate repository. Run its Cargo commands with
`--manifest-path session-store-file/Cargo.toml`.

Client library code is under `rumqttc-v4/src/` and `rumqttc-v5/src/`. Protocol
codecs live under `mqttbytes-v4/src/` and `mqttbytes-v5/src/`. Integration tests
live in each client crate's `tests/`, and runnable examples are in each client
crate's `examples/`.

The explicit MQTT version crates are published as Cargo packages `rumqttc-v4-next` and `rumqttc-v5-next`
because the un-suffixed package names are not owned in crates.io. Their library target is still named
`rumqttc`, so users can import them with clean Rust paths such as `use rumqttc::MqttOptions;`. Use the
`*-next` package names in Cargo commands (`-p`, dependency declarations, and package-qualified features).

## Spec Compliance References
- For MQTT spec-compliance tasks, agents should consult `docs/spec/` first.
- Primary documents are `docs/spec/mqtt-v3.1.1.md` and `docs/spec/mqtt-v5.0.md`.
- Machine-readable requirement indexes are `docs/spec/mqtt-v3.1.1.requirements.json` and `docs/spec/mqtt-v5.0.requirements.json`.

## Build, Test, and Development Commands
- `cargo check --workspace`: fast compile check across all workspace crates.
- `cargo check --manifest-path native-wrappers/Cargo.toml --workspace`: check the native wrapper crates.
- `cargo check --manifest-path session-store-file/Cargo.toml --workspace`: check all optional file-store crates.
- `cargo test -p rumqttc-v4-next`: run MQTT 3.1.1 crate tests.
- `cargo test -p rumqttc-v5-next`: run MQTT 5 crate tests.
- `cargo test -p rumqttc-v4-next --test reliability -- --nocapture`: run v4 reliability integration tests with logs.
- `cargo fmt --all`: format Rust code.
- `cargo fmt --manifest-path native-wrappers/Cargo.toml --all`: format the native wrapper workspace.
- `cargo fmt --manifest-path session-store-file/Cargo.toml --all`: format the file-store workspace.
- `cargo hack --each-feature --exclude-all-features test -p rumqttc-v4-next -p rumqttc-v5-next`: CI-style feature matrix test (requires `cargo-hack`).
- `cargo hack clippy --each-feature --exclude-all-features --no-dev-deps -p rumqttc-v4-next -p rumqttc-v5-next`: lint parity with pre-commit/CI.

## Coding Style & Naming Conventions
Rust edition is `2024` (workspace-level). Follow `.editorconfig`: LF endings, spaces (4), trimmed trailing whitespace, and 120-char max line length for general files. Prefer idiomatic Rust naming:
- `snake_case` for modules, functions, and test names.
- `PascalCase` for structs/enums/traits.
- `SCREAMING_SNAKE_CASE` for constants.

Keep protocol behavior changes consistent between MQTT v4 and v5 paths when applicable.

## Documentation
User-facing client and native-wrapper changes must be documented in
`CHANGELOG.md`; file-store changes belong in `session-store-file/CHANGELOG.md`.
If they affect examples or recipes, update those as well.

## Testing Guidelines
Write integration tests in the relevant crate `tests/` directory with behavior-focused names (for example, `reconnection_resumes_from_the_previous_state`). Prefer targeted runs while iterating, then run full crate tests before opening a PR. If feature-sensitive code changes, run the `cargo hack` matrix command used in CI.

## Commit & Pull Request Guidelines
Use squash-friendly, conventional-style commit messages as described in `CONTRIBUTING.md`: `<tag>(<component>): <title>` with a clear body. Common tags include `fix`, `feat`, `docs`, `refactor`, `perf`, and `test`. PRs should:
- Explain what changed and why.
- Reference related issues when available.
- Include test evidence (commands run and results).
- Update `CHANGELOG.md` for user-facing changes.
