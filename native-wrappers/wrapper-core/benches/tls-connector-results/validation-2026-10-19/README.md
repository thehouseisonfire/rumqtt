# Validation

All commands ran on Linux on 2026-10-19. Cargo commands use
`--locked --manifest-path native-wrappers/Cargo.toml` from the repository root.

- `full-core.txt`: `cargo test -p rumqttc-wrapper-core-next --features tls-connector-proof`;
  421 passed, zero failures and three intentional ignores across unit, integration
  and documentation tests. The ignored cases are two manual release measurements
  and the Mosquitto-dependent will test.
- `inline.txt`: the 12 focused `--lib tls_connector_proof` tests with
  `RUMQTTC_TLS_PROOF_HOST_MODE=inline` and default features; all passed.
- `native-only-threaded.txt`, `native-only-inline.txt`: the same focused tests
  with `--no-default-features --features tls-connector-proof`; all passed in both modes.
- `clippy.txt`: `cargo clippy -p rumqttc-wrapper-core-next --tests
  --features tls-connector-proof,readiness -- -D warnings -W clippy::pedantic
  -W clippy::nursery`; passed. Readiness avoids an existing unrelated
  feature-dependent runtime const suggestion.
- `existing-c-abi.txt`: `native-wrappers/c/tests/abi/check.sh all`; passed, including
  generated-header/export parity, C/C++ consumers and relocated static pkg-config
  consumption. No new external-TLS C API is claimed by this check.
- `package-list.txt`: `cargo package --locked --allow-dirty --list
  -p rumqttc-wrapper-core-next`; the private Rust/C fixture and measurement scripts
  are included, while raw `benches/tls-connector-results/` evidence is excluded.

Rust formatting and whitespace checks passed. The evidence summarizer accepted
both final timing/allocation matrices and rejected incomplete, duplicate and
failed-process evidence. Temporarily removing the lower-writer serialization
guard made the forced key-update/blocked-flush regression fail; restoring it passed.
