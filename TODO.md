# Matched Benchmark Hardening TODO

Implement every unchecked item in this document. This is a strict engineering
task, not a performance-results task. Preserve unrelated working-tree changes,
follow `AGENTS.md`, and keep the two matched client adapters semantically
equivalent.

## Scope and invariants

- Work in the main workspace and keep changes under `benchmarks/` unless a
  root-level changelog or CI update is required.
- Use the existing `rumqtt-library-bench` schema-v2 output and
  `compare-libraries` workflow. Extend them compatibly where practical.
- Keep `rumqttc-v5-next` and the exact locked `mqtt5` dependency on identical
  MQTT protocol settings and identical workloads.
- Continue using MQTT 5 Correlation Data for message identity and timestamps.
- Preserve adapter-observation-time latency and in-window throughput
  accounting.
- A failed correctness condition must invalidate a run; it must never be
  silently converted into a valid performance sample.
- Every potentially blocking fault path must have an explicit timeout and
  terminate cleanly.
- Do not commit generated benchmark results.
- Update `CHANGELOG.md` for user-visible harness changes and update benchmark
  documentation where commands, metrics, or report fields change.

## 1. Deterministic fault and accounting contract tests

- [ ] Add narrowly scoped, deterministic fault controls to the synthetic MQTT
  router. Support exactly these independently selectable behaviors:
  - drop one matching delivery;
  - duplicate one matching delivery;
  - delay one matching delivery until after the measurement deadline;
  - reject one QoS 1 publish with an appropriate negative acknowledgement;
  - disconnect a client while a publish is outstanding;
  - withhold a completion/delivery long enough to force incomplete drain or
    operation timeout.
- [ ] Make each fault deterministic by explicit trigger criteria such as packet
  sequence, packet identifier, client role, or occurrence count. Do not use
  random fault selection.
- [ ] Keep the normal synthetic-router path unchanged when no fault is
  configured.
- [ ] Add adapter contract tests that run each relevant fault against both
  `rumqttc` and `mqtt5`.
- [ ] Assert the exact observable outcome of every fault:
  - drop increments loss or causes incomplete drain and invalidates the run;
  - duplicate increments duplicates and invalidates the run;
  - post-deadline delivery is excluded from timed throughput and reported as a
    drain delivery;
  - rejection increments rejected/failed publication accounting and
    invalidates the run;
  - disconnect and withheld completion terminate within the configured bound,
    report a classified error/timeout, and invalidate the run.
- [ ] Verify fault tests never hang and leave no router, broker, or benchmark
  subprocess running.
- [ ] Add the smallest useful CI smoke coverage for the fault contract. Keep CI
  deterministic and short.

Acceptance criteria:

- Every listed fault has a behavior-focused automated test for both adapters
  where both APIs can observe the condition.
- All invalid cases are excluded from valid comparison pairs.
- Normal-path synthetic smoke tests continue to pass.
- No general-purpose fault scripting language or randomized chaos framework is
  introduced.

## 2. Compact representative matched scenario set

- [ ] Expand the maintained `matched` scenarios without generating a full
  Cartesian matrix.
- [ ] Add throughput scenarios covering:
  - QoS 0 and QoS 1;
  - 64-byte, 1-KiB, and 16-KiB payloads;
  - the existing 1-publisher/1-subscriber baseline;
  - one fan-in topology and one fan-out topology.
- [ ] Add latency scenarios covering:
  - QoS 0 and QoS 1;
  - one low offered rate intended to remain comfortably unsaturated;
  - one higher fixed offered rate intended to expose load sensitivity.
- [ ] Add connection scenarios covering:
  - serial concurrency;
  - one moderate concurrent connection level.
- [ ] Add one maintained matched TLS smoke scenario using the existing private
  CA flow.
- [ ] Give every scenario a unique topic, explicit transport, complete quality
  gates, fixed duration/warmup/drain values, and an unambiguous primary metric.
- [ ] Ensure catalog validation and broker-fixture selection recognize every
  new scenario.
- [ ] Document the intended purpose of each scenario family and state that the
  set is representative rather than exhaustive.

Acceptance criteria:

- The maintained set covers all dimensions above with the fewest sensible
  scenario files.
- Scenario files are reviewed data, not dynamically generated output.
- No automatic Cartesian-product matrix language or matrix orchestrator is
  added.
- CI runs only a minimal smoke subset, not the complete performance suite.

## 3. Exact broker configuration preservation

- [ ] Persist the exact effective Mosquitto configuration used by each broker
  fixture run inside its output directory before temporary fixture files are
  removed.
- [ ] Record the persisted configuration's relative path and SHA-256 digest in
  `broker-validation-summary.json`.
- [ ] Record all effective EMQX fixture environment overrides and listener
  settings in the same summary, in stable sorted form.
- [ ] Continue recording the broker image tag and locally resolved image
  digest.
- [ ] Record active transports, bound host/ports, persistence setting,
  anonymous/authentication setting, and TLS certificate mode in normalized
  broker metadata.
- [ ] Do not record private keys, credentials, tokens, or other secrets.
- [ ] Add tests proving that persisted metadata matches the configuration
  actually passed to the broker process/container.

Acceptance criteria:

- A retained broker-validation directory is sufficient to reconstruct the
  broker's relevant settings without access to the deleted temporary
  directory.
- Mosquitto and EMQX summaries use a common top-level broker metadata shape
  where concepts overlap.
- Configuration serialization and hashing are deterministic.

## 4. Connection-churn correctness and diagnostics

- [ ] Replace the aggregate connection failure counter with stable,
  adapter-neutral result classes:
  - successful connect and graceful disconnect;
  - connect timeout;
  - connect failure;
  - disconnect timeout;
  - disconnect failure.
- [ ] Apply explicit connect and disconnect timeouts equally to both adapters.
- [ ] Count a cycle as successful only after CONNACK and the configured
  disconnect-completion condition.
- [ ] Stop starting new cycles at the measurement deadline. Classify and bound
  cycles already in progress.
- [ ] Report attempts, successful cycles, every failure class, cycles in flight
  at the deadline, elapsed time, and successful connections per second.
- [ ] Invalidate a connection run when:
  - it records any failure class;
  - it completes zero successful cycles;
  - an in-progress cycle exceeds its timeout or cannot be drained.
- [ ] Add deterministic tests for success, refused connection, connect timeout,
  disconnect failure/timeout where representable, and zero-success
  invalidation.

Acceptance criteria:

- Both adapters expose the same public metric names and failure classes.
- Error strings may remain in raw output, but comparison metrics and quality
  gates rely only on stable classes.
- A one-second or heavily contended run cannot be marked valid with zero
  attempts or zero successes.

## 5. Lockfile and source provenance

- [ ] Add the root `Cargo.lock` SHA-256 digest to matched run and comparison
  provenance.
- [ ] Record whether the repository working tree was dirty when the benchmark
  ran.
- [ ] Record the resolved workspace commit for `rumqttc-v5-next`.
- [ ] Record the exact locked `mqtt5` version and registry/source identity from
  resolved Cargo metadata rather than relying only on duplicated hard-coded
  strings.
- [ ] Record the exact enabled benchmark feature set in a single canonical
  field.
- [ ] Preserve all existing environment fields and do not replace valid
  fallback values with null values.
- [ ] Add tests for clean/dirty parsing, lockfile hashing, Cargo metadata
  extraction, and summary preservation.

Acceptance criteria:

- A report can identify the source commit, dirty state, lockfile contents,
  dependency version/source, optimization profile, and enabled features.
- Provenance collection is read-only and occurs outside timed measurement.
- Missing optional provenance produces an explicit unavailable/null field and
  does not abort a benchmark run.

## 6. Common outstanding-publish diagnostics

- [ ] Instrument only the shared harness-owned publish lifecycle. Do not infer
  private client queue state or expose asymmetric client-specific milestones.
- [ ] Report:
  - common publish operations outstanding at the measurement deadline;
  - the peak common outstanding count during measurement;
  - common operations still outstanding after the bounded completion/drain
    phase.
- [ ] Define an outstanding operation consistently as a publish task that has
  acquired the shared window permit and has not yet returned from the
  adapter's public publish-completion operation.
- [ ] Ensure increments/decrements are cancellation-safe and cannot underflow.
- [ ] Invalidate a run if common operations remain outstanding after the
  configured bound.
- [ ] Add tests covering immediate completion, delayed completion, rejection,
  timeout, and cancellation.
- [ ] Document that these are shared-harness diagnostics, not true internal
  local-admission, socket-write, or broker-ack queue depths.

Acceptance criteria:

- Both adapters use identical instrumentation outside their adapter
  implementations.
- The counters reconcile with attempts/completions/rejections for completed
  runs.
- Instrumentation does not add locks or per-message allocation to the hot
  publish path beyond existing task/semaphore mechanics.

## Required final verification

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `python3 -m unittest discover -s benchmarks/tests -p 'test_*.py'`.
- [ ] Run `cargo check --locked -p benchmarks --all-targets`.
- [ ] Run `cargo test --locked -p benchmarks --lib --bins`.
- [ ] Run `cargo test --locked -p benchmarks --test codec_smoke`.
- [ ] Run `cargo clippy --locked -p benchmarks --all-targets -- -D warnings`.
- [ ] Run the corresponding `alloc-metrics` check and Clippy commands.
- [ ] Run `git diff --check`.
- [ ] Run short normal-path smoke tests for both matched adapters.
- [ ] Run the deterministic fault contract smoke set.
- [ ] If local facilities permit, run one matched TLS fixture smoke and one
  EMQX TCP fixture smoke. Treat unavailable Docker/broker facilities as
  explicitly reported skipped verification, not as successful verification.
- [ ] Confirm that no generated benchmark results, temporary certificates,
  broker containers, or background processes remain in the repository or
  running after verification.

## Explicit non-goals

Do not add any of the following as part of this TODO:

- physical CPU-count discovery;
- authentication benchmarking or credential configuration;
- adapter-specific guesses for true local admission, packets written, or
  private client queue depth;
- automatic full workload-matrix generation;
- automatic saturation calibration;
- automatic suspicious-outlier reruns;
- automatic CPU pinning or process/core isolation;
- controlled-network-path provisioning;
- routine packet capture or broker-log QoS verification;
- a generalized broker queue-monitoring abstraction;
- full EMQX TLS/WebSocket transport parity;
- a generalized profiling orchestration system;
- randomized chaos testing.
