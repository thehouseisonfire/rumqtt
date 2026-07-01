codex resume 019f1f7a-b9f2-71d2-b6e5-9560ab76063c


• I reviewed the public API surface, READMEs, feature flags, event loop/state/session code, examples, changelog, and ran
  validation:

  cargo check --workspace passed
  cargo test -p rumqttc-v4-next --lib passed: 319 tests
  cargo test -p rumqttc-v5-next --lib passed: 544 tests

  The fork is already well past “basic MQTT client” shape: it has protocol-split packages, tracked notices, persistent-session
  hooks, manual ACKs, websocket/proxy/TLS/custom socket support, strict codec validation, and strong reconnect/session work. The
  most valuable remaining work is consumer adoption, diagnostics, and panic-free/production ergonomics.

  High-Value Improvements

  1. Publish a focused upstream migration guide
     Problem: The API has several intentional breaks: package names, Client::builder, PublishOptions, ConnectAuth, AckMode,
     TopicAliasPolicy, Broker, explicit TLS transports.
     Why it matters: This is likely the largest adoption tax for existing rumqttc users.
     Change: Add MIGRATION.md plus README links with “old upstream style -> new fork style” examples for common flows.
     Value: Faster upgrades, fewer issue reports, clearer fork positioning.
     Complexity: Low.
     Risk: None.
     Validate: Compile all migration snippets as doctests or examples/migration_*.
     Priority: must-have.

  2. Add panic-free constructors/configuration paths
     Problem: Some consumer paths can still panic, notably sync ClientBuilder::build() runtime creation and default rustls native-
     cert loading in TlsConfiguration::default_rustls() / default transport helpers. See rumqttc-core/src/lib.rs:112 and rumqttc-
     v4/src/client.rs:385.
     Why it matters: Libraries used in production services should let callers handle host TLS/runtime failures.
     Change: Add try_build(), TlsConfiguration::try_default_rustls(), and Transport::try_tls_with_default_config() equivalents
     while keeping old APIs.
     Value: Better reliability in daemons, embedded gateways, containers with unusual cert stores.
     Complexity: Low-medium.
     Risk: Low, additive.
     Validate: Unit tests with injected failing cert loader where possible; docs show fallible path.
     Priority: must-have.

  3. Add MqttOptions::validate() and try_build()
     Problem: Some invalid combinations are discovered only at connect time, for example broker/transport mismatch, missing
     websocket feature usage patterns, secure URL rejection, packet-size/session settings.
     Why it matters: Consumers want fail-fast config validation during startup, before a reconnect loop hides the root cause.
     Change: Add a structured ConfigError and MqttOptions::validate() -> Result<(), ConfigError>, then have try_build() call it.
     Value: Better deployment diagnostics and safer config-file driven apps.
     Complexity: Medium.
     Risk: Low if additive.
     Validate: Table-driven tests over TCP/TLS/WS/WSS/Unix/proxy/url/session combinations.
     Priority: high-value.

  4. Expose structured runtime diagnostics
     Problem: The event loop has important internal state, but consumers mostly get Event, ConnectionError, sparse log output, and
     a few queue length helpers.
     Why it matters: Production MQTT outages are often “why is publish stuck?” or “what is inflight?” not just “connection failed.”
     Change: Add EventLoop::diagnostics() returning a non-exhaustive snapshot: connected/disconnected, pending/queued/request-
     channel lengths, inflight, packet-id pressure, disconnect-drain state, session-store state, broker-only resume, read/write
     batch config.
     Value: Makes support dashboards, health checks, and bug reports much better.
     Complexity: Medium.
     Risk: Low if snapshot type is non-exhaustive.
     Validate: Unit tests for diagnostic values during blocked publish, graceful disconnect, reconnect replay, broker-only session
     resume.
     Priority: high-value.

  5. Improve error specificity and messages
     Problem: Some errors are too generic or lose actionable detail: v5 ConnectionError::Timeout, ClientError::Request/TryRequest,
     and websocket ResponseValidation has an empty display string. See rumqttc-v5/src/eventloop.rs:183.
     Why it matters: Operators need to know whether they hit channel full, sender dropped, connect timeout, flush timeout, broker
     protocol violation, or config mismatch.
     Change: Add more structured variants or fields while keeping existing non-exhaustive enums; preserve send failure cause where
     possible.
     Value: Faster debugging and better retry/drop policies.
     Complexity: Medium.
     Risk: Medium if pattern matching users rely on current variants, but enums are already non_exhaustive.
     Validate: Error-display snapshot tests and channel-full/disconnected tests.
     Priority: high-value.

  6. Provide an async Stream adapter
     Problem: Async users must manually loop on eventloop.poll().await; this is correct but not idiomatic for many Tokio/futures
     integrations.
     Why it matters: Consumers often want select!, stream combinators, service loops, and graceful shutdown composition.
     Change: Add EventLoopStream or impl Stream wrapper behind a small stream feature using futures-core.
     Value: Easier integration without changing the core externally-polled design.
     Complexity: Low-medium.
     Risk: Low, additive.
     Validate: Stream polling tests, cancellation-safety tests, README example with tokio::select!.
     Priority: high-value.

  7. Ship a tested session-store companion or feature-gated file store
     Problem: The SessionStore trait is solid, but every production user must implement crash-consistent persistence correctly.
     Why it matters: Restart-safe persistent sessions are a major differentiator, but the hardest part is delegated to consumers.
     Change: Prefer a small companion crate, e.g. rumqttc-session-store-file-next, or optional feature with atomic temp-file +
     rename semantics. Avoid database adapters in the main crate.
     Value: Makes strict persistent sessions immediately usable.
     Complexity: Medium.
     Risk: Low if outside the main crate.
     Validate: Crash-style tests for partial writes, corrupt checkpoints, client-id mismatch; example using v4 and v5.
     Priority: high-value.

  8. Add production recipes for common deployments
     Problem: Existing README examples are useful but mostly minimal. Advanced features exist, but consumers need recipes for TLS
     roots/client certs, WSS headers, proxies, persistent sessions, reconnect resubscribe, bounded channels, manual ACKs, and
     broker-specific quirks.
     Why it matters: MQTT adoption often happens against AWS IoT, EMQX, HiveMQ, Mosquitto, or corporate proxies.
     Change: Add docs/recipes/ with copy-pasteable, tested recipes and a feature matrix.
     Value: Reduces integration time and support load.
     Complexity: Low-medium.
     Risk: None.
     Validate: Compile examples; optional dockerized Mosquitto/EMQX smoke tests.
     Priority: high-value.

  9. Add topic/filter validated newtypes for subscriptions
     Problem: ValidatedTopic helps repeated publishes, but repeated subscriptions still use raw strings or protocol filter structs.
     Why it matters: Consumers building routers often subscribe repeatedly to known filters and want validation once.
     Change: Add ValidatedFilter and accept it in subscribe APIs, mirroring ValidatedTopic.
     Value: Better ergonomics and discoverability, minor performance win.
     Complexity: Low.
     Risk: Low, additive.
     Validate: Unit tests for wildcard-valid filters versus publish-topic-invalid filters.
     Priority: nice-to-have.

  10. Make observability integrate with tracing optionally
     Problem: The crate uses log; modern Tokio services often standardize on tracing spans/fields.
     Why it matters: MQTT connection lifecycle, reconnects, packet-id pressure, and protocol violations benefit from structured
     fields.
     Change: Add optional tracing feature or convert key diagnostics through tracing while preserving log compatibility if desired.
     Value: Better production telemetry.
     Complexity: Medium.
     Risk: Low if feature-gated.
     Validate: Feature matrix build and tests asserting key spans/events via test subscriber.
     Priority: nice-to-have.

  Avoid Adding

  Avoid native async-std/smol support in the core crates right now. The code is deeply Tokio-based; abstraction would be high-
  maintenance and low near-term value.

  Avoid putting database-backed session stores in the main crate. A file store or companion crate is enough; Postgres/Redis/Sled
  adapters belong outside.

  Avoid a high-level typed-topic DSL inside rumqttc. The repo already points users to mqtt-typed-client-next; duplicating it would
  split ownership.

  Avoid building a broker/router API into these client crates. Keep benchmark/router tooling separate.

  Avoid automatic reconnect policy DSLs unless there is concrete demand. The externally-polled event loop is a core design
  strength; document recipes first.

  Ranked Roadmap Before Release

  1. MIGRATION.md with compile-tested examples.
  2. Panic-free try_build() / fallible TLS default constructors.
  3. MqttOptions::validate() and structured ConfigError.
  4. EventLoop::diagnostics() snapshot.
  5. Error-message cleanup and more actionable ClientError/ConnectionError variants.
  6. Production recipes for TLS/WSS/proxy/session/reconnect/backpressure.
  7. Async Stream adapter.
  8. File-backed session-store companion.
  9. ValidatedFilter.
  10. Optional tracing integration.
