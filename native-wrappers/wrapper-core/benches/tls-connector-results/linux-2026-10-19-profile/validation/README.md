# Investigation validation

Cargo commands run from the repository root with
`--locked --manifest-path native-wrappers/Cargo.toml`.

- `full-core-threaded.txt`: `cargo test -p rumqttc-wrapper-core-next --features tls-connector-proof`
  with threaded hosting: 424 passed, zero failures, four intentional ignores.
  The ignored cases are three manual measurement/profiling tests and the
  Mosquitto-dependent will test.
- `focused-default-inline.txt`: `cargo test -p rumqttc-wrapper-core-next --lib
  --features tls-connector-proof tls_connector_proof` with inline hosting:
  15 passed and two manual tests ignored.
- `focused-native-only-threaded.txt`, `focused-native-only-inline.txt`: the
  focused command with `--no-default-features`, under each hosting mode:
  15 passed and two ignored in each.
- `clippy.txt`: `cargo clippy -p rumqttc-wrapper-core-next --tests
  --features tls-connector-proof,readiness -- -D warnings -W clippy::pedantic
  -W clippy::nursery`; passed. Readiness avoids the unrelated feature-dependent
  runtime const suggestion documented in the earlier validation.
- `package-list.txt`: `cargo package -p rumqttc-wrapper-core-next --allow-dirty --list`;
  the private profile entry point and Python helpers are included, and raw
  `benches/tls-connector-results/` evidence is excluded.

All 18 CPU/resource workload processes completed successfully. The corrected
baseline runner validated 448 timing and 64 separate allocation samples; exit 1
means the unchanged performance gate failed, rather than a workload failure.
Reanalysis from retained text produces the same summary as original perf data.
Analysis checks reject incorrect scenarios, failed workloads, incomplete matrices
and lost CPU samples. Rust formatting, Python compilation, source-hash and
whitespace checks passed.
