# File Session-Store Benchmarks

This package owns the maintained persistence workloads for the file-backed
session-store workspace. It emits the same JSON schema as the repository's
client benchmark package and uses the shared Python scenario runner.

Run a workload directly:

```bash
cargo run --manifest-path session-store-file/Cargo.toml \
  -p session-store-file-benchmarks --bin rumqtt-session-store-file-bench -- \
  persistence envelope --payload-size 1048576
```

Run a named scenario from the repository root:

```bash
python3 benchmarks/runner.py run \
  --scenario persistence-envelope-1mib \
  --runs 5 \
  --warmup-runs 1
```

See [`PERSISTENCE.md`](PERSISTENCE.md) for methodology, including lifecycle and
resource measurements, and [`PERSISTENCE-RESULTS.md`](PERSISTENCE-RESULTS.md)
for recorded results.

File-store save and load scenarios use the production streaming API by
default. Load latency includes the checksum-validation pass and subsequent
payload-delivery pass. Pass `--io-mode complete` to measure the allocation-heavy
convenience methods separately.

The `maintenance`, `backpressure`, and `recovery` commands cover ordered
barriers, bounded streaming under deterministically gated endpoints, and MQTT
v4/v5 checkpoint loading and state application through the production adapter
and client restore paths. Each emits raw schema-version-1 samples plus p50,
p95, and p99 metrics.
