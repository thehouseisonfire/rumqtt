# Linux execution evidence

Release builds on an Intel Core i5-13500H (16 logical CPUs), Linux 7.2.9
CachyOS/glibc 2.44, Rust 1.96.1, Python 3.14.7 and Mosquitto 2.1.2.
[Method, results and limits](../execution.md) describe the offered workload.
The source is the uncommitted TODO33 implementation based on commit
`ae468c150aca8b2a8445cffb0c641c81f60d31d7`; environment files preserve exact Rust
source and artifact SHA-256 values, tool versions and resource settings. Source
hashes describe the measured builds; later documentation-only edits do not
change their execution behavior.

- `workloads.csv`: 1,584 passing runs; three repetitions of both consumers,
  both protocols, 1/10/100/1,000 clients, three execution modes and QoS 0/1/2.
  Idle/reconnect are repeated only once per QoS matrix, since they do not publish.
- `resources.csv`: 672 passing runs; seven repetitions of idle and bounded
  busy-client workloads. C idle and all Python cases come from
  `execution-resources`; C busy-client cases come from `execution-bounded-hotspot`.
- `original-saturation.csv`: all 168 original C busy-client runs, including
  26 observer timeouts. This producer admitted untracked QoS 0 continuously;
  the final C producer instead bounds its outstanding work to eight tracked
  QoS 0 operations. These workloads are distinct and their percentiles must
  not be combined. The failure occurred in every execution mode and both protocols.
- `tls-smoke.csv`: eight passing TLS publish runs, dedicated/shared, both
  consumers and protocols. These are smoke tests, not a resource comparison.
- `raw.jsonl.gz`: all 2,432 original run records, including every individual
  latency sample and failure. Each line has `suite` and `record` fields;
  failing entries additionally preserve `consumer_stderr`. Suite names
  distinguish repeated case identifiers across the original and bounded runs.
- `*-environment.json`: original metadata for each suite. The bounded workload
  changes the C benchmark executable hash; the shared/shard Rust library hashes
  are identical across the three TCP suites. The TLS smoke uses the same selected
  C/Python libraries. The private shard artifact changes only the
  Tokio runtime builder as described in the method; no shard mode is shipped.

CSV percentiles are per run; comparison tables use their medians. Empty cells
mean unavailable measurements, including observations from failed cases, not
zero latency. Inspect a raw record with standard Python:

```python
import gzip
import json

with gzip.open("raw.jsonl.gz", "rt") as source:
    for line in source:
        entry = json.loads(line)
        print(entry["suite"], entry["record"]["case"], entry["record"]["status"])
```

These are Linux TCP measurements. TLS smoke runs passed through both real
consumers and protocols, but are not the resource baseline. The local broker
lacks WebSocket listener support; WS/WSS measurement attempts did not run MQTT
traffic. macOS/Windows measurements remain pending CI execution.
