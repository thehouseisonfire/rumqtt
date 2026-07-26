# Apples-to-Apples `rumqttc` vs `mqtt5` Library Benchmark

> Implementation status: the direct-library harness, schema-v2 paired runner,
> correctness accounting, optional allocation metrics, synthetic smoke
> coverage, and pinned Mosquitto/EMQX fixtures are implemented. Unchecked
> performance-matrix and publication gates intentionally require controlled
> benchmark hardware and are not claimed from a development or shared-CI host.

## Goal

Build a controlled, reproducible benchmark that can determine where the
`rumqttc` and LabOverWire `mqtt5` client libraries are faster without measuring
materially different CLI implementations.

The existing `compare-external` result is an application-level comparison
between this repository's `rumqtt-bench` driver and `mqttv5 bench`. It is useful
for comparing those tools as shipped, but it is not sufficient evidence that
one underlying library is intrinsically faster. The two drivers differ in
payload construction, topic construction, callback/event-loop architecture,
request buffering, publish completion semantics, and per-message work.

This plan replaces those differences with one shared workload implementation
and two minimal library adapters.

## Result We Must Be Able to Defend

The final report should answer separate, precisely scoped questions:

- Which client sustains more end-to-end delivered messages per second?
- Which client has lower publish-to-receive latency at equal offered load?
- Which client uses less CPU and memory at equal delivered throughput?
- How do connection establishment and teardown rates compare?
- How sensitive is each result to QoS, payload size, concurrency, broker, and
  flow-control settings?

There may not be one overall winner. Conclusions must name the workload,
library versions, broker, transport, and completion milestone to which they
apply.

## Provenance and Version Control

- [x] Add `mqtt5` as a direct, exact-version benchmark dependency rather than
  invoking `mqttv5-cli`.
- [x] Initially pin `mqtt5` to `=0.37.2`; record the resolved version and source
  in every result.
- [x] Continue benchmarking the current workspace `rumqttc-v5-next` source and
  record its package version and Git commit.
- [ ] Record Rust version, target triple, enabled Cargo features, optimization
  profile, allocator, operating system, CPU model, logical/physical CPU count,
  and total memory.
- [x] Commit the resulting `Cargo.lock` changes so dependency resolution is
  reproducible.
- [x] Provide an explicit version-update procedure. Updating either library
  invalidates comparisons with previous results unless both sides are rerun.

## Shared Harness Architecture

- [x] Add a single benchmark executable with a backend selector such as
  `--client rumqttc|mqtt5`.
- [x] Put scenario setup, payload allocation, topic generation, timing,
  sampling, statistics, output serialization, and shutdown logic in
  backend-neutral code.
- [x] Define a small internal client-adapter trait implemented once for
  `rumqttc` and once for `mqtt5`.
- [x] Keep adapters minimal: connect, subscribe, publish, observe completion,
  receive, disconnect, and expose supported protocol limits.
- [x] Run each backend in a fresh subprocess. Do not measure both libraries
  concurrently in one process.
- [x] Alternate backend execution order for paired runs.
- [x] Keep the current external-CLI comparison available, but label it
  separately as an application-level comparison.

## Fairness Contract

Every paired run must use identical values for:

- [ ] MQTT protocol version and transport.
- [ ] Broker address and broker configuration.
- [ ] Clean Start, Session Expiry Interval, keepalive, and authentication.
- [ ] QoS, retain flag, topic, subscription filter, and MQTT properties.
- [ ] Exact application payload bytes and exact encoded payload length.
- [ ] Publisher and subscriber counts.
- [ ] Warmup, measurement, drain, and timeout durations.
- [ ] Offered rate or maximum outstanding publish window.
- [ ] Broker Receive Maximum and client-side in-flight limit where configurable.
- [ ] Success criteria and the metric's start and stop boundaries.

The shared harness must:

- [ ] Prebuild the same immutable payload outside the measured hot loop for
  throughput tests.
- [ ] Generate sequence numbers and timestamps in shared code only when the
  scenario requires them.
- [ ] Use exactly the same topic bytes for both adapters; do not append
  backend-specific publisher suffixes.
- [ ] Perform equivalent subscriber work. The receive callback/event loop may
  increment a counter or decode the same shared timestamp, but must not do
  backend-specific parsing or logging.
- [ ] Disable per-message console output and tracing unless the same work is
  enabled for both sides.
- [ ] Start measurement only after every connection is established and every
  subscription is acknowledged.
- [ ] Stop publishers at the same deadline, then allow the same bounded drain
  period before taking final delivered counts.
- [ ] Treat lost, duplicated, rejected, late, and out-of-window messages
  consistently.

## Publish and Backpressure Semantics

This is the most important source of accidental unfairness.

- [ ] Document what each library's async publish call means: accepted into a
  local queue, written to the socket, or acknowledged by the broker.
- [ ] Do not compare raw publish-call counts when the calls represent different
  completion milestones.
- [ ] Use subscriber-observed, unique in-window deliveries as the primary
  saturation-throughput metric.
- [ ] Report separately:
  - publish attempts;
  - locally accepted publishes;
  - packets written, if observable;
  - broker-acknowledged publishes for QoS 1/2, if observable;
  - unique subscriber deliveries;
  - duplicates and losses;
  - messages remaining queued or in flight at shutdown.
- [ ] Implement a common bounded-outstanding-work policy. Prefer a semaphore
  released at the same MQTT milestone for both clients.
- [ ] If either public API cannot expose a matching milestone, document the
  limitation and run both:
  - a bounded-rate end-to-end test where neither client saturates its queue;
  - a saturation test whose primary metric is subscriber delivery and whose
    queue/in-flight settings are reported prominently.
- [ ] Never infer successful QoS delivery solely from a publish method returning
  successfully.

## Workload Matrix

### Saturation Throughput

- [ ] MQTT 5 TCP with QoS 0 and QoS 1.
- [ ] Payloads of 0 B, 64 B, 1 KiB, and 16 KiB.
- [ ] Topologies of 1 publisher/1 subscriber, 4/1, and 1/4.
- [ ] At least one explicitly matched in-flight/window setting.
- [ ] Primary metric: unique subscriber deliveries per measured second.
- [ ] Secondary metrics: accepted publishes, acknowledged publishes, loss,
  duplicates, CPU time, peak RSS, and allocation counts if available.

### Fixed-Rate Latency

- [ ] Use shared timestamp/sequence encoding and decoding.
- [ ] Test rates comfortably below both clients' saturation point, then at
  increasing common rates approaching saturation.
- [ ] Use the same pacing algorithm and missed-deadline policy.
- [ ] Report p50, p95, p99, and maximum end-to-end latency plus achieved rate,
  loss, duplicates, and coordinated-omission warnings.
- [ ] Do not compare latency if either side fails to maintain the requested
  offered rate or the report must clearly classify that run as overload.

### Connection Churn

- [ ] Match clean-session settings, authentication, keepalive, and immediate
  post-CONNACK behavior.
- [ ] Measure successful CONNECT/CONNACK/DISCONNECT cycles per second.
- [ ] Run both serial and bounded-concurrency variants.
- [ ] Report failures, timeouts, and broker-side resource-limit responses.

### Efficiency at Equal Work

- [ ] Choose fixed offered rates that both clients can sustain without loss.
- [ ] Measure process CPU time, peak RSS, and allocations where tooling permits.
- [ ] Report CPU nanoseconds and bytes allocated per delivered message.
- [ ] Pin or isolate benchmark processes to reduce scheduler noise when the
  host supports it.
- [ ] Capture profiles outside the timed statistical run and label them
  diagnostic rather than directly comparable measurements.

## Broker Matrix

- [ ] Use the in-repository synthetic router for client-side isolation and
  deterministic smoke tests.
- [ ] Use a pinned Mosquitto version as the primary realistic local broker.
- [ ] Add at least one second production broker implementation to detect
  broker-specific interactions before making general claims.
- [ ] Run the broker on a dedicated core or separate host for publish-rate
  saturation tests when possible.
- [ ] Monitor broker CPU, memory, disconnects, and queue pressure. Reject a run
  as broker-limited when the broker saturates before both clients do.
- [ ] Repeat a representative subset over loopback and over a controlled network
  path. Do not combine those results.
- [ ] Keep TLS results separate from plain TCP results and use identical TLS
  versions, trust roots, session-resumption policy, and crypto provider where
  possible.

## Correctness Validation Before Performance

- [ ] Verify exact payload size and bytes at the subscriber.
- [ ] Verify exact topic and subscription behavior.
- [ ] Verify QoS handshake behavior with packet captures or broker logs for a
  small diagnostic run.
- [ ] Verify that sequence accounting detects loss and duplicates.
- [ ] Verify that warmup messages cannot enter measured totals.
- [ ] Verify that post-deadline drain messages are classified consistently.
- [ ] Add adapter contract tests using the synthetic router.
- [ ] Add tests for counter reset, timing boundaries, timeout, broker
  disconnect, rejected publish, and incomplete drain behavior.

## Statistical Procedure

- [ ] Use paired runs with alternating or randomized backend order.
- [ ] Require at least 12 measured pairs for publishable conclusions; allow
  smaller runs only for smoke validation.
- [ ] Keep one or more warmup runs outside measured statistics.
- [ ] Report every raw sample, median, mean, standard deviation, coefficient of
  variation, median absolute deviation, and paired bootstrap confidence
  interval.
- [ ] Define noise and confidence gates before examining results.
- [ ] Mark comparisons inconclusive when confidence intervals cross the
  equivalence threshold or quality gates fail.
- [ ] Define a practical-equivalence band, initially ±5%, so statistically
  detectable but operationally trivial differences are not called wins.
- [ ] Rerun suspicious outliers; never delete them without recording the reason.
- [ ] Avoid other CPU-intensive work, power-saving frequency changes, and
  thermal throttling during measurements.

## Reports and Reproduction

- [x] Extend schema version 1 or introduce a documented schema version 2 for
  matched-library comparisons.
- [x] Store the complete normalized configuration and both backend-specific
  effective configurations.
- [x] Store commands, raw stdout/stderr, raw measurements, environment metadata,
  quality-gate results, and comparison statistics.
- [x] Generate JSON, CSV, and HTML reports.
- [x] Add a one-command reproduction recipe to `benchmarks/README.md`.
- [x] Document methodology and interpretation rules in
  `benchmarks/BENCHMARKING.md`.
- [x] Add CI smoke tests for both adapters without treating shared CI runners as
  performance evidence.

## Acceptance Criteria

The controlled comparison is complete only when:

- [x] Both libraries are invoked directly by the same harness without
  `mqttv5-cli`.
- [ ] All fairness-contract fields are verified or an unavoidable mismatch is
  called out beside every affected result.
- [ ] Correctness tests pass for both adapters.
- [ ] The primary Mosquitto matrix completes with at least 12 valid paired runs
  per reported scenario.
- [ ] Neither client nor broker reports errors, unexplained loss, or an
  incomplete drain in a run used for a throughput conclusion.
- [ ] Noise and confidence gates pass.
- [ ] Results are reproducible from a clean checkout using documented commands.
- [ ] Conclusions are scoped by workload and do not turn one scenario into a
  universal claim about either project.

## Final Decision Language

Use statements such as:

> With MQTT 5 over TCP, QoS 1, a 1 KiB payload, one publisher and one
> subscriber, a matched outstanding window of 100, and Mosquitto X.Y on this
> host, library A delivered N% more unique messages per second than library B
> (paired 95% confidence interval L% to H%).

Do not use an unqualified statement such as "`rumqttc` is faster than `mqtt5`."
If different workloads produce different winners, report the crossover instead
of selecting one project as the universal winner.
