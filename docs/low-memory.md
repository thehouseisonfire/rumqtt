# Low-memory client acceptance profile

This repository continuously tests one deliberately narrow client configuration
under a kernel-enforced 10 MiB process-memory budget. Both the MQTT 3.1.1
(`rumqttc-v4-next`) and MQTT 5 (`rumqttc-v5-next`) clients pass the profile
described below.

This is an acceptance test, not a throughput benchmark or a claim about every
way the clients can be built.

## Reproduce the result

Run from the repository root on an x86_64 Linux host with Docker and cgroup v2:

```bash
scripts/test-low-memory.sh
```

The command builds both client images, starts a separate Mosquitto broker, runs
each client, prints its result and peak, and removes its containers and temporary
Docker network. Detailed logs and cgroup counters are written below
`target/low-memory/results/`.

The runner refuses to produce a pass when it cannot verify cgroup v2,
`memory.max`, `memory.swap.max`, or `memory.peak`. An alternate limit can be
used for diagnosis, without changing the official result:

```bash
scripts/test-low-memory.sh --memory-mib 12
```

## Exact profile

| Setting | Value |
| --- | --- |
| Host and architecture | Linux, x86_64 |
| Rust toolchain and target | Rust 1.85.0, `x86_64-unknown-linux-musl` |
| Build | Workspace release profile, locked dependencies, LTO, stripped |
| Client features | `default-features = false`; no optional features |
| Runtime | Tokio current-thread |
| Transport | Plain TCP |
| Broker | Mosquitto 2.0.22 in a separate, unconstrained container |
| Measured container | `scratch`; the statically linked client binary is PID 1 |
| Connections and subscriptions | One connection and one unique QoS 1 subscription |
| Session | Clean session (v4) / clean start (v5) |
| Request channel | Four entries |
| Outgoing inflight limit | Four QoS 1 publishes |
| Request and read batching | At most four packets per batch |
| Packet limit | 1,024 bytes locally for v4 incoming/outgoing and v5 incoming; the broker also enforces 1,024 bytes and advertises that outgoing ceiling to v5 |
| Payload | 128 deterministic bytes |
| Socket buffers | 4,096-byte send and receive sizes requested from Linux |
| Keep-alive | Three seconds; both `PINGREQ` and `PINGRESP` must be observed |
| Connection timeout | Five seconds |
| Initial exchange | 32 QoS 1 self-published messages in four-message waves |
| Connection loss | The external broker is restarted after a phase marker |
| Reconnect exchange | 100 ms failed-attempt backoff, automatic reconnect, resubscribe, then eight QoS 1 messages |
| Per-phase timeout | 15 seconds |
| Completion | Graceful MQTT disconnect and exit status zero |

Every outgoing QoS 1 publish must produce a `PUBACK`, and every echoed publish
must have the expected topic and byte-for-byte deterministic payload. Missing or
unexpected acknowledgements, duplicates, payload errors, premature connection
loss, missing reconnection, or timeouts fail the scenario.

## Meaning of the limit

The exact hard limit is `10 * 1024 * 1024 = 10,485,760` bytes. Docker is invoked
with equivalent memory and memory-swap values, resulting in:

```text
memory.max=10485760
memory.swap.max=0
```

The measured cgroup contains only the static client executable. Its accounted
memory includes the allocator, Tokio runtime, MQTT state, executable pages,
thread stack, socket memory charged to the cgroup, and other kernel-accounted
process memory. The broker, Docker daemon, and host-side measurement script are
outside that cgroup. The runner reads the kernel's monotonic `memory.peak`
counter rather than inferring compliance from sampled RSS. It also records
`memory.current`, `memory.events`, Docker's `OOMKilled` state, process exit
status, and duration.

The constrained process has no swap allowance. A run is a memory failure if the
kernel reports an OOM event, Docker reports an OOM kill, the process exits from
`SIGKILL`, or the recorded peak exceeds the requested limit.

## Measured result

The following strict run was recorded on 2026-07-29:

| Client | Result | `memory.peak` | OOM-killed | Exit | Duration |
| --- | --- | ---: | --- | ---: | ---: |
| MQTT 3.1.1 / v4 | Pass | 8,318,976 bytes (7.93 MiB) | No | 0 | 4,991 ms |
| MQTT 5 / v5 | Pass | 8,257,536 bytes (7.88 MiB) | No | 0 | 5,235 ms |

Both cgroups reported zero `oom`, `oom_kill`, and `memory.max` events. The
measurement host used Linux 7.1.5 on x86_64, Docker 29.6.2 with the systemd
cgroup driver and cgroup v2, the pinned Rust 1.85.0 Alpine builder image, static
musl 1.2.5 linking, and the pinned Mosquitto 2.0.22 image. The relevant output
was:

```text
Running clients with memory.max=10485760 bytes and memory.swap.max=0...
v4  PASS  peak=8318976 bytes oom_killed=false exit=0 duration=4991ms
v5  PASS  peak=8257536 bytes oom_killed=false exit=0 duration=5235ms
```

## Limits of the claim

This result supports only the documented Linux/x86_64 static-musl, plain-TCP,
no-optional-feature configuration and workload. It does not prove that TLS,
WebSockets, proxies, tracing, other allocators, other operating systems or
architectures, arbitrary application queue sizes, larger packets, heavier
workloads, the dynamically linked GNU target, or an entire physical device fit
within 10 MiB. Broker memory and application components outside this client
process are not included.
