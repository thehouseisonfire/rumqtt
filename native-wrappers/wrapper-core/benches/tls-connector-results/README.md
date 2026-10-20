# TODO49 Linux evidence

- `linux-2026-10-19-buffered-flush/`: corrected baseline after the buffered-stream
  fix. Each timing mode has 224 samples; each separate allocation mode has 32.
  Threaded hosting passes 0/16 gates and inline hosting passes 8/16.
- `linux-2026-10-19-profile/`: nine separate 100,000-message CPU/resource profiles,
  raw reports, validated workload logs, build-ID/symbol evidence and summary.
  These instrumented timings cannot establish the acceptance gate.

Historical evidence:

- `linux-2026-10-19/`: initial threaded and native-runtime comparisons, before
  correcting control-write serialization during a blocked lower-stream flush.
- `linux-2026-10-19-final/`: measurements after the control-write serialization fix. Each timing
  mode contains 224 samples: 16 configurations, native/external, seven repetitions.
  Each separate allocation mode contains 32 samples with instrumentation enabled.
- `validation-2026-10-19/`: full wrapper-core tests, focused default/native-only
  threaded/inline tests, strict Clippy and existing C ABI/package checks.

Each measurement directory contains raw test output, validated CSV summaries and
selected environment/source SHA-256 metadata. Source hashes identify the measured
Rust/C implementation and measurement scripts. The two earlier timing directories
and original validation logs predate the buffered-stream flush fix. Use the
corrected baseline above for the bridge including automatic lower flushes. The
only subsequent Rust change adds an ignored single-workload profiling entry point.
Timing samples exclude allocation/copy instrumentation. Allocation samples do not
establish a timing gate. Status 1 from the matrix runner means the delivery budget
was not met when the measurement processes themselves completed successfully.

The corrected scheduling variants fail the all-configurations gate. No application
consumer is confirmed; public ABI implementation and further redesign are parked.
See [method, results and limits](../tls-connector.md).
