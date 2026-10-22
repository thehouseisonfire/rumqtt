# Owned publish buffer proof and measurement

Predeclared gate for TODO51: both protocols must show at least a 10% producer
publish-cost reduction or a 90% reduction in producer payload-attributable
allocated bytes in the same large/reused workload. Existing copying calls must
remain within a 5% regression margin against the pre-public-implementation
baseline, with paired 95% bootstrap confidence intervals. Inconclusive evidence
does not pass. Correctness and complete owner reclamation are mandatory.

The private `publish-buffer-proof` feature links a C malloc/free owner fixture.
It adds no public API or capability. Run the ownership tests in debug builds and
the ignored measurements in release builds, serially, with no concurrent builds.

The controlled native driver waits for CONNACK with one queued publication.
Rejected producer attempts therefore exercise real C admission without network
throughput or driver encoding confounding the FFI copy measurement. Separate
command-construction and create/build/release samples describe accepted-input
preparation and single-use costs; they do not measure successful MQTT delivery.
Final public API validation also covers successful admitted publication.

Measure 21 alternating paired repetitions at 64 B, 1 KiB, 16 KiB, 256 KiB and
1 MiB. Timing samples disable allocation instrumentation. Allocation samples
count Rust allocation/reallocation requests on the producer thread only; native
workers, allocator metadata and C allocations are excluded. The benchmark allocator
still performs a disabled tracking check in timing samples, so timing ratios are
diagnostic; the allocation gate and the external production-artifact comparison
provide acceptance evidence. Identical input
preparation is included in lifecycle samples. The C owner fixture independently
proves application allocations can be released by C on another thread.
Retained application storage is the distinct input allocation per size, shared
across reused attempts; count release after all references disappear. This is
neither process RSS nor a whole-stack copy/allocator measurement.

```sh
RUMQTTC_BUFFER_RESULTS="$PWD/native-wrappers/target/buffer-baseline.jsonl" \
  cargo test --release --manifest-path native-wrappers/Cargo.toml \
  -p rumqttc-c-next --no-default-features --features publish-buffer-proof \
  publish_buffer_measurements -- --ignored --nocapture --test-threads=1
```

Record raw results, environment, final public API rerun and outcome here before
claiming the public feature has passed. A rejected-attempt allocation benefit
does not establish higher broker throughput or end-to-end zero-copy.

## Public API outcome (Linux, 2026-10-20)

The performance gate passes. The public implementation retains application bytes
without a payload-sized FFI allocation/copy for successful, rejected and deferred
admission. Existing copying APIs pass the 5% regression margin in the isolated
external C comparison described below. This is a measured FFI benefit, not a
network-throughput or end-to-end zero-copy result.

The [raw evidence](publish-buffer-results/linux-2026-10-20/) includes environment,
source/artifact provenance, complete repeated samples, diagnostic summaries and
`buffer-gate.json`. No binary artifact is checked into the repository. The
private-stage baseline shared library was preserved before public API changes;
its copying functions are unchanged from the recorded Git base. Build both
libraries with the same compiler, release profile and transport feature set when
repeating the comparison.

Producer-thread allocated bytes per 1 MiB publication/attempt (copying / owned):

| Protocol | Successful admission | Rejected admission | Deferred creation |
| --- | ---: | ---: | ---: |
| MQTT 3.1.1 | 1,048,929 / 329 | 1,048,952 / 352 | 1,048,872 / 296 |
| MQTT 5 | 1,048,961 / 361 | 1,049,312 / 712 | 1,048,872 / 296 |

The avoided payload allocation exceeds the agreed 90% criterion at both 256 KiB
and 1 MiB in both protocols. Each copying call also copies the logical payload
length at the FFI boundary; pointer assertions and the owned construction path
establish zero payload-copy bytes at that boundary for buffer publishes. These
are selected-path copy counts, not instrumentation of codecs or dependencies.

Public owned-buffer handles add creation/bookkeeping costs. At 64 B, the measured
owned create/build/release lifecycle is approximately three times the copying
constructor lifecycle; ordinary copying calls remain the default. The lifecycle
comparison includes identical application input allocation/initialization and
counts owned handle creation and final release. It does not measure MQTT delivery.

Reusable cases retain one original application input allocation of the reported
capacity through the sample, independently of logical per-attempt charges.
Admission measurements report that backing length explicitly and assert one final
callback after client cleanup. Single-use input lives through each operation's
final release. Native ownership tests separately hold queued/replayed storage,
cancel pending attempts, save independent checkpoints and check reclamation after
shutdown. These known input capacities and owner lifetimes do not measure process
RSS, allocator caches or arbitrary host pool/mapping capacity.

### Isolating copying-call regression checks

The original Rust producer diagnostic changes its owned branch from a private
`Bytes` proof to public handles. Some tiny constructor/lifecycle comparisons fell
outside the 5% margin; those loops cannot isolate changes to exported copying
calls from changes to the benchmark itself. The raw diagnostic failures are kept.

`copy_path.c` resolves the same existing C API symbols from either shared library.
One compiled external producer therefore measures identical caller code against
both artifacts. A TCP peer accepts CONNECT and withholds CONNACK; one publication
fills the native request channel, and measured attempts must all return
backpressure. A lifecycle variant also includes C input allocation/initialization
and release. Library startup, warm-up, shutdown and loading are outside timing.
Both protocols and all five payload sizes have 21 alternating paired process
samples, with CPU affinity fixed to CPU 0. Timing runs had no concurrent builds;
feature-matrix compilation was suspended between tests and resumed afterward.

Every case except 256 KiB rejected admission met the 5% upper confidence bound in
the initial external matrix. Those two cases were inconclusive (medians below
baseline, wide confidence intervals). A targeted 21-pair follow-up used 8,192
iterations per sample instead of 256, preserving the workload and acceptance
margin. Its current/baseline median ratios and paired 95% intervals were:

| Protocol | Median ratio | 95% interval |
| --- | ---: | ---: |
| MQTT 3.1.1 | 0.978 | 0.975–0.981 |
| MQTT 5 | 0.977 | 0.975–0.980 |

The complete external matrix plus that longer follow-up clears the copying-call
gate. The validator only permits the longer follow-up to supplement inconclusive
256 KiB cases; it does not discard a confidently regressed case. Source/API
behavior and native fixtures provide separate correctness evidence.

### Reproduction

The ignored Rust tests produce three-phase and successful/deferred-admission
measurements. Allocation instrumentation is disabled during timing samples;
admitted/deferred samples time API entry separately from peer progress or
cancellation observation. The latter cleanup time is recorded independently.

```sh
RUMQTTC_BUFFER_RESULTS="$PWD/native-wrappers/target/buffer-final.jsonl" \
  cargo test --release --manifest-path native-wrappers/Cargo.toml \
  -p rumqttc-c-next --no-default-features --features publish-buffer-proof \
  publish_buffer_measurements -- --ignored --nocapture --test-threads=1
RUMQTTC_BUFFER_ADMISSION_RESULTS="$PWD/native-wrappers/target/buffer-admission.jsonl" \
  cargo test --release --manifest-path native-wrappers/Cargo.toml \
  -p rumqttc-c-next --no-default-features --features publish-buffer-proof \
  publish_buffer_admission_measurements -- --ignored --nocapture --test-threads=1
cc -std=c11 -O3 -Wall -Wextra -Wpedantic -Werror \
  -I native-wrappers/c/include native-wrappers/c/benches/copy_path.c -ldl \
  -o native-wrappers/target/buffer-copy-producer
python3 native-wrappers/c/benches/compare_copy_path.py \
  --producer /absolute/path/to/buffer-copy-producer \
  --baseline /absolute/path/to/baseline/librumqttc.so \
  --current /absolute/path/to/current/librumqttc.so \
  --output native-wrappers/target/buffer-copy-external.jsonl
# The same command with these additional arguments reproduces the longer case:
# --size 262144 --phase rejected --iterations 8192
```

Validate the checked-in evidence without rerunning benchmarks:

```sh
python3 native-wrappers/c/benches/verify_publish_buffer_gate.py \
  --benefit native-wrappers/c/benches/publish-buffer-results/linux-2026-10-20/buffer-final.jsonl \
  --admission native-wrappers/c/benches/publish-buffer-results/linux-2026-10-20/buffer-admission.jsonl \
  --copying native-wrappers/c/benches/publish-buffer-results/linux-2026-10-20/buffer-copy-external.jsonl \
  --followup native-wrappers/c/benches/publish-buffer-results/linux-2026-10-20/buffer-copy-followup.jsonl
```

The external loader measurement is Linux-specific. Portable native C ownership,
example and unload fixtures are wired into the existing Windows/macOS/Linux CI
matrix; local measurements do not establish execution evidence on other platforms.
