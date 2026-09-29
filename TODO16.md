# C Binding Parity: Implementation and Validation

The requested fixtures, fixes, documentation, and CI changes are implemented.
Linux execution is recorded below. macOS and Windows execution remains pending
on their CI runners; CI configuration is not execution evidence.

## Completed implementation

- [x] **C-WC-05:** `native_auth_overlap` uses broker barriers and bounded polling
  to observe the overlap completing while the active request remains pending.
  It rejects missing/duplicate/reordered events through Failed, Disconnected,
  and reconnect authentication, checks operation IDs and stable typed results,
  verifies no extra AUTH reaches the broker, and counts exactly-once callbacks.
  Wrapper-core rejects an overlap without interrupting the active exchange;
  MQTT 5 preserves the ConnectionClosed lifecycle event before cleanup. Focused
  Rust regressions cover transport loss, preserved success, and later admission.
- [x] **C-WC-01/03/04:** `native_wire_options` replaces nonempty Will inputs for
  both protocols and MQTT 5 CONNECT properties, overwrites both sets of caller
  buffers, starts/restarts with exact replacement wire assertions, and clears
  the configuration. `native_runtime_batching` observes request/read batches at
  zero, one, and eight using admitted requests, a broker burst, and bounded
  event-queue backpressure. `native_runtime_limits` checks standalone reset/
  clear defaults through wire data and successful operations beyond old limits.
- [x] **C-WC-11:** `native_auth` verifies AUTH property names, values, order,
  duplicates, and present-empty values, retains nonempty string/binary views
  across client destruction, and validates public-helper copies after event
  destruction. `native_event_properties` does the same for nonempty broker
  DISCONNECT strings and compares PUBLISH/SUBSCRIBE/UNSUBSCRIBE outgoing IDs
  against broker-observed IDs. `event_contract` covers CONNACK scalar/string
  selectors, authentication/DISCONNECT optional outputs, wrong kinds, invalid
  selectors, NULL events, and initialized failure outputs, including rejected
  CONNACK property events.
- [x] **C-WC-12:** `native_network_options` starts after clearing bind/buffer
  overrides and disabling TCP_NODELAY, checks broker-observed ports and actual
  socket values, and compares equivalent reference sockets to account for OS
  buffer adjustments. Windows process-snapshot socket inspection is implemented.
- [x] **C-WC-13:** `native_tls_matrix` accepts a trusted platform-root broker
  and rejects an untrusted broker for both protocols and each enabled backend.
  `platform_trust.py` uses disposable macOS keychain/admin trust and Windows
  current-user Root stores, preserves cleanup intent before side effects,
  cleans up partial installation/child failure, protects preexisting trust,
  verifies restoration, and retains manifests for unconditional CI cleanup.
  Linux uses isolated certificate-file/directory inputs. Harness regressions
  simulate platform-store cleanup without changing the local host's stores.
- [x] **CI and evidence:** All seven package profiles retain platform/feature
  metadata, command logs, and CTest JUnit results. Linux/macOS/Windows jobs
  provision Mosquitto and require all four supported Will cases. ASan/UBSan,
  Valgrind, macOS leaks, and Windows ASan jobs include ownership, cancellation,
  and retained-event coverage as supported. CI retains platform-specific logs
  and always attempts outstanding trust cleanup. `CHANGELOG.md`, the C README,
  and `wrapper-core/PARITY.md` describe behavior and execution limits.

## Local validation (Linux x86_64)

- [x] Native C suite and examples: 46/46 passed.
- [x] Native C AddressSanitizer/UndefinedBehaviorSanitizer: 46/46 passed;
  the Rust library is not sanitizer-instrumented in this C harness run.
- [x] Valgrind ownership/cancellation/event-lifetime selection: 11/11 passed.
- [x] Harness cleanup and required-Will execution regressions: 11/11 passed.
- [x] Main and native-wrapper workspace checks; native-wrapper workspace tests;
  MQTT 5 crate tests; focused overlap/transport-loss regressions; wrapper-core
  native TLS/WebSocket/proxy combination tests and the explicit real-broker
  Rust Will test.
- [x] MQTT 5 feature matrix: all 19 cargo-hack configurations passed.
- [x] All seven installed package profiles: 49 consumer and 161 native
  transport/disabled-feature checks passed with zero skips; Rustls and native
  TLS platform trust passed for both protocols.
- [x] Current FFI/header and export checks.
- [x] Historical ABI resolver ran: no applicable published 0.1.0-alpha baseline.
  Historical compatibility remains **unverified**, not passed.
- [x] Both workspace format checks, Python Ruff checks/formatting, project-wide
  Pyrefly (zero errors), actionlint, standard wrapper-core Clippy with warnings
  denied, and source-diff checks.
  The additional pedantic/nursery Clippy run still reports existing errors in
  unchanged wrapper-core code; it is not recorded as passed.

Local logs and JUnit results are under `native-wrappers/target/`; per-profile
logs and metadata are under `target/c-feature-matrix/` in that workspace.

## Pending external execution

- [ ] Run the native suite, seven package profiles, installed static/shared
  CMake/pkg-config consumers, disabled-feature and TLS/proxy cases, required
  Mosquitto Will cases, socket inspection, and platform trust on macOS/Windows.
- [ ] Run supported macOS/Windows sanitizer/leak jobs and retain their evidence.
  Windows UBSan and Valgrind/macOS leaks are unsupported.
- [ ] Compare with an applicable published release ABI baseline when available.
  The historical comparator supports Linux x86_64 and macOS arm64; Windows
  current header/export/contract checks use its platform-specific scripts.

Required workspace/header/export commands remain:

```bash
cargo fmt --manifest-path native-wrappers/Cargo.toml --all --check
cargo check --manifest-path native-wrappers/Cargo.toml --workspace
cargo test --manifest-path native-wrappers/Cargo.toml --workspace
native-wrappers/c/tests/abi/check.sh ffi-header
native-wrappers/c/tests/abi/check.sh exports
native-wrappers/c/tests/abi/compare-release.sh
```
