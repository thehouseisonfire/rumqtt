# MQTT persistence benchmarks

This package owns MQTT-specific persistence workloads: canonical v4/v5 session
codec cost, checkpoint growth, production adapter recovery, and broker-backed
MQTT persistence behavior.

```bash
cargo run --release --manifest-path session-store-file/Cargo.toml \
  -p session-store-file-benchmarks --bin rumqtt-session-store-file-bench -- \
  persistence codec --protocol v4 --mode encode --inflight 100
```

Protocol-neutral envelope, filesystem, coordination, lifecycle, maintenance,
and backpressure benchmarks live with
[`atomic-blob-store`](https://github.com/thehouseisonfire/atomic-blob-storage).
The repository-level Python runner continues to route the retained
`persistence-*` scenarios to this package.

See [PERSISTENCE.md](PERSISTENCE.md) for methodology and
[PERSISTENCE-RESULTS.md](PERSISTENCE-RESULTS.md) for recorded results.
