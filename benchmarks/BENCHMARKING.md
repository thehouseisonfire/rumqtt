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
- For reproducible broker-backed smoke validation, prefer
  `python3 benchmarks/broker_fixture.py validate`. It starts a Docker
  Mosquitto with TCP, TLS, and websocket listeners, runs scenarios through the
  normal runner, writes `broker-validation-summary.json`, and removes the
  broker afterward.
- For client throughput and latency, prefer a broker on the same isolated host
  or on a quiet, directly connected host. Do not compare local-broker results
  with remote-broker results.
- Keep broker configuration, persistence settings, TLS settings, and OS network
  tuning identical across compared runs.
- Match the scenario transport to the broker URL: `mqtt://` for TCP,
  `mqtts://` or `ssl://` for TLS, and `ws://` for websocket. Do not compare
  TCP, TLS, and websocket runs as if they measure the same path.
- TLS and websocket scenarios require a broker configured for those transports.
  Record certificate handling, websocket path, and broker listener settings
  with the result.
- Local Mosquitto binaries may be built without websocket support. The fixture
  defaults to Docker for that reason; use `--backend system` only when TCP/TLS
  coverage is enough or after verifying the binary accepts `protocol
  websockets`.

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
- Feature-gated scenarios declare `cargo_features` in TOML. Keep the same
  feature set across compared branches.
- Soak scenarios intentionally run longer than normal throughput scenarios.
  Run them on controlled hardware and avoid mixing soak results with short
  throughput runs.

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
- Throughput scenarios also emit per-second samples and stability metrics.
  `throughput_collapse_pct` compares the first-half and second-half sample
  medians; lower is better.
- On Linux, throughput runs emit RSS diagnostics. `rss_growth_bytes` is a
  process-level signal for local harness/client memory growth, not broker
  memory. For broker memory, record broker-side telemetry separately.
- Keep the generated `summary.json` and `raw/` directory with any claim. The
  summary records scenario hash, command, git refs, rustc, OS, CPU count, and
  quality status; raw files keep the full parsed benchmark payloads.
