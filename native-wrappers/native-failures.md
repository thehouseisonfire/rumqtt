# Structured native failure evidence

`Error::native_failure()` and C `rumqttc_error_native_failure_details()` expose
bounded scalar observations. Existing status, code, kind, retryability, delivery,
broker reason and dedicated failure accessors remain authoritative. Classifications
do not authorize retries or imply non-delivery. Python/JavaScript formats are unchanged.

Initialize `rumqttc_native_failure_details_t` with
`RUMQTTC_NATIVE_FAILURE_DETAILS_INIT`. The accessor requires a valid-sized output,
returns `OK` with `present=0` for an error without evidence, and clears known payload
fields before validating the error handle. It preserves `struct_size` and future
extension bytes; null/undersized outputs return `INVALID_ARGUMENT` without writing
beyond the size prefix. The record contains no pointers and is independent of the
error/client lifetime after copying. Retained error handles likewise own their data.

`fields` is a bitmask of independently present values. Zero with a presence bit is
a value; zero without the bit is absent. Unknown domain/code/I/O kind is zero with
the containing observation still present. Disabled features produce no fabricated
evidence; unknown future variants retain a known domain where possible. Applications
must accept unknown classifications and ignore unrecognized field bits.

## Classification and nested causes

Domains and codes are wrapper-owned explicit numbers, independent of Rust enum
layouts. The public C constants define their stable values. Equivalent v4/v5
errors share classifications. A domain-specific `UNKNOWN` retains the domain;
there is no invented numeric identity for unknown Rust variants.

The outer typed TLS, proxy, WebSocket or I/O error supplies `domain/code`.
`cause_domain/cause_code` describe the most specific recognized nested cause.
For example, I/O wrapping rustls certificate rejection reports `IO/IO` with
`TLS/CERTIFICATE` as its cause. TLS wrapping socket I/O reports `TLS/IO`, with
I/O evidence. Intermediate wrappers are not a retained source tree. Scalar I/O
kind and OS code come from the innermost inspected I/O error together, including
absence of that error's OS code. HTTP status comes only from a typed HTTP response.

One budget of 16 inspected source/wrapper levels bounds evidence extraction,
including redirected target recursion. At the limit, deeper evidence is absent
and a redirected target can have `CONNECTION/UNKNOWN`. Sources are never formatted
or retained in this record. Opaque TLS/backend failures explicitly remain unknown;
their text, numeric backend details and source-owned strings are not exposed.

Existing dedicated store/authentication errors are not traversed to extract
unrelated transport causes. Redirect failures retain their existing terminal
classification and may additionally expose target connection evidence. Exhaustion
itself has no native record: inspect its independently owned last-error handle.

## Scalar semantics

- `io_kind` is an explicitly mapped portable subset of `std::io::ErrorKind`;
  unmapped/new kinds use `UNKNOWN`. DNS and socket failures can both be generic
  I/O; no DNS stage is inferred from strings, hostnames or configuration.
- `os_error` exists only for a genuine `raw_os_error()`. `os_domain` identifies
  Linux, Darwin, Windows (system/WinSock), other Unix, or unknown target namespaces.
  These are target-specific numbers, not portable errno identities. Other-Unix
  and unknown values additionally require knowledge of the executing target.
- `packet_kind` uses MQTT control numbers 1..15. Packet kind and identifier have
  independent presence. Native unsolicited-ACK errors provide only an identifier.
  Unexpected-packet errors expose the kind, without inspecting packet contents.
- `quantity` defines every present measurement: full packet bytes, remaining-length
  bytes, item counts, QoS levels, topic-alias numbers, or additional required bytes.
  `maximum` is an upper bound; `expected` is equality. Neither is inferred from a
  later configuration. SUBACK/UNSUBACK mismatches report return-code counts, not
  byte sizes. A full publish window supplies its maximum but no invented actual count.
- V4 incoming `PayloadSizeLimitExceeded` supplies only remaining length, without
  its configured maximum or full frame size. V5 supplies full packet size and
  maximum. Outgoing size limits use the native size/maximum, which may be local
  policy or a broker-negotiated maximum depending on the native path.

Connection provenance freezes at the source, including absent generation/revision.
Later connection/context updates do not reattribute retained failures. Operation
IDs may be attached when that failure resolves a particular unfinished operation.
Completed outcomes are never rewritten. Reconnect summaries retain sanitized scalar
evidence, fixed error-code messages and no opaque owners; diagnostic equality includes
the evidence and origin revision.

## Mapping inventory

The following groups inventory the current native variants. Names separated by `/`
share behavior. Unknown future variants use their containing domain's `UNKNOWN`.
Scalar fields not listed are absent. These mappings supplement the legacy mapping.

### Connection errors

- `MqttState`: See state / codec inventories below.
- `Io`: `IO / IO`, I / O kind and optional OS code; inspect typed inner cause.
- v4 `NetworkTimeout`, v5 `Timeout`: `CONNECTION / CONNECTION_TIMEOUT`.
- v4 `FlushTimeout`, both `DisconnectTimeout`: `CONNECTION / FLUSH_TIMEOUT` or `DISCONNECT_TIMEOUT`.
- `ConnectionRefused`: `CONNECTION / BROKER_REFUSED`; existing broker reason preserved.
- `NotConnAck`: `PROTOCOL / UNEXPECTED_PACKET`, received packet kind only.
- `SessionStateMismatch`: `PROTOCOL / SESSION_MISMATCH`; session booleans intentionally omitted.
- `BrokerTransportMismatch`, `RequestsDone`: `CONNECTION / CONFIGURATION` or `SHUTDOWN`.
- `SessionRecoveryPending / SessionRecoveryInvalid`: `CONNECTION / RECOVERY`.
- `SessionStore / SessionRestore`, optional `OrderedDisconnect`: Existing dedicated details; store/restore has no
  supplemental record. Ordered results retain available nested transport/protocol evidence.
- v5 `AuthProcessingError`: `PROTOCOL / AUTHENTICATION`; no AUTH text / data.
- v5 `Redirect`: Recurse only `FollowFailed` / `SrvTargetsExhausted.last_error`; other redirect variants use
  existing details.
- optional `Tls / Proxy / Websocket`: See feature inventories below.
- optional `WsConnect / InvalidUrl`: `WEBSOCKET / CONFIGURATION`; no URL / request text.
- optional `ResponseValidation / RequestModifier`: `WEBSOCKET / WEBSOCKET_PROTOCOL` or `CALLBACK`; existing
  callback detail preserved.

### State and codec errors

- `Io`, including codec `Io`: `IO / IO` and typed nested evidence.
- `Unsolicited`: `PROTOCOL / UNSOLICITED_ACK`, packet ID only.
- `AwaitPingResp / CollisionTimeout`: `CONNECTION / PING_RESPONSE_TIMEOUT` or `COLLISION_TIMEOUT`.
- `ConnectionAborted`: `IO / PEER_CLOSED`, no invented OS / I / O kind.
- `InvalidState`: `PROTOCOL / INVALID_STATE`.
- `EmptySubscription / EmptyUnsubscription`, v5 `Coversion`: `PROTOCOL / INVALID_REQUEST`.
- v4 `OutgoingPublishWindowFull`, v5 `OutgoingPublishQuotaExceeded`: `PROTOCOL / WINDOW_FULL`, maximum items.
- `ProtocolViolation::DuplicateConnAck`: `PROTOCOL / DUPLICATE_CONN_ACK`, CONNACK kind.
- `ProtocolViolation::UnexpectedIncomingPacket`, v4 codec `NotConnect`: `PROTOCOL / UNEXPECTED_PACKET`,
  received kind.
- `SubAckReturnCodeCountMismatch`, v5 `UnsubAckReasonCodeCountMismatch`: `PROTOCOL / ACK_COUNT_MISMATCH`, ACK
  kind, ID, actual and expected items.
- v5 `InvalidAlias`: `PROTOCOL / ALIAS_OUT_OF_RANGE`, actual and maximum aliases.
- v5 `QoSNotSupported`: `PROTOCOL / CAPABILITY_UNSUPPORTED`, requested and maximum QoS.
- v5 `RetainNotSupported / WildcardSubscriptionNotSupported / SubscriptionIdentifiersNotSupported /
  SharedSubscriptionNotSupported`: `PROTOCOL / CAPABILITY_UNSUPPORTED`; capability identity intentionally
  grouped.
- v5 state `IncomingPacketTooLarge`, codec `PayloadSizeLimitExceeded`: `PROTOCOL / INCOMING_TOO_LARGE`,
  measurements as documented above.
- state / codec `OutgoingPacketTooLarge`: `PROTOCOL / OUTGOING_TOO_LARGE`, actual and maximum packet bytes.
- v5 `ServerDisconnect / ConnFail / ServerRedirect`: `CONNECTION / BROKER_DISCONNECT`, `BROKER_REFUSED` or
  `REDIRECT`; existing broker / redirect details preserved.
- v5 `AuthError / AuthenticatorNotSet / AuthenticatorLockPoisoned`: `PROTOCOL / AUTHENTICATION`; text omitted.
- codec `InsufficientBytes`: `PROTOCOL / MALFORMED_PACKET`, additional bytes needed; no stage inference.
- codec `InvalidConnectReturnCode / InvalidProtocol / InvalidProtocolLevel / IncorrectPacketFormat /
  InvalidPacketType / InvalidPropertyType / InvalidQoS / InvalidSubscribeReasonCode / PacketIdZero`: `PROTOCOL
  / MALFORMED_PACKET`; invalid field values intentionally grouped.
- codec `PayloadSizeIncorrect / PayloadTooLong / PayloadRequired / TopicNotUtf8 / BoundaryCrossed /
  MalformedPacket / MalformedRemainingLength`, v4 `UnexpectedConnect`: `PROTOCOL / MALFORMED_PACKET`; text and
  boundary counts omitted.
- v5 codec `InvalidReason / InvalidRemainingLength / InvalidRetainForwardRule / ProtocolError /
  ProtocolViolation / PayloadNotUtf8`: `PROTOCOL / MALFORMED_PACKET`; existing broker reason behavior
  unchanged.

### Feature-dependent causes

- rustls / native TLS native `Io`: `TLS / IO`, inspect typed I / O cause.
- rustls native `TLS`: `TLS / TLS_PROTOCOL`, specific rustls cause below.
- rustls native `WebPki`: `TLS / CERTIFICATE`; no third-party payload.
- native `Addr / UnsupportedBackendConfiguration`, rustls `DNSName / NativeCerts / CryptoProviderUnavailable /
  Pem / NoValidCertInChain / NoValidClientCertInChain / NoValidKeyInChain`: `TLS / CONFIGURATION`; no names,
  certificate arrays, paths, keys or PEM text.
- native `NativeTls`: `TLS / UNKNOWN`; opaque backend error omitted; inspect any typed I / O source within the
  shared budget.
- rustls `InvalidCertificate / NoCertificatesPresented / InvalidCertRevocationList`: `TLS / CERTIFICATE`;
  certificate subtype / details omitted.
- rustls `BadMaxFragmentSize / InconsistentKeys / UnsupportedNameType`: `TLS / CONFIGURATION`.
- rustls `InappropriateMessage / InappropriateHandshakeMessage / InvalidMessage / PeerIncompatible /
  PeerMisbehaved / AlertReceived / DecryptError / EncryptError / HandshakeNotComplete /
  PeerSentOversizedRecord / NoApplicationProtocol`: `TLS / TLS_PROTOCOL`; native enum payloads omitted.
- rustls `General / Other / InvalidEncryptedClientHello / FailedToGetCurrentTime / FailedToGetRandomBytes`:
  `TLS / UNKNOWN`; arbitrary provider text / owners omitted.
- proxy `Io`, HTTP `IoError`, SOCKS `Io`: `PROXY / IO` and nested I / O evidence.
- HTTPS proxy `Tls`: `PROXY / TLS_PROTOCOL` and specific TLS cause.
- HTTP `HttpCode200`: `PROXY / PROXY_AUTHENTICATION` for 401 / 407, `PROXY_POLICY` for 403, otherwise
  `PROXY_UNAVAILABLE`; HTTP status.
- HTTP `EndOfFile`: `PROXY / PEER_CLOSED`.
- HTTP `HttpParseError / MaximumResponseHeaderLengthExceeded / NoHttpCode / NoHttpReason /
  HttpReasonConnectionEstablished`: `PROXY / MALFORMED_PACKET`; response strings omitted.
- SOCKS `NoAcceptableAuthMethods / UnknownAuthMethod / InvalidAuthValues / PasswordAuthFailure /
  AuthorizationRequired / IdentdAuthFailure / InvalidUserIdAuthFailure`: `PROXY / PROXY_AUTHENTICATION`;
  backend auth codes / text omitted.
- SOCKS `ConnectionNotAllowedByRuleset`: `PROXY / PROXY_POLICY`.
- SOCKS `ProxyServerUnreachable / GeneralSocksServerFailure / NetworkUnreachable / HostUnreachable /
  ConnectionRefused / TtlExpired`: `PROXY / PROXY_UNAVAILABLE`; no invented OS code.
- SOCKS `InvalidTargetAddress / CommandNotSupported / AddressTypeNotSupported`: `PROXY / CONFIGURATION`.
- SOCKS `InvalidResponseVersion / InvalidReservedByte / UnknownAddressType`: `PROXY / MALFORMED_PACKET`.
- SOCKS `ParseError / UnknownError`: `PROXY / UNKNOWN`.
- WebSocket `Io`: `WEBSOCKET / IO` and nested I / O evidence.
- WebSocket `ConnectionClosed / AlreadyClosed / Protocol::ResetWithoutClosingHandshake`: `WEBSOCKET /
  PEER_CLOSED`.
- WebSocket `Http`: `WEBSOCKET / WEBSOCKET_HTTP`, status only; headers / body omitted.
- WebSocket other `Protocol / Utf8 / AttackAttempt`: `WEBSOCKET / WEBSOCKET_PROTOCOL`; text omitted.
- WebSocket `HttpFormat / Url`: `WEBSOCKET / CONFIGURATION`.
- WebSocket `Tls / Capacity / WriteBufferFull`: `WEBSOCKET / UNKNOWN`; opaque TLS details and owned messages
  omitted.

## Verification

Typed mapping tests cover scalar evidence, shared extraction bounds, redaction,
owner release, dedicated failures, snapshot equality and origin preservation.
The C `native-failure-details` fixture exercises refusal, duplicate CONNACK,
unsolicited ACK, malformed packets, incoming limits and missed PING, then compares
event/completion/reconnect records after destroying the clients. Feature mapping,
C sizing/output initialization and header/export checks complete the contract.
