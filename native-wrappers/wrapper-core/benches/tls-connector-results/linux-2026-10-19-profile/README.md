# Bounded TODO49 CPU investigation

Nine cases cover MQTT 5/current-thread execution, native/threaded/inline TLS,
and single TLS/1 KiB, nested TLS/1 KiB, and nested TLS/16 KiB. Each case has two
separate 100,000-publication processes: CPU/resource counters and CPU sampling.
All workload processes completed their delivery assertions and cleanup.

- `environment.json`: commands, parameters, source hashes, corrected-baseline
  metadata and whole-process `getrusage` deltas from the counter runs.
- `*-stat*.txt`, `*-record-workload.txt`: raw counters and successful workload logs.
- `*-report.txt`: userspace CPU samples with self counts and inline/caller details.
- `*-callers.txt`: inclusive caller summary; percentages overlap and must not be summed.
- `*-buildids.txt`: recorded object build IDs, extracted from the original perf data.
- `libc-symbols.csv`: unresolved libc addresses mapped with matching debug symbols.
- `summary.csv`: CPU/resource observations and mutually exclusive flat sample labels.
- `analysis.json`: analysis-script hash, libc build ID and debug-file hash.
- `validation/`: correctness, lint and package checks after adding the ignored entry point.

Perf text reports retain their values and call details with trailing whitespace
removed for version control. The summaries can be regenerated from that text.

The regular release optimizer/LTO settings are retained, with debug information
enabled and symbol stripping disabled. Sampling uses user callchains explicitly
so it does not require kernel-callchain permission. Context switches come from
`getrusage`; user-only perf context-switch counters are not used. No kernel
security settings were changed.

Whole-process CPU includes user/system CPU, startup/teardown, the Rustls peer,
native MQTT client and the small perf parent. Samples contain userspace CPU only.
Flat labels are not isolated causal costs: optimized inline work can remain in
its caller. `summary.csv` does not apply the timing gate. The gate uses the
separate corrected uninstrumented matrix.

Libc symbols came from the system debuginfod client cache. Resolve the recorded
libc build ID using the host's debug package or debuginfod service, then regenerate
the summary from retained text without needing the binary perf recordings:

```sh
python3 native-wrappers/wrapper-core/benches/summarize_tls_profiles.py \
  native-wrappers/wrapper-core/benches/tls-connector-results/linux-2026-10-19-profile \
  --libc-debug /path/to/matching/libc/debuginfo
```

The summarizer verifies the nine-case matrix, workload parameters/success,
positive metrics, no lost samples and the libc build ID. With `--data DIRECTORY`,
it extracts/refreshes the build-ID text from original `perf.data` files. The
large binary recordings remain under `native-wrappers/target/` and are not
retained in Git or the published package. Full collection commands are in
`environment.json` and the [method document](../../tls-connector.md).
