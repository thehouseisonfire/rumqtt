# Structured diagnostics measurements

The structured snapshot shares one immutable native capture per client. Native
publication replaces the cache after a completed poll; acquisition reads that
cache and independently samples wrapper observations without waking the driver.

## Method

The ignored `handle::diagnostics_tests::diagnostics_cost_measurements` unit test
measures both protocols with 16 and 4,096 outstanding QoS 1 publications. It uses
real native tracking state, populated without network I/O, and the production
wrapper capture, legacy projection, publication, and acquisition functions.

Each operation has 100 warm-up iterations and seven batches of 2,000 iterations.
The results report the minimum, median, and maximum per-iteration batch averages
in nanoseconds. These are batch averages, not individual-call latency percentiles.
`black_box` keeps native inputs opaque and retains the observed results.

- `native_baseline`: the existing native `EventLoop::diagnostics()` scan, which
  the wrapper already performed after completed polls.
- `capture_projection_publication`: that scan, complete owned mapping, existing
  legacy projection, and replacement of the shared native cache. The difference
  from the baseline includes mapping, timing, allocation, reference counting,
  synchronization, and destruction of the preceding cache.
- `owned_acquisition`: production `ClientHandle::diagnostics_snapshot()` with
  configuration, connection, and retry observations and no pending fence or retry
  failure. This includes dropping the returned owned observation. It excludes
  the C handle's box allocation and C accessor projection.

The acquisition fixture has the default redacted configuration and a closed
configuration controller. Reads still use the production snapshot path; this
does not measure contention with profile preparation or event delivery. Separate
integration tests cover concurrent acquisition alongside MQTT, keepalive,
shutdown, and shared-worker peer traffic.

Reproduce from the repository root:

```sh
cargo test --release --manifest-path native-wrappers/Cargo.toml \
  -p rumqttc-wrapper-core-next --no-default-features --lib \
  diagnostics_cost_measurements -- --ignored --nocapture
```

This measures the native work that existed before publication, rather than
historical end-to-end wrapper throughput. It does not measure network throughput,
allocation counts with an instrumented allocator, C call overhead, redirect string
sizes, ordered-fence allocations, retry-error allocation, or contended tail latency.

## Results

The [recorded batch results](diagnostics-results.csv) were collected on an Intel
Core i5-13500H with 16 logical CPUs, Linux 7.2.9 CachyOS, and Rust 1.96.1. The
workspace release profile uses one code-generation unit and LTO. No other build
or benchmark ran alongside these measurements; CPU affinity was not pinned.

Median batch averages, in nanoseconds per operation:

| Protocol | Inflight | Native baseline | Capture/projection/publication | Owned acquisition |
| --- | ---: | ---: | ---: | ---: |
| 3.1.1 | 16 | 783 | 829 | 140 |
| 5 | 16 | 779 | 850 | 140 |
| 3.1.1 | 4,096 | 3,631 | 3,621 | 140 |
| 5 | 4,096 | 3,923 | 3,981 | 140 |

Acquisition remains independent of native tracking population in this fixture.
The capture difference is small compared with the native scan and subject to
short-run timing noise. Noise can reverse small differences, as the v4
large-state result shows. The raw minimum/maximum batch averages preserve that variation. These
results support the bounded incremental work and absence of per-read scans;
they do not establish a universal latency or throughput guarantee.

The native capture occupies 360 bytes in this 64-bit, no-default-features build,
plus the `Arc` reference counts, allocator overhead, and any owned redirect
strings. This private Rust size is neither a C layout nor a portable size promise.

## Retention

Each publication adds one `Arc<NativeDiagnosticsSnapshot>` allocation. Mapping
moves the already captured MQTT 5 redirect strings; it does not make a second
copy. Ordered shutdown adds its optional owned native observation when enabled.
The latest native cache is swapped under a short mutex; allocation, native scans,
and destruction of the preceding cache happen outside that mutex.

Repeated acquisition shares the native `Arc` and does not rescan native vectors
or bitsets. Wrapper observation copies and the C handle allocation belong to the
returned snapshot. A retained snapshot can keep an older native capture alive;
the library itself keeps only the latest capture. A weak-reference regression
publishes 1,000 captures, verifies immediate release of unretained predecessors,
and verifies that dropping a caller's retained snapshot releases its old capture.
Another check verifies generation saturation instead of wrapping.

There is no retained library history, client/callback ownership, application
payload, session checkpoint, credential, or TLS input in these observations.
