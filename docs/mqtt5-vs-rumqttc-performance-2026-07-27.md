# `mqtt5` versus `rumqttc-v5-next`: matched performance results

**Run date:** 2026-07-27  
**Repository commit:** `9bf0cc0c149ca3ec07f0c956ee11f1789c36fdac`  
**Libraries:** workspace `rumqttc-v5-next 0.34.0-alpha` and crates.io `mqtt5 0.38.0`

## Executive summary

The maintained direct-library harness produced one statistically valid comparison:

- Against EMQX 5.9.3, saturated MQTT 5 QoS 1 delivery throughput was **practically
  equivalent**. Median throughput was 53,849 msg/s for `rumqttc-v5-next` and
  54,143 msg/s for `mqtt5`. The paired relative difference was +1.66% for
  `mqtt5`, with a 95% bootstrap confidence interval from -3.11% to +3.00%.
  Because the whole interval lies inside the predeclared ±5% equivalence band,
  neither library demonstrated a practically meaningful throughput advantage.

The remaining comparisons failed one or more predeclared quality gates and do
not support performance rankings:

- Mosquitto saturation had 12/12 valid rumqttc runs but 0/12 valid `mqtt5`
  runs because the `mqtt5` runs did not complete their accepted deliveries
  within the bounded drain.
- The 1,000 msg/s latency scenario was loss-free on both brokers and libraries,
  but the harness pacer achieved only about 698–716 msg/s at the median. Every
  run therefore failed the required 99% offered-rate gate.
- Connection churn produced failed connection cycles for both libraries on
  both brokers. Too few zero-failure pairs remained for statistical inference.

Accordingly, this experiment supports a narrow conclusion—equivalent saturated
QoS 1 throughput on the tested EMQX setup—not a general claim that the
libraries have equivalent performance.

## Experimental design

The comparison used `benchmarks/runner.py compare-libraries` and
`rumqtt-library-bench`. Each library ran in a fresh subprocess. The runner
alternated library order, discarded warm-ups, paired runs by index, and used
10,000-sample paired bootstrap intervals at 95% confidence.

Each reported broker/scenario combination used:

- 1 warm-up pair and 12 measured pairs;
- the Cargo `release` profile;
- MQTT 5 over loopback TCP;
- a fresh broker container, isolated from the other scenarios;
- a strict 100% valid-run requirement;
- at least 12 valid pairs;
- primary-metric CV no greater than 10%;
- primary-metric MAD no greater than 5%;
- relative confidence-interval width no greater than 10%; and
- a predeclared ±5% practical-equivalence band.

Message tests rejected runs with loss, duplicates, malformed traffic, rejected
publishes, an incomplete bounded drain, or (for latency) an achieved rate below
99% of the requested rate. Throughput counted unique subscriber-observed
deliveries before the measurement deadline, not client-side publish calls.

The tested host was Linux x86-64 with a 13th Gen Intel Core i5-13500H, 16
logical CPUs, approximately 24.9 GB RAM, and Rust 1.96.1. Allocation
instrumentation was disabled. Brokers were the pinned Docker images
`eclipse-mosquitto:2.0.22` and `emqx/emqx:5.9.3`; the resolved EMQX image digest
was `sha256:86632adf230bea06c4caad91318825c77aec1e57a820e6711b89fec7d0957eaf`.

## Results

### Quality-gate overview

| Broker | Scenario | Valid rumqttc runs | Valid `mqtt5` runs | Valid pairs | Gate result | Supported conclusion |
| --- | --- | ---: | ---: | ---: | --- | --- |
| EMQX 5.9.3 | QoS 1 saturation, 1 KiB, 1 publisher / 1 subscriber | 12/12 | 12/12 | 12 | Pass | Equivalent within ±5% |
| Mosquitto 2.0.22 | QoS 1 saturation, 1 KiB, 1 publisher / 1 subscriber | 12/12 | 0/12 | 0 | Fail | None |
| EMQX 5.9.3 | QoS 1 latency at requested 1,000 msg/s | 0/12 | 0/12 | 0 | Fail | None |
| Mosquitto 2.0.22 | QoS 1 latency at requested 1,000 msg/s | 0/12 | 0/12 | 0 | Fail | None |
| EMQX 5.9.3 | Serial connection churn | 3/12 | 1/12 | 1 | Fail | None |
| Mosquitto 2.0.22 | Serial connection churn | 7/12 | 4/12 | 3 | Fail | None |

### Valid EMQX throughput comparison

| Metric | `rumqttc-v5-next` | `mqtt5` |
| --- | ---: | ---: |
| Median delivered throughput | 53,849 msg/s | 54,143 msg/s |
| Mean delivered throughput | 53,964 msg/s | 54,264 msg/s |
| Minimum–maximum | 51,955–56,285 msg/s | 50,997–57,091 msg/s |
| CV | 2.65% | 3.62% |
| MAD / median | 2.20% | 2.87% |
| Lost / duplicate / malformed deliveries | 0 / 0 / 0 | 0 / 0 / 0 |
| Median process peak RSS | 263.8 MiB | 425.5 MiB |
| Median process CPU time per delivery | 17.1 µs | 27.1 µs |

The primary paired comparison classified the libraries as equivalent:

| Paired statistic | Result |
| --- | ---: |
| Median relative difference (`mqtt5` versus rumqttc) | +1.66% |
| 95% paired bootstrap interval | -3.11% to +3.00% |
| Interval width | 6.11 percentage points |
| Practical-equivalence band | -5% to +5% |

RSS and CPU-per-delivery are secondary process-level diagnostics, not separately
controlled microbenchmarks. Their observed differences are substantial, but the
harness declares only delivered throughput as the primary metric and does not
compute paired confidence intervals for causal memory or CPU claims. They
should motivate targeted profiling rather than a general efficiency verdict.

### Invalid Mosquitto saturation comparison

Rumqttc completed all accepted deliveries in every measured run. Its median
delivered throughput was 121,609 msg/s, with zero loss and 3.08% CV.

Every `mqtt5` run was invalid. Across all 12 diagnostic samples, median
in-window delivery was 88,405 msg/s and the median count still undelivered
after the bounded drain was 175,426 messages. These figures must not be compared
as if they were valid throughput samples.

There is also an API-semantics asymmetry in this scenario. The rumqttc adapter
uses `publish_tracked(...).wait_completion_async()` for QoS completion, whereas
the `mqtt5` public publish future returns a packet identifier after its own
publish handoff and exposes no equivalent public PUBACK-completion future.
Consequently, the harness's local completion window does not represent the same
milestone for both adapters. End-to-end delivery accounting catches the
resulting backlog, but the asymmetry prevents a clean attribution of the
Mosquitto outcome to intrinsic client throughput.

### Invalid latency comparisons

All four library/broker combinations delivered without loss, duplicates, or
malformed messages. They nevertheless failed the predeclared maintained-rate
gate:

| Broker | Library | Median achieved rate | Diagnostic median p99 |
| --- | --- | ---: | ---: |
| Mosquitto | `rumqttc-v5-next` | 698 msg/s | 303 µs |
| Mosquitto | `mqtt5` | 701 msg/s | 310 µs |
| EMQX | `rumqttc-v5-next` | 712 msg/s | 401 µs |
| EMQX | `mqtt5` | 716 msg/s | 367 µs |

The pacer advances a one-millisecond Tokio sleep for every message and resets
its deadline after a miss. On this host it accumulated thousands of missed
deadlines and could not maintain the requested 1,000 msg/s. The latency values
therefore describe a lower and variable offered load; they are diagnostic only
and cannot establish comparative latency at 1,000 msg/s.

### Invalid connection-churn comparisons

The scenario invalidates a ten-second sample if even one connection cycle
fails. Neither broker produced the required 12 zero-failure pairs:

| Broker | Library | Zero-failure samples | Successful cycles | Failed cycles | Valid-sample median rate |
| --- | --- | ---: | ---: | ---: | ---: |
| Mosquitto | `rumqttc-v5-next` | 7/12 | 55,995 | 1,532 | 415 connections/s |
| Mosquitto | `mqtt5` | 4/12 | 100,904 | 5,484 | 851 connections/s |
| EMQX | `rumqttc-v5-next` | 3/12 | 42,192 | 6,140 | 420 connections/s |
| EMQX | `mqtt5` | 1/12 | 59,476 | 12,491 | 878 connections/s |

The valid-sample point rates favor `mqtt5`, but sample survival is sparse and
selection-biased, while the paired confidence intervals fail the experiment's
quality requirements. The data show that `mqtt5` attempted/completed cycles
faster when a sample remained clean, but they do not establish a reliable
connection-rate advantage under the scenario's zero-failure contract.

## Interpretation

The valid EMQX result suggests that, for a single publisher and subscriber
exchanging 1 KiB QoS 1 messages over local TCP, neither client library is the
dominant throughput bottleneck at approximately 54,000 delivered messages per
second. The broker and complete publish/route/deliver path matter: the same
scenario reached much higher rumqttc throughput on Mosquitto but did not yield
a valid `mqtt5` comparison.

The experiment also reveals that the current harness is not yet sufficient for
a broad library ranking:

1. The latency pacer must be repaired or the scenario must predeclare a
   sustainable rate before collecting inferential latency data.
2. Connection churn needs failure-cause telemetry and likely broker/OS resource
   controls before its rate can be interpreted.
3. QoS 1 publish-completion semantics should be aligned. If `mqtt5` cannot
   expose PUBACK completion, a delivery-window design with a bounded offered
   load should be used instead of treating both publish futures as equivalent.
4. More scenarios are needed: QoS 0 and QoS 2, payload-size sweeps, multiple
   publishers/subscribers, TLS, longer soak tests, and controlled CPU/memory
   profiling.

## Threats to validity

- Results come from one laptop-class host and one run date.
- Broker and clients shared the same host and loopback network.
- CPU governor, core affinity, and background-system activity were not captured
  or controlled beyond keeping this run otherwise idle.
- Only TCP, QoS 1, a 1 KiB payload, and low client concurrency were tested.
- Broker versions, client versions, and compiler optimizations materially limit
  generalization to other releases.
- Peak RSS includes the whole benchmark process and allocator behavior.
- The connection and latency diagnostic medians are drawn from invalid
  experiments and are not inferential results.

## Reproduction and artifacts

The complete retained artifact set is under
[`benchmark-results/mqtt5-vs-rumqttc-20260727`](../benchmark-results/mqtt5-vs-rumqttc-20260727/).
The six isolated full-run directories used in this report are:

- `mosquitto-throughput`
- `mosquitto-latency`
- `mosquitto-connections`
- `emqx-throughput`
- `emqx-latency-ready`
- `emqx-connections`

Each contains the broker-validation manifest plus the scenario's `summary.json`,
`summary.csv`, HTML report, and per-process raw JSON. Smoke tests, the initial
EMQX readiness failure, and an aborted multi-scenario Mosquitto batch are also
retained for auditability but excluded from the reported results.

The benchmark was invoked per scenario and broker with the equivalent of:

```text
python3 benchmarks/broker_fixture.py validate \
  --backend docker --broker <mosquitto|emqx> --transport tcp \
  --scenario <matched-scenario> \
  --runs 12 --warmup-runs 1 --cargo-profile release \
  --timeout-sec 600 --output-dir <artifact-directory>
```
