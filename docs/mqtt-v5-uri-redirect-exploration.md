# MQTT 5 URI redirect exploration

**Exploration date:** 2026-08-03

**Scope:** `rumqttc-v5-next`

## Decision

Support absolute `mqtt`, `mqtts`, `ws`, and `wss` URIs in MQTT 5 Server
Reference, in addition to the authority forms already supported. This is a
rumqttc extension, not an MQTT conformance requirement: MQTT 5 section 4.11
deliberately leaves the reference format unspecified and only recommends a
simplified URI authority.

URI support should enable real WebSocket redirects. In particular, the parsed
WebSocket path and query must become the request target used by the redirected
connection. Accepting a `ws` URI and then reducing it to host and port would be
incorrect.

The advertised scheme constrains which connection family is valid, but it must
not create TLS configuration, client certificates, CONNECT credentials, proxy
credentials, or WebSocket headers. The application redirect policy remains the
security boundary which supplies those values explicitly.

## Why the current model is insufficient

`RedirectReference` currently contains only `host` and `port`.
`RedirectTargetProfile` contains that reference and a `Transport`, and
`EventLoop::apply_redirect_profile` always constructs `Broker::tcp`. This is
safe for authority redirects over TCP or TLS, but it cannot represent a
WebSocket request target. A path such as `/mqtt/v5?tenant=green` would be lost.

Constructing a `Broker` independently in application policy is also not enough.
The event loop must be able to prove that the complete selected target,
including the WebSocket resource name, came from one of the advertised
references.

## Reference and endpoint model

Keep the advertised reference and the resolved connection endpoint distinct:

```rust,ignore
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RedirectScheme {
    Mqtt,
    Mqtts,
    Ws,
    Wss,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RedirectReference {
    raw: String,
    kind: RedirectReferenceKind,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum RedirectReferenceKind {
    Authority { host: String, port: Option<u16> },
    Uri {
        scheme: RedirectScheme,
        host: String,
        port: Option<u16>,
        // Present only for ws/wss and preserved without decoding.
        resource_name: String,
    },
}

#[derive(Clone, Debug)]
pub struct RedirectTargetProfile {
    reference: RedirectReference,
    broker: Broker,
    transport: Transport,
    // Existing identity, session, and credential policy fields follow.
}
```

The public accessors should include `raw()`, `scheme()`, `host()`, `port()`,
and, for WebSockets, `websocket_resource_name()`. A feature-independent
`ensure_supported()` accessor should report whether the current build can
materialize the reference. `host()` should continue to return an unbracketed
IPv6 literal, matching the present API.

`RedirectTargetProfile` should carry the complete validated `Broker`, not just
a host and port. There must be no public constructor or setter accepting an
unrelated `Broker`. Instead, fallible constructors derive it from the selected
`RedirectReference` and check the supplied transport:

```rust,ignore
impl RedirectTargetProfile {
    pub fn isolated(
        reference: RedirectReference,
        transport: Transport,
    ) -> Result<Self, RedirectTargetError>;

    pub fn broker(&self) -> &Broker;
}
```

This is a justified breaking adjustment to the newly added, unreleased redirect
API. It makes provenance structural: the profile owns the parsed reference, and
its private broker is derived only by the library. The event loop should still
perform a defensive value check that `profile.reference()` equals one of the
current `RedirectContext::references` before using the profile. It should not
rely on an index because indices are meaningful only within one callback.

Broker derivation should use the same constructors and compatibility validator
as ordinary options:

- authority, `mqtt`, and `mqtts` references derive `Broker::tcp(host, port)`;
- `ws` and `wss` references derive a WebSocket `Broker` containing the complete
  normalized URI; and
- the resulting broker/transport pair passes the same
  `broker_transport_matches`/`MqttOptions::validate` path as a normal client.

The small broker/transport compatibility helper should therefore become a
crate-visible reusable validator instead of being duplicated in redirect code.

### Fallible policy callbacks

Broker derivation can fail because of a scheme/transport mismatch or a feature
which is not compiled. Policy must be able to preserve that structured cause
without calling `unwrap` or converting it into a generic rejection. Change the
handler result to:

```rust,ignore
pub type RedirectPolicyResult = Result<RedirectDecision, RedirectTargetError>;

RedirectPolicy::new(max_attempts, |context| -> RedirectPolicyResult {
    let reference = context.references.first().ok_or(RedirectTargetError::NoTarget)?;
    let profile = RedirectTargetProfile::isolated(reference.clone(), Transport::tcp())?;
    Ok(RedirectDecision::follow(profile))
})
```

For source compatibility, `RedirectPolicy::new` may continue accepting
callbacks returning `RedirectDecision`, while a `RedirectPolicy::try_new`
constructor accepts fallible callbacks. Internally both should produce the same
result type. A target error becomes `RedirectFailure::Target(error)` rather than
`RedirectFailure::Rejected`.

## Parsing and component rules

`parse_server_references` should continue splitting on ASCII whitespace and
validating every token before policy runs. Each token is classified as an
absolute URI only when it has a URI scheme followed by `://`; otherwise it is
parsed by the existing authority grammar. Relative URI references and
scheme-relative forms are not accepted.

Schemes are matched case-insensitively and normalized to lowercase. Recognize
exactly `mqtt`, `mqtts`, `ws`, and `wss`. An otherwise syntactically valid URI
with another scheme produces `RedirectReferenceError::UnsupportedScheme`, not
the old catch-all `Scheme` error.

| Form | Default port | Path | Query | User information | Fragment |
| --- | ---: | --- | --- | --- | --- |
| authority | selected transport | not applicable | reject | reject | reject |
| `mqtt` | 1883 | empty or `/` only | reject | reject | reject |
| `mqtts` | 8883 | empty or `/` only | reject | reject | reject |
| `ws` | 80 | preserve; empty means `/` | preserve | reject | reject |
| `wss` | 443 | preserve; empty means `/` | preserve | reject | reject |

The `mqtt` and `mqtts` defaults follow the registered MQTT and secure-MQTT TCP
ports. The URI schemes themselves are provisionally registered, so these
defaults should be documented as rumqttc behavior rather than attributed to
MQTT 5.

Specific validation rules are:

- Require an authority and a non-empty host.
- Accept DNS/reg-name hosts, IPv4, and bracketed IPv6. Reject an unbracketed
  IPv6 literal, empty port, port zero, and a port outside `u16`.
- Retain existing underscore-containing authority names so SRV references stay
  observable, but continue rejecting them as connection targets until RFC 2782
  resolution exists. URI hosts do not use the SRV exception.
- Reject user information even when it has no password. It is an implicit
  credential channel and can obscure the actual host.
- Reject fragments for every scheme. Fragments are not transmitted, and RFC
  6455 specifically excludes them from WebSocket URIs.
- Reject paths and queries for `mqtt`/`mqtts` rather than silently discarding
  them. Treat an empty path and `/` as the same convenience form.
- Allow `path-abempty` and query for `ws`/`wss`. Preserve the encoded path and
  query exactly for the handshake; do not percent-decode and re-encode them.
  An empty WebSocket path materializes as `/`, as required for its resource
  name.
- Reject ASCII controls, backslashes, malformed percent triplets, and any URI
  which the existing WebSocket request builder would reject. Validation must
  happen before the target is installed in `MqttOptions`.
- Do not read CONNECT username/password or any other option from a URI query.
  A WebSocket query is opaque routing data only.

The parser should not use the optional `url` feature: redirect syntax must not
change when that unrelated convenience feature is toggled. Prefer a small,
shared URI-component parser based on `http::Uri`, moving `http` out from behind
the `websocket` feature if necessary. Apply the explicit component rules after
generic parsing; a generic URI parser alone is not a security policy.

## Transport and credential rules

The compatibility matrix for URI references is strict:

| Advertised form | Permitted transport |
| --- | --- |
| authority | explicit `Tcp` or `Tls`; existing default-port behavior remains |
| `mqtt` | `Tcp` only |
| `mqtts` | `Tls` only, with application-supplied `TlsConfiguration` |
| `ws` | `Ws` only |
| `wss` | `Wss` only, with application-supplied `TlsConfiguration` |

Do not allow an application to pair `mqtt://` with TLS, `mqtts://` with plain
TCP, `ws://` with WSS, or `wss://` with WS. If policy wants a different security
mode, the server must advertise a reference whose scheme says so, or advertise
an authority whose transport is explicitly chosen by policy.

For `mqtts` and `wss`, the scheme asserts that TLS is required; it does not
select roots, a crypto provider, client certificates, ALPN, or a default TLS
configuration. The policy supplies `Transport::Tls(config)` or
`Transport::Wss(config)`. TLS server-name verification continues to use the
redirected URI host through the ordinary connector.

The current isolated-profile defaults remain correct. CONNECT authentication,
the enhanced authenticator, proxy settings, request modifiers, Client
Identifier, and session/checkpoint scope remain cleared unless policy opts each
one back in. A WebSocket query must never be interpreted as permission to copy
an HTTP authorization header.

## Loop identity

Loop detection should compare connection identities, not the spelling of the
advertisement. Construct the key after the policy transport has been validated:

```text
tcp://host:port
tls://host:port
ws://host:port/resource-name
wss://host:port/resource-name
```

Normalize the key as follows:

1. lowercase the transport identity and DNS host;
2. remove one terminal dot from a DNS host;
3. parse and canonically print IP literals, with brackets around IPv6;
4. insert the effective port, including scheme or selected-transport defaults;
5. for WS/WSS, normalize an empty path to `/`, uppercase hex digits in percent
   triplets, decode percent-encoded unreserved characters, and remove literal
   dot segments; and
6. retain path case and the normalized query because both are part of the
   WebSocket resource name.

Thus `broker.example`, `broker.example:1883`, and
`mqtt://BROKER.EXAMPLE.:1883/` select the same TCP identity when the authority
uses `Transport::Tcp`. Likewise, `ws://broker.example` and
`ws://BROKER.EXAMPLE:80/` are the same WebSocket identity. Two WebSocket URIs
with different paths or queries are different targets and may legitimately
route to different MQTT services.

The non-zero attempt bound remains necessary even with normalization: DNS
aliases, changing addresses, and application routing can create semantic loops
which string normalization cannot prove.

## Feature-gated behavior

Syntax parsing must be feature-independent. A build without `websocket` should
still parse and expose a valid `ws` or `wss` reference to policy. Likewise, a
build without a TLS backend should still recognize `mqtts` and `wss`.

`RedirectReference::ensure_supported()` reports feature availability without
requiring the caller to construct a transport variant which does not exist in
that build. The fallible profile factory calls it before checking
scheme/transport compatibility. Consequently, trying to materialize a `ws`
reference in a non-WebSocket build reports `WebsocketUnavailable`, even if the
only transport value the caller could supply is `Tcp`. A policy remains free to
reject such a reference deliberately; only an attempted selection is reported
as unavailable.

Materializing a selected profile reports a structured error:

```rust,ignore
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RedirectTargetError {
    #[error("Server Reference scheme {scheme:?} is incompatible with the selected transport")]
    TransportMismatch { scheme: Option<RedirectScheme> },
    #[error("Server Reference requires the disabled `websocket` feature")]
    WebsocketUnavailable,
    #[error("Server Reference requires an enabled TLS backend and explicit TLS configuration")]
    TlsUnavailable,
    #[error("Server Reference names an unsupported SRV target")]
    SrvUnavailable,
    #[error("Server Reference did not provide a selectable target")]
    NoTarget,
}
```

These errors should flow through `RedirectFailure::Target`. Do not classify a
recognized-but-disabled `ws` URI as `UnsupportedScheme`, and do not make one
malformed merely because a crate feature is absent. This also lets a policy
choose an available `mqtt` reference from a mixed `mqtt`/`wss` list in a
non-WebSocket build.

Because the current `Broker::Websocket` and `Transport::Ws` variants are
compiled out, the intermediate reference representation must remain
feature-neutral. Broker construction occurs in the fallible profile factory
under the applicable feature gate. The profile can carry the resulting
validated `Broker` once construction succeeds.

## Policy examples

The examples omit selection predicates beyond choosing the first reference;
production policy should also apply an application allow-list.

### Plain MQTT over TCP

```rust,ignore
let max_attempts = NonZeroUsize::new(3).expect("three is non-zero");
let policy = RedirectPolicy::try_new(max_attempts, |context| {
    let reference = context.references.first().cloned().ok_or(RedirectTargetError::NoTarget)?;
    let profile = RedirectTargetProfile::isolated(reference, Transport::tcp())?;
    Ok(RedirectDecision::follow(profile))
});
```

`mqtt://broker.example` becomes TCP `broker.example:1883`.

### MQTT over TLS

```rust,ignore
let tls = TlsConfiguration::Simple {
    ca: broker_ca,
    alpn: None,
    client_auth: None,
};
let max_attempts = NonZeroUsize::new(3).expect("three is non-zero");
let policy = RedirectPolicy::try_new(max_attempts, move |context| {
    let reference = context.references.first().cloned().ok_or(RedirectTargetError::NoTarget)?;
    let profile = RedirectTargetProfile::isolated(
        reference,
        Transport::tls_with_config(tls.clone()),
    )?;
    Ok(RedirectDecision::follow(profile))
});
```

`mqtts://broker.example` becomes TLS `broker.example:8883`. No TLS state comes
from the URI.

### WebSocket

```rust,ignore
let max_attempts = NonZeroUsize::new(3).expect("three is non-zero");
let policy = RedirectPolicy::try_new(max_attempts, |context| {
    let reference = context.references.first().cloned().ok_or(RedirectTargetError::NoTarget)?;
    let profile = RedirectTargetProfile::isolated(reference, Transport::ws())?;
    Ok(RedirectDecision::follow(profile))
});
```

`ws://broker.example/mqtt/v5?tenant=green` becomes a WebSocket broker whose
handshake uses `/mqtt/v5?tenant=green`; the default TCP port is 80.

### Secure WebSocket

```rust,ignore
let max_attempts = NonZeroUsize::new(3).expect("three is non-zero");
let policy = RedirectPolicy::try_new(max_attempts, move |context| {
    let reference = context.references.first().cloned().ok_or(RedirectTargetError::NoTarget)?;
    let profile = RedirectTargetProfile::isolated(
        reference,
        Transport::wss_with_config(tls.clone()),
    )?;
    Ok(RedirectDecision::follow(profile))
});
```

`wss://broker.example:8443/mqtt?region=sa` retains the explicit port and full
resource name. The supplied TLS configuration verifies `broker.example`.

## Implementation work

1. Extend `RedirectReference` with feature-neutral authority/URI variants,
   raw-value access, scheme access, and WebSocket resource-name access.
2. Replace the authority-only parser with classification plus strict
   scheme-specific validation. Keep the existing authority parser and SRV
   behavior intact.
3. Make `RedirectTargetProfile::isolated` fallible, derive and store `Broker`
   internally, and expose a fallible policy callback without removing the
   infallible convenience path.
4. Add `RedirectTargetError` and route it through `RedirectFailure::Target`.
   Keep malformed syntax in `RedirectReferenceError` and policy refusal in
   `RedirectFailure::Rejected`.
5. Reuse the ordinary broker/transport compatibility validator and change
   redirect application to install the profile broker instead of always
   constructing `Broker::tcp`.
6. Extend normalized redirect identity with scheme defaults and WebSocket
   resource names. Seed the visited set from TCP/TLS and WebSocket origins.
7. Keep parsing available in all feature combinations and gate only target
   materialization and connection code.
8. Update `rumqttc-v5/design.md`, public API documentation, WebSocket recipes,
   and `CHANGELOG.md` when implementation lands.

## Test work

Add table-driven parser tests for:

- all four schemes, case normalization, explicit/default ports, DNS, IPv4, and
  bracketed IPv6;
- retained authority and SRV behavior;
- empty hosts, malformed/zero/overflow ports, unbracketed IPv6, relative URIs,
  unsupported schemes, user information, fragments, controls, backslashes, and
  malformed percent encodings;
- `mqtt`/`mqtts` empty and `/` paths plus rejection of other paths and queries;
  and
- `ws`/`wss` empty, path-only, query-only, and path-plus-query resource names,
  asserting byte-for-byte preservation into `Broker::websocket_url()`.

Add profile and event-loop tests for:

- every permitted and forbidden scheme/transport pair;
- explicit TLS configuration on `mqtts` and `wss`, including redirected TLS
  server-name selection;
- proof that an independently chosen broker cannot be substituted for the
  advertised reference;
- authority/URI loop aliases, default ports, DNS case/trailing dots, canonical
  IPv6, and normalized WebSocket resource names;
- distinct WebSocket paths and queries remaining distinct loop identities;
- a real local WS redirect whose handshake observes the advertised path and
  query, plus the equivalent WSS test under each maintained TLS backend;
- mixed reference lists where policy chooses one target;
- `--no-default-features`, WebSocket-only, TLS-only, and WebSocket-plus-TLS
  builds returning `WebsocketUnavailable` or `TlsUnavailable` as appropriate;
  and
- existing isolation guarantees for authentication, request modifiers,
  proxies, client identity, and session-store scope.

Run the targeted redirect and WebSocket tests while iterating, then
`cargo test -p rumqttc-v5-next` and the CI `cargo hack` feature matrix before
merging the implementation.

## References

- [MQTT 5.0 section 4.11, Server redirection](https://docs.oasis-open.org/mqtt/mqtt/v5.0/mqtt-v5.0.html#_Toc3901254)
- [RFC 3986, URI Generic Syntax](https://www.rfc-editor.org/rfc/rfc3986.html)
- [RFC 6455 section 3, WebSocket URIs](https://www.rfc-editor.org/rfc/rfc6455.html#section-3)
- [IANA URI schemes registry](https://www.iana.org/assignments/uri-schemes/uri-schemes.xhtml)
- [IANA service names and port numbers
  registry](https://www.iana.org/assignments/service-names-port-numbers/service-names-port-numbers.xhtml?search=mqtt)
