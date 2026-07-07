# Benchmarking Discipline

Benchmark results are only useful when the machine, broker, and run shape are
controlled. Treat casual laptop runs as smoke tests, not performance evidence.

## Machine Setup

- Run on an otherwise idle machine. Avoid browsers, IDE indexing, background
  builds, package updates, and container workloads during measurements.
- Prefer a fixed CPU frequency governor such as `performance`. Record the
  governor used with the result if the machine is shared.
- Avoid thermally constrained machines. Discard results if CPU frequency drops
  during the run.
- Pinning benchmark and broker processes to isolated CPUs can reduce variance
  for serious comparisons, but only compare runs that use the same pinning.

## Build Profile

- Use the runner default release profile for benchmark data:
  `python3 benchmarks/runner.py run --scenario ...`.
- Use `--cargo-profile dev` only for harness debugging. Debug builds are not
  valid performance measurements.
- Compare branches using the same Rust toolchain, target, feature set, and
  benchmark scenario file.

## Broker Placement

- Codec scenarios do not require a broker and are the least noisy.
- Client scenarios require an external broker passed with `--broker-url`.
- For client throughput and latency, prefer a broker on the same isolated host
  or on a quiet, directly connected host. Do not compare local-broker results
  with remote-broker results.
- Keep broker configuration, persistence settings, TLS settings, and OS network
  tuning identical across compared runs.

## Run Shape

- Use warmup runs. The runner defaults are suitable for smoke-level checks, but
  serious comparisons should use enough measured runs for stable confidence
  intervals.
- Keep `--runs`, `--warmup-runs`, scenario arguments, broker URL, and cargo
  profile identical across baseline and target.
- Prefer `compare` for regressions because it alternates branch order by
  default, reducing bias from temperature and background drift.

## Interpreting Results

- Trust the scenario primary metric first. Secondary metrics help explain a
  result, but should not redefine success after the run.
- `improvement` and `regression` classifications use the configured metric
  direction. For latency scenarios, lower is better. For throughput and
  connection-rate scenarios, higher is better.
- Treat `inconclusive` as "run more or reduce noise", not as "no change".
- Keep the generated `summary.json` with any claim; it records the scenario,
  command, raw run payloads, git refs, and environment fields emitted by the
  benchmark binary.
