# Runtime configuration updates

`ClientHandle::try_configuration_update(RuntimeConfigUpdate)` and the C
`rumqttc_client_update_configuration_tracked()` API stage owned partial updates
after startup. The C capability is `RUMQTTC_CAP_RUNTIME_CONFIGURATION`, available
in every build. Protocol, endpoint, transport kind and session identity stay fixed.
Startup `ClientConfig` / `rumqttc_config_t` handles remain independent of clients.

## Applicability audit

Both native event loops read request/read batching and pending-replay throttle
from options during polling. These fields do not resize queues, replace protocol
state or change packet-ID ownership. Connection inputs are read when establishing
a connection; replacing them during an attempt would violate profile coherence.
The wrapper applies only the following audited native setters between completed
polls, keeping an outstanding poll pinned across control wakeups.

| Input | Activation | Clear/default | Native applicability |
| --- | --- | --- | --- |
| `max_request_batch` | Next safe poll boundary | Zero: one request per poll | Read for each native request batch |
| `read_batch_size` | Next safe poll boundary | Zero: native adaptive policy | Read for each batch; native clamp of 1–128 retained |
| `pending_throttle` | Next safe poll boundary | Zero duration | Native pending-replay pacing; no general publish rate guarantee |
| Broker username/password | Before the next origin attempt | Both absent | Native CONNECT auth; v4 requires a username with a password; v5 supports password alone |
| Broker TLS profile | Before the next origin attempt | Clearing rejected | Entire immutable profile on existing TLS/WSS; fresh backend state and client session cache |
| Network settings | Before the next origin attempt | Complete default `NetworkConfig` | Buffer sizes, TCP nodelay, local bind, device and MPTCP; platform validation retained; newly created sockets only |
| Connection timeout | Before the next origin attempt | Five seconds | v4 network timeout and v5 total connect deadline; nonzero whole seconds, C setter takes milliseconds |

The network value is an aggregate replacement: absent optional fields restore
their defaults. Custom connectors retain their startup socket-policy contract;
their existing request contains the newly selected network settings. Broker TLS
replacement never changes independent HTTPS-proxy or redirected-target profiles.

The remaining native setter families were reviewed and excluded from this API:

| Setter family | Classification and reason |
| --- | --- |
| Client ID, clean session/start, session mode/expiry, store owner/scope, broker-session compatibility | Session reset or new client; persistent recovery and checkpoint keys must remain coherent |
| Transport kind/endpoint, proxy, Unix socket path, connector, SRV resolver, redirect policy | Construction-only in this stage; connector/callback ownership and redirect saved options require separate transitions |
| Keepalive, Will and MQTT 5 CONNECT properties (receive/packet/alias maxima, metadata and authentication method/data) | Construction-only in this stage; native derived state and broker-negotiated semantics require separate audits |
| Request/channel capacity, inflight and packet-size limits, publish budget/policy, ACK mode, topic aliases | Construction-only in this stage; live queues, protocol state and delivery tracking cannot be replaced safely by an options assignment |
| Enhanced-authentication manager, synchronous/asynchronous authenticators | Deferred; active exchanges and cached AUTH state require generation-aware replacement |
| WebSocket request modifiers/handshake authority | Construction-only registration; existing callbacks already run per attempt and can refresh tokens |
| Connection deadline | Internal native attempt metadata, never a host override |

See [PARITY.md](PARITY.md) for the native setter inventory. A new client is
required for excluded fields; there is no arbitrary live `MqttOptions` mutation.
TLS profile hooks and identity registrations are part of the copied TLS profile,
with their existing ownership, deadline and stale-response rules.

## Admission, staging and activation

Every field distinguishes unchanged, replace and clear. Credentials form one
coherent value; replacing only a password requires supplying the intended
username presence too. Updates are serialized in admission order under the
client's lifecycle gate. Concurrent callers have no ordering guarantee before
that gate. Each proposal merges against the latest committed desired values.
An empty update fails admission. Unsupported or invalid fields reject the whole
proposal, including other valid fields in the same update.

Admission copies/retains inputs into a separate bounded control queue: at most
16 preparing/queued updates, 1 MiB of declarative input per update and 4 MiB in
total. Full count/byte capacity returns backpressure. Size accounting includes
credentials, TLS roots/identity catalogs, ALPN, pins and network strings,
including copied vector-entry storage. Opaque
application-owned callback data and parsed TLS-library allocations have independent
sizes. Each client processes one proposal at a time. Connection profiles are
prepared on a tracked blocking worker; tuning-only proposals stage directly on
the driver. Joining a client includes profile preparation work.

When native shutdown finishes, configuration admission closes and queued/preparing
updates are discarded before graceful or ordered shutdown drains completions.
Updates that have not staged complete with a shutdown error; unactivated staged
receipts become `ClosedBeforeActivation`. Detached preparation stays tracked
through owner cleanup, and client join still waits for it to finish.

Validation and TLS preparation happen before commit. Failures consume no revision
and retain the previous desired/effective values. A successful commit increments
one monotonic revision and returns `Completion::ConfigurationStaged(receipt)` /
`RUMQTTC_COMPLETION_CONFIGURATION_STAGED`. This proves driver staging. It does
not prove activation, connection establishment or peer authentication. Destroying
or timing out a completion observer does not cancel an admitted update.

Tuning and connection groups have separate desired and effective revisions. A
mixed update can activate tuning while credentials/TLS remain staged. A newer
revision touching a group supersedes its earlier unactivated receipt and releases
obsolete staged inputs; unchanged fields in that group are still merged forward.
An already activated receipt remains activated. Retained receipts contain only
revision/status and never keep a client or secret owner alive.

Tuning activates only once the existing native poll finishes. A keepalive-disabled
idle connection can delay this indefinitely. Staging does not start/cancel an
attempt or wake its network I/O. Event backpressure can also delay staging and
activation; the existing event-delivery timeout/overflow policy still applies.
Control work yields cooperatively on shared execution; there is no hard latency
guarantee. Controlled reconnect is a separate future extension.

An attempt already in progress keeps its original credentials, TLS and network
settings. Before the next origin attempt, the wrapper selects the latest complete
prepared profile using narrow setters, preserving native cleanup/replay and session
state. Once selected, it persists across failures and retries with no fallback to
retired credentials or trust. Authentication and terminal-error policies retain
their existing behavior. Origin connection errors retain `configuration_revision()`
and C `rumqttc_error_configuration_revision()` independently of later retries;
redirected attempts and SRV lookup failures have no origin revision. Failure
attribution follows the failing connection phase independently of last-attempt
history; restoring origin options does not attribute an earlier redirect failure
to that origin profile.

## Redirects, security and observations

MQTT 5 rejects connection-group proposals during a redirect transition and while
using temporary/permanent targets, including mixed tuning/profile proposals.
Tuning-only proposals remain available. An origin rotation staged before a
temporary redirect remains staged while using the isolated target, then applies
before the restored origin attempt. Target credentials never inherit the rotation.
Only after a successful attempt on the permanent target does an unactivated origin
receipt become `UnavailableAfterRedirect` and release its prepared profile and
owned origin declarations. Only redacted origin-profile summaries and independent
tuning are retained; tuning-only updates remain available on the target. Receipt
retirement and origin-owner release precede event delivery, so a blocked or
failed event send cannot delay retirement or change its terminal status. Changing
the route alone does not retire the profile. Failed or cancelled moves leave the
receipt staged until activation, supersession or client termination; termination
marks it `ClosedBeforeActivation`. Snapshots continue to identify origin-profile values
and report the actual route separately. They do not describe the target's profile.

Every prepared connection profile builds fresh TLS backend state, including a
fresh Rustls session cache. A selected rotation cannot resume an old security
identity or bypass replacement trust. Normal retries can use that selected
profile's own resumption policy. Existing pin/verifier policies that disable
resumption continue to do so. Native TLS capability restrictions remain enforced.

Wrapper-owned broker password and TLS secret buffers use zeroizing storage.
Native `Bytes` password clones share their zeroizing owner, erased after the final
owner releases it. Active native options, redirect saved options, handshakes and
streams can retain older owners until their work ends. Custom connector closures
retain only their connector registration and request metadata, so they do not
extend retired credential or TLS profile lifetimes. TLS-library/wire buffers
and application-owned copies have independent allocation and erasure lifetimes;
this API cannot promise their erasure. Obsolete staged profiles and declarations
are released without holding the admission gate. Existing callback generations
reject stale responses after cancellation.

Preparation work remains tracked through destruction of its captured inputs and
any discarded prepared profile. Client teardown, including shared-execution
join/close, waits for those owner-release callbacks to finish. Accepted profiles
transfer into the driver's ordinary connection ownership.

`configuration_snapshot()` and C owned snapshots expose only redacted presence,
TLS backend/identity/pin counts, configured tuning and network settings. No
credential, key, certificate, pin digest or mutable native object is returned.
The snapshot reports the latest attempt number, captured origin revision, route,
pending/succeeded/failed/cancelled outcome and most recent successful connection
revision. A redirected success has no origin revision; a failed attempt does not
replace the last successful revision. Activation means local selection, even if
the peer later rejects the connection.

Effective tuning/read-batch values are cached when tuning is first applied,
changed or reasserted after redirect restoration. Native adaptive read batching
may subsequently change; this is a dated sample, not a live measurement. C status
reports monotonic ages for the effective sample, connection observation and owned
snapshot. Snapshots can age after capture. Receipts and snapshots survive client
destruction; staged receipts become `ClosedBeforeActivation` on termination.

The [C example](../c/examples/configuration_rotation.c) stages coherent credentials
and tuning. Set `RUMQTTC_AWAIT_RECONNECT=1` to await a broker/network-triggered
reconnect; provide `RUMQTTC_USERNAME`, `RUMQTTC_PASSWORD` and
`RUMQTTC_NEXT_PASSWORD` through the application's secret source. It never logs
secret values. Its native fixture verifies both CONNECT packets for v4/v5.
