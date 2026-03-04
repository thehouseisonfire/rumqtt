# Repository Guidelines

## Project Structure & Module Organization
This repo is a Rust workspace. Main members are:
- `rumqttc-v4/`: MQTT 3.1.1 client crate.
- `rumqttc-v5/`: MQTT 5 client crate.
- `benchmarks/` and `benchmarks/simplerouter/`: performance tooling and benchmark helpers.
- `docs/`: design notes and contributor conduct docs.
- `utils/mqttverifier/`: Node.js scripts to generating bytes for protocol verification.

Core library code is under `rumqttc-v4/src/` and `rumqttc-v5/src/`. Protocol codecs live under each crate's `mqttbytes` module. Integration tests live in each crate's `tests/`, and runnable examples are in each crate's `examples/`.

## Spec Compliance References
- For MQTT spec-compliance tasks, agents should consult `docs/spec/` first.
- Primary documents are `docs/spec/mqtt-v3.1.1.md` and `docs/spec/mqtt-v5.0.md`.
- Machine-readable requirement indexes are `docs/spec/mqtt-v3.1.1.requirements.json` and `docs/spec/mqtt-v5.0.requirements.json`.
- Regeneration workflow is documented in `docs/spec/README.md` and implemented in `docs/spec/generate_mqtt_specs.py`.

## Build, Test, and Development Commands
- `cargo check --workspace`: fast compile check across all workspace crates.
- `cargo test -p rumqttc-v4`: run MQTT 3.1.1 crate tests.
- `cargo test -p rumqttc-v5`: run MQTT 5 crate tests.
- `cargo test -p rumqttc-v4 --test reliability -- --nocapture`: run v4 reliability integration tests with logs.
- `cargo fmt --all`: format Rust code.
- `cargo hack --each-feature --exclude-all-features --optional-deps url test -p rumqttc-v4 -p rumqttc-v5`: CI-style feature matrix test (requires `cargo-hack`).
- `cargo hack clippy --each-feature --exclude-all-features --no-dev-deps --optional-deps url -p rumqttc-v4 -p rumqttc-v5`: lint parity with pre-commit/CI.

## Coding Style & Naming Conventions
Rust edition is `2024` (workspace-level). Follow `.editorconfig`: LF endings, spaces (4), trimmed trailing whitespace, and 120-char max line length for general files. Prefer idiomatic Rust naming:
- `snake_case` for modules, functions, and test names.
- `PascalCase` for structs/enums/traits.
- `SCREAMING_SNAKE_CASE` for constants.

Keep protocol behavior changes consistent between MQTT v4 and v5 paths when applicable.

## Testing Guidelines
Write integration tests in the relevant crate `tests/` directory with behavior-focused names (for example, `reconnection_resumes_from_the_previous_state`). Prefer targeted runs while iterating, then run full crate tests before opening a PR. If feature-sensitive code changes, run the `cargo hack` matrix command used in CI.

## Commit & Pull Request Guidelines
Use squash-friendly, conventional-style commit messages as described in `CONTRIBUTING.md`: `<tag>(<component>): <title>` with a clear body. Common tags include `fix`, `feat`, `docs`, `refactor`, `perf`, and `test`. PRs should:
- Explain what changed and why.
- Reference related issues when available.
- Include test evidence (commands run and results).
- Update `CHANGELOG.md` for user-facing changes.
