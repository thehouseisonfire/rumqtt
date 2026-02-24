# Benchmark Proof: `8420fdf5 -> 63ee4d98` (Strict Interleaved)

Date: 2026-02-23

This tracked proof bundle mirrors key outputs from local regression artifacts.

- `no_batch/`: strict run with default behavior (no batching override)
- `batch16/`: strict run with `RUMQTT_BENCH_MAX_REQUEST_BATCH=16`

Both runs used:

- interleaved branch mode
- `runs=40`, `warmup=2`
- `quality-mode=strict`
- same benchmark binary: `cargo run -p benchmarks --bin rumqttasync --release`

Batch16 run used temporary patch refs (applied symmetrically to both compared refs):

- baseline temp ref from `8420fdf5`: `c68b24d442f5a6f150f41fc71ca3d20f5d5f8a3e`
- target temp ref from `63ee4d98`: `462d785b9e1007e53e8c16a22d0ae2d50db6770e`

Key result summary (primary metric `throughput`, paired analysis):

- no_batch: median delta `+0.004%`, 95% CI `[-0.969%, +1.429%]`, classification `inconclusive`
- batch16: median delta `+177.883%`, 95% CI `[+173.874%, +180.788%]`, classification `uplift`

See `SHA256SUMS.txt` for content checksums.
