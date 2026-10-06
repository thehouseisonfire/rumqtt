# rumqttc-wrapper-core

Private Rust infrastructure shared by native rumqtt wrappers. The crate owns
the MQTT event loop on dedicated or explicitly shared execution and exposes owned, protocol-neutral
configuration, commands, completions, events, diagnostics, and errors.

## Execution ownership

`NativeClient::start(config)` preserves dedicated-thread startup.
`ExecutionContext::new(ExecutionOptions::default())` constructs an explicit
library-owned Tokio runtime; `NativeClient::start_in(config, &context)` selects
it for one client. Both use the same driver future and terminal reconciliation.
Client capacity covers starts under construction and drivers awaiting auxiliary
cleanup. Closing/joining a client leaves its peers and context workers running.

Context clones, client owners and closers retain execution. Dropping a reference
requests no shutdown while other owners remain. Final-owner release requests
cleanup without waiting; retain a context observer to prove teardown explicitly.
`request_shutdown()` coalesces immediate client cleanup and closes admission.
Use ordinary graceful client close first if MQTT DISCONNECT is required.
`join(timeout)` requires shutdown; timeout ends only observation, so join can
be retried. `try_join()` returns false while pending. `Quiescent` appears only
after runtime destruction and the management thread has been joined.

Defaults are 1,024 clients, two scheduler workers and at most 32 blocking workers,
plus one management thread. The blocking queue is not bounded by the worker
limit; DNS and deferred TLS may create auxiliary work. Client joining includes
tracked deferred TLS work. Context joining additionally waits for all Tokio
blocking work and thread teardown. Host-retained tokens, streams, registrations,
configurations and host jobs have independent lifetimes; release those and join
host jobs before unloading code. Final-owner release alone proves no quiescence.

Nonzero synchronous completion, event, client-close/join and context waits are
rejected on context workers and in host callbacks, including waits for peers.
Polling and nonblocking admissions/shutdown remain available. Synchronous host
callbacks/destructors must return promptly; deadlines cannot preempt them.
Driver tasks may migrate between workers. Cooperative yields bound ready MQTT
and wrapper-control loops; there are no hard peer-latency guarantees.
See the [C example](../c/examples/shared_execution.c) and
[measurement method and results](benches/execution.md).

## Publish admission

MQTT 5 clients use strict negotiated-capability admission and finite publish
limits (1,024 outstanding / 16 MiB charged data) by default. See the
[publish admission and recovery contract](publish-admission.md) for policy
selection, accounting, completion and retry semantics.

## Protocol support contract

This crate supports both MQTT 3.1.1 and MQTT 5 through one shared API. Each
`NativeClient` instance explicitly selects exactly one protocol through its
`ClientConfig`, and that selection remains fixed for the client's lifetime. A
client does not negotiate, fall back to, or switch between protocol versions;
construct a new client to use another version.

Protocol-neutral commands and events share types only where their semantics
genuinely overlap. Observable protocol differences remain explicit:

- MQTT 3.1.1 uses `V4Config`, including `clean_session`;
- MQTT 5 uses `V5Config`, including `clean_start` and session expiry;
- MQTT 5 publish, subscribe, per-filter subscription, and unsubscribe extensions use explicit
  operation-specific protocol option enums; and
- broker reason information is present only where the selected protocol
  exposes it.

Native MQTT clients, event loops, acknowledgement values, packet translation,
and protocol-specific validation are confined to the private `backend` module.
The shared handle, lifecycle, admission, completion, diagnostics, event-delivery,
and shutdown machinery delegates through one enum-dispatched backend; it does
not use dynamic dispatch or duplicate the wrapper architecture per protocol.

Supplying an option that is incompatible with the selected protocol is an
error. The core must not silently discard MQTT 5 properties for an MQTT 3.1.1
client or invent a common interpretation for settings whose behavior differs.
`VersionNeutral` commands work with either protocol. Selecting a `V5` variant,
including a default-valued variant, requires an MQTT 5 client and is rejected
before request-channel admission on an MQTT 3.1.1 client. SUBSCRIBE packet
properties remain separate from each filter's No Local, Retain As Published,
and Retain Handling options; UNSUBSCRIBE User Properties remain scoped to the
UNSUBSCRIBE command.
This includes CONNECT authentication: MQTT 3.1.1 requires a username whenever
a password is present, while MQTT 5 permits username-only, password-only, and
combined credentials. Client identifiers and usernames are validated as MQTT
UTF-8 strings, and their encoded lengths—as well as the binary password
length—must fit the protocol's two-byte field before the driver starts.
MQTT 5 publish properties are directional in the Rust API.
`V5OutgoingPublishProperties` contains only properties legal on a
client-originated PUBLISH and therefore cannot represent a Subscription
Identifier. `V5IncomingPublishProperties` retains every observable property on
a broker-originated PUBLISH, including all received Subscription Identifiers.
Outgoing properties are validated before admission for payload format,
Response Topic syntax, MQTT UTF-8 strings, and two-byte binary/string lengths.
Publish Topic Names for both protocols reject U+0000 and values exceeding the
MQTT UTF-8 string length before admission.

The wrapper builds MQTT 5 clients with
`PublishAdmissionPolicy::RequireNegotiatedCapabilities`. `rumqttc-v5`, rather
than this wrapper, owns the coherent negotiated Maximum QoS, Retain Available,
Topic Alias Maximum, connection generation, and outgoing manual Topic Alias
mapping used by producer admission. Alias mapping changes are transactional with
request-channel admission and are invalidated under the same boundary as
event-loop reconnect cleanup. Replayed concrete-topic publishes lose their old
connection's alias, while an unrecoverable alias-only tracked publish completes
with `TopicAliasReplayUnavailable`; the wrapper neither mirrors this state nor
repairs rumqttc's request queue.
Broker publish capabilities are unknown before the first MQTT 5 CONNACK and
again while reconnecting. During those intervals, nonblocking admission of a
QoS 1/2, retained, or Topic-Alias publish reports transient backpressure without
admitting the packet; asynchronous admission waits and retries when CONNACK
installs the new connection's capabilities. Alias-free, non-retained QoS 0
publishes remain admissible because every MQTT 5 server supports that form.
Native language wrappers should normally ship one package supporting both
protocols and translate their explicit protocol selector into `ClientConfig`.

The request and event channels are bounded. Applications must continuously
consume events: if the event buffer remains full beyond
`CommonConfig::event_delivery_timeout`, the driver terminates with an explicit
backpressure error rather than dropping incoming publishes. Terminal status
uses an independent channel and remains observable when the ordinary event
buffer is full.
Wrapper control traffic and MQTT polling use fair arbitration, so sustained
diagnostics or completion traffic cannot indefinitely suppress network and
keep-alive progress. MQTT 5 processes only bounded registration and ready-result
batches before arbitration and yields after handling that work, preserving I/O,
authentication-deadline and shutdown progress during overlapping reauthentication.

Admission is distinct from MQTT completion. Dropping or timing out a
`CompletionHandle` never cancels work already admitted to rumqttc.
`CompletionHandle` is cloneable, and polling, blocking waits, and async waits
all borrow it and repeatably observe the same immutable terminal result. A
caller's wait deadline is only an observation outcome and is never cached as the
operation result. Immediate
shutdown interrupts an in-progress connection attempt and reports unfinished
admitted work as ambiguous; once connected, rumqttc observes its priority
disconnect at an event-loop scheduling point. Immediate shutdown remains a
persistent driver condition after its wake-up notification is consumed, so all
subsequent event deliveries bypass a full wrapper event buffer while the driver
closes. Graceful shutdown uses rumqttc's disconnect barrier and completes only
after the event loop closes.
For MQTT 5 QoS 2 recovery, a `PUBCOMP` with `PacketIdentifierNotFound` is a
successful terminal completion only when rumqttc identifies the corresponding
`PUBREL` as replayed; the same reason on an ordinary flow remains a broker
rejection.
`CompletionHandle::try_outcome()` returns an optional shared immutable
`TerminalOutcome` without propagating its operation error. `None` means pending;
a terminal outcome contains the legacy result and optional `BrokerAcknowledgement`.
Existing waits keep their result types and classification. MQTT 5 ACK details
retain exact codes, packet identifiers, Reason String presence, and ordered duplicate
User Properties, including on rejected publishes and recovered QoS 2 completions.
SUBACK/UNSUBACK properties belong to the packet; filter codes remain ordered.
MQTT 3.1.1 retains identifiers and SUBACK codes but has no scalar reasons or
MQTT 5 properties, and UNSUBACK has no filter results. QoS 0 and local failures
have no broker ACK. Successful QoS 2 PUBREC and intermediate PUBREL remain
internal: these outcomes expose terminal packets, not handshake history or exact
wire encodings. Snapshots and completion handles share the retained storage,
may outlive the client, and release it with their final owner. Debug and automatic
logs omit ACK property contents. C exposes these details; Python/JavaScript
continue exposing their existing coarse completion/error contracts.

In manual-acknowledgement mode, admission means the PUBACK or PUBREC entered
rumqttc's request channel, while `Completion::Acknowledged` is reported only
after the event loop flushes that packet to the network. Cancelling an
asynchronous acknowledgement while it is still waiting for request-channel
capacity restores its token for retry. Retransmissions of the same unacknowledged
incoming packet share one token; consuming it invalidates every copy, and a
retransmission received while its ACK is already queued needs no additional ACK
token. Reconnect processing advances the token generation and drains any late
connection-scoped ACK request under the same admission gate, preventing an ACK
that raced connection-loss cleanup from entering the replacement connection.

`Command::AcknowledgeWithOptions` accepts
`AcknowledgementProtocolOptions::V5(V5AcknowledgementOptions)` to choose legal
client-originated reason codes, an optional Reason String, and ordered duplicate
User Properties. Explicit V5 defaults are rejected on v4; `VersionNeutral` and
`Command::Acknowledge` retain default success. Server-only `0x10` is rejected.
Validation checks MQTT strings and the full encoded ACK against the current
generation's negotiated Maximum Packet Size before reserving the token or
allocating an operation. Failed admission restores the original default packet,
so retries cannot inherit previously attempted content.

Negative PUBREC releases incoming QoS 2 state and receive quota; its completion
still means local ACK flush. Negative acknowledgements terminate delivery and
do not request retry or shared-subscription reassignment. Diagnostic properties
target the broker, not the original publisher. Python and JavaScript currently
expose default ACK timing; the C wrapper also exposes ACK content.

Request admission and the transition to `Closing` share one ordering gate. A
request that wins that gate is admitted before the disconnect barrier; a
capacity-waiting async request that loses it wakes and returns `NotAdmitted`
instead of entering rumqttc after shutdown has begun. Capacity waiters are also
woken when the driver reaches `Closed` or `Failed`, so a terminal driver cannot
leave synchronous or asynchronous admission blocked on a channel that will never
make further progress. Graceful shutdown also resolves diagnostics admitted
before its barrier with the final cached driver snapshot; immediate shutdown can
still leave unfinished diagnostics ambiguous.

`NativeClientCloser` is the host-neutral close coordinator. Concurrent graceful
callers share one completion, successful repeated calls return the same graceful
outcome, immediate close can escalate an outstanding graceful close, and each
caller's timeout is one budget spanning completion observation and driver-thread
join. Immediate callers also retain their tracked completion and observe the
same shutdown failure on repeated calls. Joining alone reports thread teardown.
Finalizer cleanup remains a nonblocking immediate-shutdown signal.

## TLS policy

TLS configuration is shared by broker TLS/WSS, HTTPS proxies and redirect
transports. `TlsConfig` adds defaulted `version_policy` and `pins` fields;
exhaustive literals must be updated. `TlsVersionPolicy` selects backend defaults,
TLS 1.2-only, TLS 1.3-only or the combined allowed set.
`TlsRootPolicy::PlatformAndPem` augments platform trust with supplied roots.
`TlsBackend::capabilities()` reports enforceable version/root policies and pin
formats; disabled backends return empty masks. `TlsConfig::validate()` constructs
temporary TLS resources without networking, and client startup consults platform
trust again. All three TLS layers use the same validation and construction.

Rustls pins are SHA-256 digests of leaf certificate DER or complete SPKI DER.
Any configured pin may match, up to `MAX_TLS_PINS` (32), after normal certificate
and hostname validation. Handshake signatures remain verified. Pinned profiles
disable resumption and revalidate on every reconnect. Native TLS rejects pins;
unsupported policies always fail. Enable the opt-in `tls12` feature for Rustls
TLS 1.2-only policy support; otherwise the combined allowed set uses TLS 1.3.
Native TLS enforcement follows the platform limitations documented in
[the C profile API](../c/README.md#owned-tls-profiles). No process-global provider
is installed or changed, and separate TLS layers never merge their policies.

`TlsConfig` also owns ordered cipher IDs, `TlsSniPolicy`,
`TlsResumptionPolicy`, an optional `TlsVerifierConfig` and
`TlsClientIdentity::External(TlsExternalIdentityConfig)`. Cipher selection and
callbacks require Rustls; native TLS supports SNI policy. External catalogs
contain public certificate PEM, opaque key IDs and ordered signature schemes;
`TlsIdentityProvider` selects an entry and signs the exact unhashed message.
Returned signatures are checked against that entry's leaf key. `TlsVerifier`
supplements standard authentication and pins. Neither hook runs at validation.
These hooks are synchronous, bounded and retained with `Arc`; they may run concurrently
across clients and must return promptly without waiting for their driver.
`TlsCallbackFailure` records stage, reason and connection layer with fixed
redacted diagnostics. Only timeout/transient callback failures retry. A fresh
handshake guard prevents failed optional client authentication from exposing an
anonymous stream. Pins and hooks disable resumption; connection deadlines remain
absolute across TCP, proxy, TLS and WebSocket setup. See the
[C contract](../c/README.md#owned-tls-profiles) for algorithms and resource limits.

For deferred answers, set `TlsConfig::async_verifier` to `AsyncTlsVerifierConfig`
or use `TlsClientIdentity::ExternalAsync(AsyncTlsExternalIdentityConfig)`.
`AsyncTlsVerifier` and `AsyncTlsIdentityProvider` receive owned, redacted request
snapshots and return `TlsCallbackFuture<T>`. They may return immediately or await
remote services. Synchronous and asynchronous verifiers are mutually exclusive;
selection/signing share one fixed, validated catalog. Native TLS rejects these
hooks eagerly. All hooks preserve standard authentication and pins, validate
returned signatures and disable resumption.

Deferred profiles drive Rustls on one cancellable worker per active handshake.
The driver constructs/polls/drops host futures, including synchronous TLS hooks
in mixed profiles; synchronous-only profiles keep their existing path. The
original connection deadline covers queued and pending work. Dropping a future
cancels observation; detached host work must own its data and ignore late answers.
Close and timeout wake worker callback and network waits before runtime teardown.
Panics during future construction, polling or destruction become sanitized
terminal `TlsCallbackReason::Panic` failures. Methods and future polls/destructors
must return or yield promptly and cannot wait for their own driver's MQTT work.
Destruction failures during cancellation remain visible to the driver and fail
shutdown and pending operations; they override retryable timeout failures.
If graceful close expires, the close operation retains its timeout result, while
a TLS destruction failure makes the driver and pending operations fail with the
typed callback error instead of reporting completed immediate shutdown.
Failure tracking belongs to each client, including its proxy and redirect TLS
layers, so shared profiles cannot transfer these failures between clients.
The C binding supplies retained completion tokens and bounded operation ownership.


## Admission modes and host threads

`ClientHandle::try_admit` is nonblocking and reports request-channel
backpressure immediately. `ClientHandle::admit_async` waits asynchronously for
capacity and is the normal choice when integrating with a host-language future
or promise. `ClientHandle::admit` is an explicitly blocking convenience API.

Never call `ClientHandle::admit` from a JavaScript event-loop thread, a Python
async-executor thread, or another latency-sensitive async thread. Doing so can
prevent the host runtime from processing unrelated work while the bounded MQTT
request channel is full. C APIs and other synchronous wrappers may expose it as
an explicitly blocking operation; async wrappers should use `admit_async` or
move the blocking call to a wrapper-owned worker thread. Use `try_admit` when
the host must remain nonblocking and prefers an immediate backpressure result.

This crate has no Python, JavaScript, C ABI, serialization, or host-runtime
dependencies and does not define a stable foreign ABI.

`Error::context()` exposes protocol, connection phase, successful connection
generation, and operation ID when available. Before the first connection the
generation is absent; reconnect-attempt errors retain the last established
generation. Completion errors carry the operation's admission context unless a
more specific failure context is available. Authentication callback generations
count initial authentication attempts separately, including failed attempts.

An accepted SRV redirect first reports a `Redirect` event without a target.
After resolution and successful connection, a second `Redirect` supplies the
selected endpoint immediately before `Connected`. No old endpoint is reported
as though it were the unresolved target.

## Extended configuration and feature selection

`CommonConfig::broker` is a `BrokerTarget::{Tcp, Unix, WebSocket}` and must match
`TransportConfig`. `IncomingPacketLimit::{Default, Bytes, Unlimited}` controls
only the local decoder. The default is 10 KiB; MQTT 5 separately advertises 10 KiB
by default in `V5Config::connect_properties.maximum_packet_size`. Set that field
to `None` to omit the CONNECT property. Batching and retransmission throttle do
not change the wrapper's bounded request/event capacities.

Wills use `LastWillProtocolOptions`, not PUBLISH properties. MQTT 5 CONNECT
properties preserve absent versus present zero/empty values and ordered repeated
User Properties. `TopicAliasPolicy` selects the native automatic alias policy;
the native negotiated-capability admission gate remains authoritative.

Default features are `use-rustls` (AWS-LC) and `websocket`. Select Ring with
`--no-default-features --features use-rustls-ring`, or supply a process default
Rustls crypto provider with `use-rustls-no-provider`. `use-rustls-aws-lc` selects
AWS-LC explicitly; Ring and AWS-LC cannot be enabled together. Optional features
are `use-native-tls`, `tls12` (Rustls TLS 1.2), `http-proxy`, `socks-proxy`, `proxy` (both proxy features),
`system-srv-resolver`, `auth-scram`, `tracing`, and `tracing-log-compat`.
Both TLS backends may be built together: `TlsBackend` always selects one
explicitly. Without defaults, TCP and Unix remain available. Public value types
remain available for disabled features and validation rejects their use.

`TlsRootPolicy::Platform` uses platform roots; `Pem` **replaces**, rather than
augments, those roots. `TlsClientIdentity::RustlsPem` and `NativePkcs12` require
their respective backend. Native TLS ALPN requires UTF-8 identifiers; all ALPN
identifiers contain 1–255 bytes. `SecretBytes` wipes each owned allocation on
drop; TLS libraries control their own parsed-key storage. Migrate old PEM fields
with `TlsConfig::rustls_pem(ca, certificate, private_key_vec)`.

HTTP CONNECT (including TLS to the proxy) and SOCKS5 compose with TCP, TLS, WS,
and WSS. SOCKS4 is intentionally unavailable because neither native client
implements it. Native SOCKS5 resolves hostname targets remotely; supply an IP
literal for application-resolved DNS. Proxy credentials, broker credentials,
and the two TLS policies are independent.

WebSocket header edits are ordered append/replace/remove operations. Upgrade,
Host, framing, and `Sec-WebSocket-*` headers are protected. Values are redacted
in Debug and marked sensitive in the prepared request. Declarative validation
can reject construction before networking.
Unix sockets are supported independently on Unix targets.

Set `CommonConfig::websocket_handshake` to an owned `WebSocketHandshakeConfig`
for asynchronous token refresh or signing. `WebSocketHandshake::prepare` receives
an owned snapshot after static header edits and returns an owned response patch.
It runs once per prepared handshake, including reconnects. Header names are
sorted; duplicate values preserve their order within a name. Values are bytes,
so absence, an empty value, and non-UTF-8 values remain distinct. This snapshot
does not promise global wire header ordering.

Responses can replace the path/query and append, replace, or remove unprotected
headers. `WebSocketHandshakeResponse::set_authority()` accepts `host[:port]` or
`[IPv6][:port]` without user information and updates the URI authority and `Host`
together. Invalid edits leave the previous override intact; generic `Host` edits
remain prohibited. GET, HTTP/1.1, the URI scheme, and upgrade fields stay fixed.
Sign the resulting authority, path/query, and selected headers as required by
your service. The configured
broker, dial target (possibly a proxy), TLS authority, and absolute connection
deadline are separate snapshot fields. Authority overrides only select HTTP
routing; they never change broker resolution, TCP or proxy destinations, TLS SNI,
or the certificate verification identity.
An isolated redirect clears both static edits and the dynamic authority.

Callbacks must return promptly; their future may defer credential retrieval.
Do not wait for MQTT operations from this callback. Construction, polling, and
future destruction panics become redacted terminal failures. The native
connection deadline also bounds callback work and cancels late results.
Rejected, abandoned, and timed-out decisions follow the existing reconnect
policy; invalid responses, resource limits, and panics terminate the driver.
`Error::websocket_failure()` retains this classification.

Requests allow at most 128 header values, an 8 KiB path/query, and 64 KiB of
aggregate request data. Response builders allow 256 edits and 64 KiB of copied
data; the final request must also fit the request limits. Wrapper Debug/error
output omits handshake credentials. Upstream WebSocket dependency TRACE logging
can include outgoing requests; configure those log targets accordingly.
Registration requires WebSocket support and a WS/WSS transport.

## Custom transports

Set `CommonConfig::connector` to an owned `TransportConnectorConfig`. Its
`TransportConnector::connect` returns a `TransportConnection` containing an
`Arc<dyn TransportIo>`, the configured mode, and explicit `NetworkHandling`.
Each attempt supplies owned target/client-ID strings, protocol, a client-local
socket-attempt generation, requested network settings, and the exact native
absolute deadline. The target is the proxy endpoint when a proxy is enabled;
the deadline includes subsequent proxy/TLS/WebSocket/MQTT negotiation.

`TransportMode::Base` supplies bytes before native proxy, TLS and WebSocket
layers. `Established` supplies MQTT-ready bytes (including any host-managed
security/tunnel) and requires TCP with no native proxy or layered redirect
profile. Custom connectors cannot be combined with Unix broker targets.
Reconnect calls the connector again; return a fresh stream each time.
`NetworkHandling::Applied` confirms the host applied all requested settings;
`NotApplicable` is accepted only for default network settings. Unsupported
settings must fail explicitly rather than being silently ignored.

`TransportIo` returns owned, sendable futures for read/write/flush/shutdown.
Reads return owned `Bytes`; writes receive owned `Bytes`. Transfers are bounded
at 16 KiB, one read can overlap the serialized writer, and short transfers are
normal. A successful empty read is permanent EOF; pending work stays pending.
Buffered writes report acceptance immediately. Subsequent writes, reads and
flushes advance them and observe errors; shutdown drains writes and flushes.
Host flush must actually flush accepted writes. The native TLS bridge handles
pending flushes through its BIO read boundary and waits for the real flush
before handshake completion or exposing outer flush/shutdown success.
During native TLS handshakes, original stream errors are retained independently
of the platform TLS error source, preserving transport classifications on Windows
as well as Linux and macOS.

Callbacks and future polls must return/yield promptly. No lifecycle/admission
lock is held during a callback. Dropping a future cancels observation; detached
work must retain its owners and buffers until released. Discard a cancelled
stream, never reuse it. Destructors must neither block nor panic. Invocation,
polling and future-destruction panics are contained as `TransportFailure::Panic`. Other failures
have fixed typed classifications; arbitrary host diagnostic strings are not
exposed through wrapper errors. See the C README for retained-operation bounds.

`Connect`, `Io`, `Timeout` and `Abandoned` failures allow reconnection. All other
transport failures terminate the driver with `DriverTerminated` and transition
the client to `Failed`. Pending operations and first-connection observers receive
the typed terminal error, and further admission is rejected.
During an MQTT 5 SRV redirect, terminal failures also stop candidate fallback
immediately. Retryable candidate failures can try the next endpoint. A failed
redirect is terminal and its error retains both `redirect_failure()` and any
underlying `transport_failure()`, including failures during TLS setup.

`tests/custom_transport_memory.rs` covers composition/reconnect for both
protocols and enabled TLS/proxy/WebSocket backends without sockets.
`tests/custom_transport_redirect.rs` covers redirect failure classification,
SRV fallback policy, pending operations and connection observers without sockets.
`tests/custom_transport.rs` and `tests/transport_composition.rs` additionally
cover real socket providers; the C examples include a transparent byte tunnel.

## Persistence and callback ownership

`SessionStoreConfig` owns an asynchronous `SessionStore` and stable tenant/store
scope. Its versioned checkpoint envelope rejects wrong protocols, unsupported
versions, corrupt data, and oversized checkpoints. Patch releases preserve the
format; minor changes may require explicit migration/invalidation. Load, save,
and clear failures terminate the driver with `StoreFailure`; data is never
silently erased to recover from a decode error. Native Rust stores can be used
through the separate `rust_session_store` module, not the host-neutral contract.

Persistent v4 requires a nonempty client identifier and `clean_session=false`.
Persistent v5 requires a nonempty identifier, `clean_start=false`, and nonzero
session expiry. Strict broker resume is the default; `AllowBrokerOnly` is an
explicit v5 choice. One active client may own each `(store owner, protocol,
scope, client id)` key; cross-process fencing remains the store's responsibility.
Checkpoints are protocol recovery state, **not** an application outbox.

Store and SRV callbacks return owned, sendable futures and must yield, not block.
Authenticator callbacks are synchronous because the native authenticator API
requires immediate responses; prepare credentials before start and never wait
for MQTT completion inside a callback. Callbacks run without wrapper lifecycle
locks, with panic containment and deadlines. Dropping a future cancels observation;
detached work must own its data and ignore late completion. A cancelled store
write may have committed and must be crash-consistent. See each trait's rustdoc
for serialization, reentrancy, teardown, and destructor requirements.

`V5Config::authenticator` is the sole challenge authority. Events are observations,
not competing response tokens. Alternatively, set `scram` and CONNECT method
`SCRAM-SHA-256`; each client gets its own mechanism. SCRAM verifies the server
signature and bounds server-selected iterations to prevent unbounded work.
Use authenticated TLS; channel-binding SCRAM variants are not exposed.
Tracked `Reauthenticate` resolves separately from admission and is not cancelled
by dropping its observer.
It requires a configured authenticator or SCRAM owner, so every admitted exchange
has a challenge authority and a bounded deadline. Reauthentication properties
must come from that owner; commands containing caller-supplied properties are
rejected at admission. Static initial CONNECT
Method/Data without an authenticator remains available for one-step initial auth.

Redirects are rejected by default. `Follow` sets a finite attempt limit and an
explicit target transport. It uses native isolated-target policy: a fresh client
identity and no inherited broker credentials, auth mechanism, session store,
proxy, or header edits. Redirect TLS credentials are supplied explicitly. A
custom `SrvResolver` overrides system discovery and uses the native lookup
timeout. Each target connection has its own connection deadline. An accepted SRV redirect initially has no selected endpoint while DNS
is unresolved; it never reports the previous endpoint as the selected target.


`RedirectPolicy::Application(RedirectAuthorityConfig)` instead calls an owned
synchronous `RedirectAuthority` once per validated redirect. `RedirectRequest`
contains source, reason, one-based attempt, effective client ID/store scope and
all advertised references in order. `RedirectResponse::follow(request, index,
target)` binds a choice to that exact request. Requests may be retained for
inspection; responses cannot be deferred. Fixed and application policies replace
each other, and rejection remains the default.

`RedirectTargetConfig` starts isolated, with a fresh ID and no credentials.
Choose `RedirectClientId::{Fresh, Reuse, Replace}` independently of
`RedirectSession`. Supply target username/password explicitly; no origin CONNECT
credentials are inherited. `reuse_authentication_authority` retains the current
sync/async authority **and** CONNECT authentication method/data; it does not
install a new authority or reuse origin username/password. Auth contexts report
the effective target ID, including broker assignment. `reuse_network_credentials`
retains the native proxy configuration and WebSocket modifiers together, including
static edits/dynamic authority. These flags are independent; selective proxy or
header reuse is unavailable. Target TLS credentials always come from the selected
transport profile. For SRV, approve a reference and leave candidate resolution,
weighted selection and fallback to the native client.

Session reuse is explicit: `RedirectSession::Reuse { store_scope }` with reused
ID and unchanged scope preserves live native state. Changing either key resets
live state and may load that target key's checkpoint. This neither migrates
checkpoints nor proves that brokers share a session. Stores require nonempty keys,
`clean_start=false` and nonzero session expiry. A per-driver factory acquires the
exact target key before applying a profile; conflicts fail with `StoreInUse`
without target I/O. Same-key adapters share a lease. Native options retain the
origin lease until restoration or permanent establishment; obsolete intermediate
leases and all teardown leases are released. Load/save/clear reject any key not
covered by the adapter's lease (`StoreFailure::KeyMismatch`). Isolated hops clear
the store and cannot resurrect it on a later hop. Strict Session Present checks,
packet-ID reconciliation, manual-ACK and alias connection generations remain
native responsibilities.

The decision timeout covers callback invocation and profile preparation. Late
results fail before application; cancellation is checked after callback return.
It cannot preempt a synchronous callback or bound a stalled driver's shutdown.
Callbacks/destructors must return promptly, be thread-safe across clients, and
must not wait for driver work or destroy/join their active client. Nonblocking
admission is supported; no wrapper admission/lifecycle lock is held during the
callback. Rust panics, callback errors, invalid/stale responses, timeouts and
resource limits have redacted typed `RedirectDecisionFailure` values. Snapshots
allow at most 256 references and 64 KiB of copied text; response fields allow
256 KiB total and MQTT wire-length limits. Selected advertised references are
reported separately from resolved endpoints. There is no chain-wide deadline or
replacement enhanced-authentication authority in this API.

## Close payloads and richer observations

The original close commands remain version-neutral. The `WithOptions` variants
and closer methods accept `DisconnectProtocolOptions::V5`. The first successfully
admitted payload wins, including during escalation and after close. Matching
closer calls coalesce; different payloads fail with `NotAdmitted`. VersionNeutral
and an explicit V5 default are distinct payload choices. Individual wait deadlines
do not replace the first operation's timeout or cancel its work.

`Connected.details`, `ConnectionRejected`, and `BrokerDisconnect` own legal
packet properties, including repeated User Properties. Authentication and redirect
events are observations; tracked ACK completions remain the only operation-result
model. `OutgoingEvent` adds the native packet identifier where available. All
events still consume bounded queue slots and require continuous consumption;
large properties increase memory per slot, bounded by the local packet limit
unless callers explicitly select `Unlimited`. Debug/error/tracing output omits
credential material; applications must likewise avoid logging raw property data.

## Session Present compatibility

The MQTT 3.1.1 default rejects a successful clean-session CONNACK with invalid
Session Present=true. The opt-in `AcceptAsClean` policy resolves only this case
as fresh, resets old local protocol state and replay work, and clears the
current scope/client-ID checkpoint before reporting success. Failed or cancelled
clearing remains pending for retry before checkpoint loading. The broker remains
non-conforming and raw connected-event/connection-result flags remain true.

Existing diagnostics expose an optional `connack` observation containing raw
Session Present, effective session resume, and a compatibility reason. Use the
effective session for resubscription decisions. No MQTT 5 clean-start recovery
is introduced; the existing broker-only resume policy retains its restrictions.

Configure `V4Config::session_present_mismatch_policy` with
`SessionPresentMismatchPolicy::{Error, AcceptAsClean}`. `Error` is the default.
`DiagnosticsSnapshot::connack` includes `raw_session_present`, `session_resumed`,
and `ConnAckDiagnostic`. Existing `V5Config::broker_session_resume_policy`
remains flat and maps into the native client's `ProtocolCompatibility`.
The added defaulted Rust fields require updates to exhaustive struct literals;
wrapper-core is private infrastructure and has no stable Rust API promise.

## Optional ordered publish shutdown

The disabled-by-default `ordered-shutdown` feature forwards both native features.
`Command::OrderedDisconnect` and `OrderedDisconnectWithOptions` install the native
fence; `Completion::OrderedShutdown` comes only from its terminal notice.
`NativeClientCloser::close_after_queued*` shares matching admitted fences and waits
for execution-owner joining within a caller budget. Ordinary graceful close keeps
its protocol-state drain contract, including when this feature is enabled.

The first admitted fence owns its absolute deadline and owned DISCONNECT payload.
Subsequent raw fences and conflicting close policies fail; matching closers retain
the result and deadline. Native-supported persistent reconnect remains active.
Timeout resolves the operation while the driver continues required terminal
storage cleanup. Observer cancellation does not abort; explicit immediate close
or destruction aborts and supersedes an unresolved fence. Retained completions do
not retain the client and remain readable after destruction.

The optional boxed `DiagnosticsSnapshot::ordered_shutdown` is absent before fence
admission, keeping publish completion records compact. Its count excludes channel
and inflight work; `captured_at` identifies observation age. It is not a delivery
proof. Typed `OrderedDisconnectFailure` preserves native reasons without leaking
host callback text. Public enums gain variants and exhaustive Rust matches need
updating. See the [C contract](../c/README.md#ordered-publish-shutdown-optional),
[recipe](../../docs/recipes/ordered-shutdown.md), and
[performance harness](benches/README.md). Cargo feature unification can activate
native costs even if wrapper ordered API support is disabled.
