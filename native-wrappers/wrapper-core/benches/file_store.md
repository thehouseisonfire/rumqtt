# Maintained file-store checkpoint costs

Run from the repository root:

```sh
cargo bench --manifest-path native-wrappers/Cargo.toml \
  -p rumqttc-wrapper-core-next --no-default-features \
  --features file-session-store --bench file_store
```

`RUMQTTC_FILE_STORE_BENCH_ITERATIONS` selects the iteration count (default 20).
The harness checks round-trip equality for both protocols and payloads of 1 KiB,
64 KiB and 1 MiB. Each sample includes an owned worker submission, checkpoint
conversion, atomic save, load and conversion back to a wrapper checkpoint. It
uses the maintained engine and a temporary local directory, without MQTT or
network traffic. Context creation, opening and joined teardown are outside the
sample interval.

`setup_encode_ns` separately measures building one native session, allocating
its payload and encoding it. It is not a pure codec microbenchmark. Save/load
latency is a percentile over the paired operations, with allocation counting
enabled. These measurements do not establish MQTT throughput or application
latency under load.

Allocation counts and cumulative requested bytes cover the measured operations,
including endpoint/engine work and sample-vector growth. They exclude setup and
teardown. They are neither retained heap size nor peak RSS; deallocations are
not subtracted. The maximum checkpoint-buffer reservation is a logical ceiling
derived from this harness's 2 MiB native serialized limit, excluding copied
keys/paths, allocator overhead, thread stacks and engine overhead. Actual
admission reservations adapt to known snapshot or file size. Operation and
observation limits apply independently.

Raw [JSONL results](file_store-results.jsonl) are retained beside this document. Re-run on the deployment
filesystem when selecting limits; durability cost depends on the filesystem,
storage device and scheduler.

## Local sample

Measured on Linux 7.2.11 (x86-64), Intel Core i5-13500H, Rust 1.96.1,
with 20 iterations per row. Other validation builds/tests ran concurrently;
CPU frequency and affinity were not controlled. These are smoke measurements
for representative checkpoint costs, not statistical performance claims.

| Protocol | Payload | Setup/build/encode µs | Save/load p50 µs | Save/load p90 µs | Allocation calls | Cumulative bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| V4 | 1 KiB | 12.0 | 125.9 | 328.7 | 2,122 | 1,662,936 |
| V4 | 64 KiB | 134.9 | 190.1 | 380.6 | 2,179 | 17,187,395 |
| V4 | 1 MiB | 1169.6 | 1742.2 | 3793.8 | 2,707 | 272,785,443 |
| V5 | 1 KiB | 2.2 | 74.0 | 84.4 | 2,119 | 1,669,315 |
| V5 | 64 KiB | 9.9 | 167.1 | 201.1 | 2,181 | 17,194,127 |
| V5 | 1 MiB | 771.1 | 1492.9 | 1915.5 | 2,693 | 272,790,815 |

Allocation totals cover all 20 save/load pairs. Each row uses a logical
checkpoint-buffer reservation ceiling of 16 MiB plus 64 bytes, with copied
keys/paths charged separately. The defaults retain a 256 MiB context budget,
16 MiB attachment bound (including its eight-byte envelope), two workers,
64 stores, and 256 operation/observation slots. These are independently
configurable admission bounds, not measured per-client memory requirements.
