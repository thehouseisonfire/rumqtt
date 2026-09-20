# Native Wrapper-Core Parity Work — Complete

No remaining wrapper-core work is tracked here. WC-02 through WC-10, WC-12,
and WC-13 are covered by the named regression tests in
[`PARITY.md`](native-wrappers/wrapper-core/PARITY.md). User-visible fixes are
recorded in [`CHANGELOG.md`](CHANGELOG.md).

Coverage includes mixed durable recovery, callback cancellation and ownership,
runtime limits, alias reconnects, enhanced authentication and SCRAM redaction,
redirect/SRV policy and store isolation, proxy/TLS/WebSocket composition,
Unix sockets, socket controls, and concurrent close behavior.

The wrapper each-feature matrix, both TLS backends, native-TLS-only builds,
and independent tracing and log-fallback configurations run in
[wrapper CI](.github/workflows/native-wrappers-ci.yml). Platform-specific tests
are gated to supported targets; deterministic platform-root fixtures run on
Linux. Native seeded SRV tests cover weighted selection alongside wrapper
broker-backed priority and endpoint tests. The native defensive alias-replay
failure is covered together with its wrapper terminal-error mapping.

`TODO16.md` retains the corresponding C ABI work; this completion does not
change that scope.
