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

See [`PERSISTENCE.md`](PERSISTENCE.md) for methodology and
[`PERSISTENCE-RESULTS.md`](PERSISTENCE-RESULTS.md) for recorded results.
