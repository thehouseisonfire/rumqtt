# TODO49 Linux evidence

- `linux-2026-10-19/`: initial threaded and native-runtime comparisons, before
  correcting control-write serialization during a blocked lower-stream flush.
- `linux-2026-10-19-final/`: measurements after the control-write serialization fix. Each timing
  mode contains 224 samples: 16 configurations, native/external, seven repetitions.
  Each separate allocation mode contains 32 samples with instrumentation enabled.
- `validation-2026-10-19/`: full wrapper-core tests, focused default/native-only
  threaded/inline tests, strict Clippy and existing C ABI/package checks.

Each measurement directory contains raw test output, validated CSV summaries and
selected environment/source SHA-256 metadata. Source hashes identify the measured
Rust/C implementation and measurement scripts. These results and validation logs
predate the buffered-stream flush fix; they do not measure its automatic lower
flushes. Rerun the matrix before assessing the current implementation's overhead.
Timing samples exclude allocation/copy instrumentation. Allocation samples do not
establish a timing gate. Status 1 from the matrix runner means the delivery budget
was not met when the measurement processes themselves completed successfully.

Both final scheduling variants fail the all-configurations gate: threaded passes
0/16, native-runtime scheduling passes 8/16. Public ABI implementation is pending.
See [method, results and limits](../tls-connector.md).
