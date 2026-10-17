# Native protocol observation allocation measurements

Measured on 2026-10-16, Linux x86_64, Intel Core i5-13500H, Rust 1.96.1.
The pre-change baseline is commit `ab4e0a9926c8ae726cc695f505e66593e7a4e9fa`.
Both baseline and current use the same release harness and no default Cargo
features. Copy the harness and its Cargo bench entry into a baseline archive and
remove only the marked `BEGIN TODO44 SELECTION` / `END TODO44 SELECTION` block to
build the baseline. Do not request observations from that baseline binary.

## Method and interpretation

Each process warms up 32 QoS 1 transactions, then measures 1,000 against a loopback
TCP peer. MQTT 5 PUBACKs carry an 8,192-byte Reason String and duplicate User
Properties, including a present-empty value. MQTT 3.1.1 carries no properties.
Enabled runs select incoming control and precise outgoing observations. Each
transaction waits for its authoritative completion and drains its two events.
Retained runs keep at most 64 caller-owned events; slow runs delay each event
consumer by 100 microseconds. All waits have finite deadlines.

Counters include the entire process, including the peer, runtime, native decoder,
terminal notice copies, and wrapper. Startup allocation is excluded. Allocation
counts and bytes vary with scheduling/channel wakeups; three repetitions are
recorded for each scenario. Timings are raw observations under concurrent build
load, not throughput claims or regression thresholds.

`peak_extra_bytes` is the process live-allocation peak above the warmed baseline.
`released_bytes` counts bytes released by dropping the bounded caller container
after client destruction. It includes that container (152 bytes in zero-retention
runs), so it is not solely packet property storage. The enabled delivery loop
has an additional bounded future allocation; queue/staging event clones share
packet detail storage. Disabled delivery does not allocate that future, clone new
properties, or retain packet history.

## Results

Means of three runs; byte columns are rounded. Raw samples, retained-event counts,
and elapsed times are in [the CSV](protocol-observations-results.csv).

| Protocol | Scenario | Allocations | Allocated bytes | Peak extra bytes | Bytes released with caller container |
| --- | --- | ---: | ---: | ---: | ---: |
| v4 | Pre-change disabled | 31,527 | 4,858,227 | 4,202 | 152 |
| v4 | Current disabled | 31,605 | 4,872,333 | 4,261 | 152 |
| v4 | Enabled, immediately released | 36,866 | 5,512,696 | 4,633 | 152 |
| v4 | Enabled, retain 64 events | 36,829 | 5,511,952 | 15,844 | 20,632 |
| v4 | Enabled, retain 64, slow consumer | 37,119 | 5,557,536 | 15,119 | 20,632 |
| v5 | Pre-change disabled | 39,322 | 22,770,960 | 29,492 | 152 |
| v5 | Current disabled | 39,506 | 22,809,237 | 28,544 | 152 |
| v5 | Enabled, immediately released | 47,052 | 23,896,861 | 28,539 | 152 |
| v5 | Enabled, retain 64 events | 47,514 | 23,984,373 | 308,707 | 289,144 |
| v5 | Enabled, retain 64, slow consumer | 47,646 | 24,018,075 | 307,786 | 289,144 |

Disabled allocation measurements remain close to the baseline; these noisy whole
process totals do not prove zero total overhead. Inspection of selection guards
establishes the stronger narrow contract: no added property copies or history
allocations while disabled. Enabled retention scales with caller-retained events
and decoded packet sizes, and the final owner releases their storage. Automatic
retention is bounded by the existing queue plus in-progress delivery and the
single ordered-shutdown staging slot. No completed-packet cache is added.

## Reproduce current measurements

```sh
cargo bench --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next \
  --bench protocol_observations --no-default-features -- --v5 --messages 1000
cargo bench --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next \
  --bench protocol_observations --no-default-features -- \
  --v5 --observations --messages 1000 --retain 64 --slow-us 100
```

Omit `--v5` for MQTT 3.1.1. Use `--property-bytes` to vary the MQTT 5 packet size,
and omit `--retain` / `--slow-us` for immediate release without consumer delay.
Property sizes remain subject to MQTT string and decoder limits. The harness
measures allocation requests, not allocator RSS or operating-system reclamation.
