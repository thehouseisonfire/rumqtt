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
- Prefer `compare` for regressions. It alternates branch order by default and
  compares paired measured runs after warmups, reducing bias from temperature
  and background drift.
- Treat each scenario's `[quality]` table as part of the benchmark definition.
  Do not loosen gates after seeing a result; change them only when the scenario
  itself changes or experience shows the threshold is unrealistic.

## Interpreting Results

- Trust the scenario primary metric first. Secondary metrics help explain a
  result, but should not redefine success after the run.
- `improvement` and `regression` classifications use the configured metric
  direction. For latency scenarios, lower is better. For throughput and
  connection-rate scenarios, higher is better.
- Treat `inconclusive` as "run more or reduce noise", not as "no change". A
  result can be inconclusive because the confidence interval crosses zero or
  because its CI width exceeds the scenario quality gate.
- Check MAD and CV before trusting a median. High MAD or CV means the scenario
  is noisy even if the point estimate looks large.
- Keep the generated `summary.json` and `raw/` directory with any claim. The
  summary records scenario hash, command, git refs, rustc, OS, CPU count, and
  quality status; raw files keep the full parsed benchmark payloads.
