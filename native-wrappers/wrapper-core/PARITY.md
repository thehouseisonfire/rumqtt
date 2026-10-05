# Native client parity inventory

This matrix records public mutable MqttOptions/NetworkOptions setters, all
public synchronous/asynchronous client methods (merged by method name), and
every native Event, Outgoing, and AuthEvent variant. Equivalent builder methods
delegate to these setters; getters observe the immutable input configuration.
Codec internals are not a native-client API. The source audit parses all feature
branches, including disabled ones, so API additions require an explicit review.

| Protocol | Category | Native names | Status | Wrapper mapping / decision | Test reference |
| --- | --- | --- | --- | --- | --- |
| v4, v5 | option | set_last_will | supported | WC-01 CommonConfig.last_will with explicit protocol properties | config_wire, value_config |
| v4, v5 | option | set_client_id, set_transport, set_keep_alive | supported | CommonConfig owns immutable connection inputs; WC-13 independent TLS backends and ownership | config, tls::tls_input_ownership_is_released_on_every_driver_exit, tls::malformed_tls_credentials_and_alpn_fail_without_network_or_secret_disclosure, tls::valid_pkcs12_with_wrong_password_fails_without_disclosing_identity, tls::platform_roots_validate_an_isolated_process_trust_store, tls::tls_failure_panic_output_is_redacted, transport_composition::disabled_transports_fail_before_opening_a_socket |
| v4, v5 | option | protocol_compatibility_mut, set_protocol_compatibility | supported | Protocol-specific flat V4Config.session_present_mismatch_policy and V5Config.broker_session_resume_policy map into native ProtocolCompatibility without duplicated policy storage; raw/effective state is mapped through existing diagnostics | backend config_tests, protocol_compatibility, session_store, parity_inventory |
| v4 | option | try_set_client_id | supported | Fallible validation before start | config |
| v4, v5 | option | set_auth, clear_auth, set_username, set_credentials | supported | Optional owned username/password | config |
| v5 | option | set_password | supported | Password-only CONNECT is legal only in v5 | config |
| v4 | option | set_clean_session, try_set_clean_session, set_session_mode, try_set_session_mode | supported | V4Config.clean_session and client-id validation | config, session_store |
| v5 | option | set_clean_start, set_session_mode, set_session_expiry_interval | supported | V5Config.clean_start and CONNECT session expiry | config_wire, session_store |
| v4, v5 | option | set_request_channel_capacity, set_max_request_batch, set_read_batch_size, set_pending_throttle | supported | WC-03 distinct channel, request/read batching and throttle controls | backend config_tests, lifecycle |
| v4 | option | set_max_packet_size, set_inflight, try_set_inflight | supported | WC-03 local input limit and v4 output/inflight limits | runtime_limits::outgoing_inflight_obeys_local_and_broker_limits, runtime_limits::oversized_outgoing_publish_reports_protocol_failure_and_resolves_on_close, runtime_limits::incoming_decoder_limit_is_independent_of_advertised_maximum |
| v5 | option | set_max_packet_size, set_incoming_packet_size_limit, set_local_incoming_packet_size_limit, set_unlimited_incoming_packet_size, set_outgoing_inflight_upper_limit | supported | WC-03 independent local and advertised limits | runtime_limits::outgoing_inflight_obeys_local_and_broker_limits, runtime_limits::oversized_outgoing_publish_reports_protocol_failure_and_resolves_on_close, runtime_limits::incoming_decoder_limit_is_independent_of_advertised_maximum |
| v5 | option | set_connect_properties, set_receive_maximum, set_topic_alias_max, set_request_response_info, set_request_problem_info, set_user_properties, set_authentication_method, set_authentication_data | supported | WC-04 nested V5ConnectProperties preserves presence/order | config_wire, value_config, topic_alias::rejected_connect_properties_preserve_reason_before_first_generation |
| v5 | option | set_topic_alias_policy | supported | WC-04 native alias map remains authoritative across connection generations | topic_alias::automatic_and_explicit_aliases_replay_concrete_topics_under_new_connack_limits, topic_alias::alias_rejection_retains_broker_reason_and_connection_generation, backend::v5::config_tests::unrecoverable_alias_notice_retains_the_native_terminal_reason |
| v4, v5 | option | set_ack_mode | supported | Automatic or manual ACK token ownership | protocol_parity |
| v4, v5 | option | set_session_store, set_session_store_arc, clear_session_store, set_session_store_scope, clear_session_store_scope | supported | WC-02 optional owned store and explicit stable scope | session_store::restart_recovers_mixed_subscriptions_publishes_and_incoming_qos2, session_store::store_completion_races_shutdown_and_abandonment_without_retaining_owner, backend session tests |
| v5 | option | set_broker_session_resume_policy | supported | Strict default; explicit AllowBrokerOnly | backend config_tests, session_store |
| v5 | option | set_authenticator, set_async_authenticator, set_auth_manager | supported | WC-05 owned nonblocking callback or per-client SCRAM | authentication::broker_authentication_method_change_retains_typed_failure, authentication::explicit_callback_rejection_is_terminal_and_releases_owner, authentication::authentication_reconnect_and_pending_challenge_shutdown_release_exchange, authentication::scram_verifies_server_proof_for_initial_authentication_and_reauthentication |
| v5 | option | set_redirect_policy, clear_redirect_policy | supported | WC-06 Reject or finite isolated-target Follow policy | redirect_policy::redirect_reference_forms_select_isolated_endpoints_for_both_sources, redirect_policy::rejected_redirects_preserve_context_and_resolve_pending_operations, redirect_policy::redirect_loops_and_attempt_exhaustion_are_terminal, redirect_policy::isolated_redirect_never_reads_or_writes_the_origin_store_scope, transport_composition::disabled_redirect_transports_fail_before_driver_start |
| v5 | option | set_srv_resolver, clear_srv_resolver | supported | WC-06 owned async resolver; native seeded weighted-selection tests supplement broker priority coverage | redirect_policy::srv_lookup_failure_empty_answers_and_cancellation_release_owner, redirect_policy::srv_priority_precedes_weight_and_selected_endpoint_is_reported; rumqttc-v5 srv::tests::inclusive_zero_draw_can_select_a_zero_weight_record, srv::tests::higher_weight_is_selected_first_materially_more_often |
| v4, v5 | option | set_proxy | supported | WC-07 HTTP CONNECT, HTTPS proxy, SOCKS5; native has no SOCKS4 | transport_composition::proxy_tls_and_websocket_compositions_reconnect_for_both_protocols, transport_composition::proxy_negotiation_failures_and_shutdown_resolve_pending_work, transport_composition::proxy_and_broker_tls_trust_policies_are_independent, transport_composition::proxy_failure_process_output_is_redacted |
| v4, v5 | option | set_request_modifier | supported | WC-09 static edits then bounded dynamic path/query/authority/header edits; isolated redirects clear origin authority | transport_composition::proxy_tls_and_websocket_compositions_reconnect_for_both_protocols, redirect_policy::websocket_redirect_uses_target_uri_and_clears_origin_header_edits, transport_composition::disabled_transports_fail_before_opening_a_socket |
| v4, v5 | option | set_fallible_request_modifier | supported | Owned async callback with explicit URI/Host authority overrides; GET/HTTP1.1, scheme and upgrade fields fixed; native deadline and typed failure | websocket_handshake::dynamic_tokens_and_request_targets_refresh_across_ws_and_wss_reconnects, websocket_handshake::pending_handshakes_use_native_deadline_and_close_cancels_them, websocket_redaction::wrapper_diagnostics_redact_handshake_credentials |
| v4, v5 | option | set_connection_deadline | not applicable | Internal native attempt metadata; generated per attempt, not a host configuration override | custom_transport_memory, backend::transport::tests |
| v4, v5 | option | set_socket_connector | supported | Owned custom base/established streams, explicit socket policy and native deadline; C retained-operation/cancellation contract; terminal failures stop SRV fallback and retain redirect detail | custom_transport, custom_transport_memory, custom_transport_native_tls, custom_transport_redirect, transport_composition, transport::proof, native_custom_transport, native_custom_transport_matrix |
| v4, v5 | option | set_connection_timeout, set_tcp_send_buffer_size, set_tcp_recv_buffer_size, set_tcp_nodelay, set_bind_addr, set_bind_device, set_mptcp | supported | WC-12 network values; unsupported platform-specific settings fail eagerly | platform_network::bind_address_reaches_broker_for_both_protocols, platform_network::bind_device_failure_reaches_socket_and_mptcp_can_connect, platform_network::unsupported_network_controls_fail_before_driver_start; rumqttc-core tests::network_controls_are_applied_to_the_created_socket |
| v5 | option | set_network_options, set_connect_timeout | supported | CommonConfig.network plus one total connection-timeout input | backend config_tests |
| v4, v5 | operation | builder | supported | NativeClient.start owns construction/runtime/event-loop lifecycle | config, lifecycle |
| v4, v5 | operation | from_sender, from_senders | intentionally omitted | Foreign channels bypass managed admission/completion; explicitly out of scope | lifecycle |
| v4, v5 | operation | publish, try_publish, publish_tracked, try_publish_tracked | supported | PublishCommand always provides tracked terminal observation | protocol_parity, shutdown, session_store |
| v4, v5 | operation | subscribe, subscribe_many, subscribe_tracked, subscribe_many_tracked, try_subscribe, try_subscribe_many, try_subscribe_tracked, try_subscribe_many_tracked | supported | SubscribeCommand owns one or more filters; local admission differs from SUBACK | protocol_parity |
| v4, v5 | operation | unsubscribe, unsubscribe_many, unsubscribe_tracked, unsubscribe_many_tracked, try_unsubscribe, try_unsubscribe_many, try_unsubscribe_tracked, try_unsubscribe_many_tracked | supported | UnsubscribeCommand owns one or more filters and tracked result | protocol_parity |
| v5 | operation | subscribe_with_properties, subscribe_many_with_properties, subscribe_with_properties_tracked, subscribe_many_with_properties_tracked, try_subscribe_with_properties, try_subscribe_many_with_properties, try_subscribe_with_properties_tracked, try_subscribe_many_with_properties_tracked | supported | Explicit packet and per-filter v5 options | protocol_parity |
| v5 | operation | unsubscribe_with_properties, unsubscribe_many_with_properties, unsubscribe_with_properties_tracked, unsubscribe_many_with_properties_tracked, try_unsubscribe_with_properties, try_unsubscribe_many_with_properties, try_unsubscribe_with_properties_tracked, try_unsubscribe_many_with_properties_tracked | supported | Explicit v5 unsubscribe properties | protocol_parity |
| v4, v5 | operation | ack, try_ack, manual_ack, try_manual_ack, prepare_ack | supported | Generation/client-bound AckToken; ACK timing on both protocols and legal client-originated MQTT 5 reason/properties in wrapper-core/C; Python/JavaScript expose default timing; local ACK flush completion | protocol_parity, manual_ack, acknowledgement unit tests, handle::acknowledgement_tests, native_acknowledgement_options |
| v4, v5 | operation | disconnect, try_disconnect, disconnect_with_timeout, try_disconnect_with_timeout, disconnect_now, try_disconnect_now | supported | Existing graceful/immediate commands and coalescing closer | shutdown, lifecycle |
| v5 | operation | disconnect_with_properties, try_disconnect_with_properties, disconnect_with_properties_timeout, try_disconnect_with_properties_timeout, disconnect_now_with_properties, try_disconnect_now_with_properties | supported | WC-10 first-admitted payload; exact MQTT5 reason/properties | config_wire, close_races::mqtt5_disconnect_options_on_v4_do_not_admit_or_close, close_races::close_callers_keep_independent_deadlines_and_escalation_preserves_payload |
| v5 | operation | reauth, try_reauth, reauth_tracked, try_reauth_tracked | supported | WC-05 tracked Reauthenticate command | authentication::overlapping_reauthentication_is_rejected_before_the_active_exchange_closes, authentication::rejected_overlap_preserves_success_and_releases_admission_for_the_next_exchange, authentication::authentication_reconnect_and_pending_challenge_shutdown_release_exchange |
| v4, v5 | event | Event.Incoming | supported | WC-11 application packets mapped below; ACK packets stay internal to completions | protocol_parity, authentication |
| v4, v5 | event | Event.Outgoing, Outgoing.Publish, Outgoing.Subscribe, Outgoing.Unsubscribe, Outgoing.PubAck, Outgoing.PubRec, Outgoing.PubRel, Outgoing.PubComp, Outgoing.AwaitAck, Outgoing.PingReq, Outgoing.PingResp, Outgoing.Disconnect | supported | Optional OutgoingEvent has coarse activity and packet id where present | backend map_outgoing, lifecycle |
| v5 | event | Outgoing.Auth | supported | Optional outgoing Other activity; auth lifecycle separately observable | authentication |
| v5 | event | Event.Auth, AuthEvent.Started, AuthEvent.Continue, AuthEvent.Succeeded, AuthEvent.Failed | supported | Owned Authentication event; callback owns challenge data | authentication |
| v5 | event | Event.Redirect | supported | Accepted/rejected source/reason/reference; resolved SRV endpoint precedes Connected | redirect_policy::redirect_reference_forms_select_isolated_endpoints_for_both_sources, redirect_policy::srv_priority_precedes_weight_and_selected_endpoint_is_reported, redirect_policy::srv_lookup_failure_empty_answers_and_cancellation_release_owner |
| v4, v5 | event | Packet.ConnAck, Packet.Publish | supported | Connected details / IncomingPublish with owned properties | config_wire, protocol_parity, authentication |
| v5 | event | Packet.Disconnect | supported | BrokerDisconnect retains properties across poll cleanup | authentication |
| v4 | event | Packet.Disconnect | not applicable | MQTT 3.1.1 servers cannot send DISCONNECT | docs/spec/mqtt-v3.1.1.md |
| v4, v5 | event | Packet.PubAck, Packet.PubRec, Packet.PubRel, Packet.PubComp, Packet.SubAck, Packet.UnsubAck | intentionally omitted | No raw ACK stream; tracked terminal outcomes retain v4 identifiers/SUBACK codes and complete v5 terminal ACK details, including rejection/recovery; successful intermediate PUBREC/PUBREL remain internal | completion unit tests, native_acknowledgement_results, protocol_parity, session_store |
| v4, v5 | event | Packet.Connect, Packet.Subscribe, Packet.Unsubscribe, Packet.PingReq, Packet.PingResp | intentionally omitted | Client-origin packets and keepalive internals have no second raw event stream | protocol_parity, backend event mapping |
| v5 | event | Packet.Auth | intentionally omitted | Raw AUTH challenges belong to the configured authority, lifecycle events stay observable | authentication |
| v4, v5 | operation | disconnect_after_queued, disconnect_after_queued_with_timeout, try_disconnect_after_queued, try_disconnect_after_queued_with_timeout | intentionally omitted | Separate optional ordered-shutdown publish fence is not substituted for the existing protocol-drain close contract; requires a separately named native command/completion policy | shutdown, docs/recipes/ordered-shutdown.md |
| v5 | operation | disconnect_after_queued_with_properties, disconnect_after_queued_with_properties_timeout, try_disconnect_after_queued_with_properties, try_disconnect_after_queued_with_properties_timeout | intentionally omitted | Same optional ordered-shutdown fence decision; ordinary WC-10 payloads are supported without changing ordering semantics | config_wire, shutdown |

Terminal acknowledgement details are exposed through wrapper-core and C only.
Python/JavaScript retain their coarse completion/error contracts. C detail views
are completion-owned, survive client destruction, and use existing copy helpers
for caller-owned strings. Property Debug/log/error output is redacted.

## C binding progress

This section tracks the C surface separately from the wrapper-core inventory
above. A row here does not change the wrapper-core support decision.

Callback ownership has a standalone native dynamic-loading fixture: it closes
the client, releases the registration and final retained completion, observes
exactly one owner destruction, and only then unloads the shared library.
The native callback race fixture exercises store, authenticator, and resolver
registration replacement, failed construction, and concurrent completion/close.
It forces cancellation to win once for each authority and verifies late and
duplicate completion rejection, plus final retained-owner destruction.

| Slice | C status | Native C evidence |
| --- | --- | --- |
| WC-02 durable sessions | covered | `native_store_restart` restores v4/v5 pending subscriptions, outgoing QoS1/QoS2 packet IDs and incoming QoS2 without redelivery; it checks session loss and strict/broker-only resume policies. `native_store_write_failure` covers save/clear errors, timeout and late completion. `native_store_atomic` interrupts staged replacement and reloads the preceding complete checkpoint. `native_store_ownership` and `native_callback_races` check callback serialization/order, key exclusion, scope independence, replacement identity, overlap across clients, reentrant admission, destroy timeout and final owner release. |
| WC-05 enhanced authentication | covered | `native_auth` checks initial/repeated reauthentication, exact AUTH user-property names/order/duplicates/present-empty values, and nonempty string/binary event views retained across client destruction and copied across event destruction. Broker barriers in `native_auth_overlap` prove the overlap result is terminal while the first operation is still pending, then assert Failed → Disconnected → reconnect Started/Connected/Succeeded, increasing IDs, stable typed results, no extra AUTH, and exactly-once callbacks with bounded waits. Rejection, timeout, cancellation, abandonment, reconnect, structural response retry, method changes, malformed AUTH and invalid SCRAM proof have native fixtures; errors and captured output are checked for secret redaction. |
| WC-06 redirects and DNS SRV | covered | `native_redirect_matrix` checks address/MQTT/TLS/WS/WSS references from both sources, malformed/disallowed references, distinct-target attempt exhaustion, retained/copied views, disabled transports and origin store isolation. `native_srv_redirect`, `native_srv_candidates`, `native_srv_failure` and `native_srv_cancel` cover priority, C weight mapping and selection bias, unusable records, refused-target fallback/exhaustion, candidate metadata, missing/empty/failing resolvers, cancellation and owner release. Seeded native Rust SRV tests check the inclusive zero-weight draw deterministically. |
| WC-07 proxies | covered | `native_proxy_matrix` exercises HTTP CONNECT, HTTPS and SOCKS5 broker TLS/WSS for both MQTT versions and enabled TLS backends. Independent proxy/broker roots, authenticated negotiation, recoverable HTTP/SOCKS failures, timeout, pending-operation results, disabled/unknown protocols and captured-output redaction are checked. Seven feature profiles exercise installed static/shared CMake and pkg-config consumers. |
| WC-11 rich events | covered | `event_contract` checks accessor kinds, initialized failure outputs, optional outputs and ordered count/at views. `native_event_properties` verifies every CONNACK scalar/string selector, absent/present-empty fields, nonempty DISCONNECT views retained across client destruction and copied across event destruction, and PUBLISH/SUBSCRIBE/UNSUBSCRIBE outgoing IDs against broker-observed packet IDs. The accessor matrix includes invalid selectors, rejected CONNACKs, wrong kinds, each optional output independently omitted, NULL events and initialized failure outputs. Authentication, redirect and tracked operation IDs are checked in their native fixtures. `native_event_queue` retains 512 owners and closes through queue backpressure before releasing them. |
| WC-01/03/04 Will, connection and runtime options | covered | `native_wire_options` verifies nonempty-to-nonempty Will replacement in both protocols and MQTT 5 CONNECT replacement after overwriting both sets of caller buffers, including restart, binary fields, ordered properties and clear/default semantics; malformed sizes/selectors/reserved fields/counts/overflow are rejected. `native_will_process` verifies graceful suppression and abrupt-exit delivery with Mosquitto for both protocols. `native_runtime_limits` checks exact packet-size boundaries, independent local/advertised limits, local/broker inflight limits, standalone reset/clear defaults beyond removed limits, and explicit/automatic alias replay under changed reconnect limits. `native_runtime_batching` observes request/read batches at 0/1/8 for both protocols through preadmitted requests, broker barriers and event-queue backpressure; zero means one request or adaptive reads (eight for a window of sixteen). |
| WC-08/09/10 Unix, WebSocket and close options | covered | `native_network_options` checks Unix reconnect, missing-path/connection timeout, both shutdown modes, unsupported-platform rejection, WS/WSS header order/replacement/removal/clear, protected headers and disabled builds. `native_close_options` verifies exact MQTT5 DISCONNECT properties, independent caller deadlines, conflicting options and escalation. A Rust FFI regression checks destruction after a custom close. |
| WC-12 sockets and capabilities | covered | `native_network_options` verifies the broker-observed bind port and socket buffer/nodelay values before and after clearing overrides, using reference sockets to account for OS adjustments without accepting ignored options. POSIX descriptor inspection is verified on Linux; Windows process-snapshot socket inspection is implemented with execution pending. It also checks invalid device network failure, error redaction, observable Linux MPTCP (or kernel-unavailable TCP fallback), and unsupported device/MPTCP errors. Feature consumers check exact known capability bits and ignore an unknown future bit across all package profiles. |
| Custom transport connectors (TODO25) | covered | `native_custom_transport` supplies only bytes while native MQTT performs CONNECT and tracked QoS1/PUBACK for both protocols; short/deferred transfers, reconnect, graceful/immediate close, retained late completion, connect timeout, failed construction and final owner release are asserted. `native_custom_transport_matrix` routes TCP/TLS/WS/WSS and HTTP/HTTPS/SOCKS5 through a transparent C byte tunnel with enabled TLS backends and trust rejection. Rust memory composition and callback budget/abandonment/stale-stream tests cover the same managed lifecycle without sockets. |
| WC-13 TLS | covered | `native_tls_matrix` verifies PEM and PKCS12 mutual TLS, ALPN, wrong roots/hostname, malformed credentials, failed-start cleanup, owned configuration, transport replacement, disabled backends, TLS/WSS and trusted/untrusted isolated Linux platform roots for both protocols and each enabled backend. macOS keychain/admin trust and Windows current-user Root fixtures include partial-install/child-failure cleanup, ownership checks, persistent cleanup manifests and unconditional CI recovery; Python cleanup regressions pass on Linux, while actual OS-store execution is pending. The seven package profiles cover Rustls-only, native-only and mixed backends with/without proxies. |
| Owned TLS profiles (TODO27) | covered | Immutable C profile inputs, additive TLS/WSS/proxy/redirect setters, combined trust, enforced version policies and Rustls certificate/SPKI pins. `native_tls_profiles` checks native C ownership, reuse, failed setters, replacement, version exclusions, independent proxy roots, isolated redirects and ticket-enabled certificate/key rotation. `native_tls_matrix` also checks profile-owned PEM/PKCS#12 identities with mutual TLS/WSS. `tls_profiles` checks wrong names/chains, expiry, invalid handshake signatures, backup pins and ticket-enabled certificate/key rotation; pinned profiles disable resumption. Rustls TLS 1.2-only is opt-in (`tls12`); native TLS limits are exposed by per-backend capability masks. `tls_advanced` proves ordered cipher/SNI policy on the wire, default/disabled resumption, supplemental verification, external signature proof, optional-client-auth failure guarding and absolute callback deadlines. `native_tls_advanced` uses native OpenSSL EVP RSA-PSS/ECDSA hosts across v4/v5 TLS/WSS, independent HTTPS proxies, isolated redirects, reconnect authentication, nonblocking reentrant admission and cancellation while a synchronous callback is active. Registrations survive released handles and destroy exactly once; malformed registration and retained-profile ownership have Rust C-ABI tests. Native TLS supports SNI but rejects cipher/resumption restrictions and hooks. Deferred Rustls hooks use cancellable handshake workers and retained C tokens; arbitrary injected backend objects remain outside the profile contract. Linux Rust/native C execution passed; macOS/Windows execution is pending in the existing platform matrix. |

Coverage above describes assertions in the fixtures. Execution evidence is
tracked separately; implementation or CI wiring alone does not verify a host.

| Validation | Linux x86_64 | macOS arm64 | Windows x86_64 |
| --- | --- | --- | --- |
| Native fixtures and runnable C examples | 46/46 passed | Pending CI execution | Pending CI execution |
| Seven package profiles; installed static/shared CMake and pkg-config consumers; native transport/disabled-feature cases | 7/7 profiles passed; 49 consumer and 161 native checks, zero skips | Pending CI execution | Pending CI execution |
| Mosquitto graceful suppression and abrupt-exit Will delivery, both protocols | All four cases passed; required in CI/profiles | Provisioning configured; required execution pending | Provisioning configured; required execution pending |
| Platform trust, both protocols and enabled backends | Trusted acceptance/untrusted rejection passed for Rustls and native TLS, both protocols | Disposable keychain/trust fixture; execution pending | Disposable current-user Root fixture; execution pending |
| C AddressSanitizer/UndefinedBehaviorSanitizer | 46/46 passed (C harness; Rust library uninstrumented) | Configured; execution pending | MSVC AddressSanitizer configured; execution pending; UBSan unsupported |
| Ownership/cancellation/event-lifetime leak checks | 11/11 selected Valgrind tests passed | `leaks` configured; execution pending | ASan configured; execution pending; no Valgrind/`leaks` support |
| Current header and exports | Both passed | Mandatory CI checks; execution pending | Mandatory PowerShell checks; execution pending |
| Historical published-release ABI comparison | No applicable 0.1.0-alpha baseline; unverified | Pending supported-host comparison; no baseline established | Historical comparator unsupported; current contract checks remain mandatory |

CI preserves CTest logs/JUnit results and per-profile command logs and platform
metadata. Supported Will cases require Mosquitto; a missing broker fails CI.
Local optional-broker skips and an unavailable ABI baseline are never counted
as completed validation. Remote CI execution is pending for this change.

The fixtures also exposed fixes for locally rejecting overlapping
reauthentication without aborting the active exchange, and delivering its
ConnectionClosed failure event before transport cleanup. Focused wrapper-core
and MQTT 5 event-loop regressions cover those behaviors. Existing regressions
cover malformed AUTH decoding, C destruction after an admitted custom
DISCONNECT, and operation-registry ownership cycles. No public C declarations,
exported symbols or ABI record layouts changed.

### Custom transport execution evidence

The public C in-memory consumer, C bridge ownership tests, and Rust short-I/O
composition matrix pass on Linux. Socket/tunnel fixtures are implemented and
compiled, and are required in CI; this implementation session cannot execute
them because its sandbox rejects local socket creation. macOS/Windows execution
is pending CI. The historical fixture counts above predate these additions.

### Advanced TLS execution evidence

The completed TODO27 extensions have current Linux x86_64 execution evidence.
Local socket fixtures ran successfully in this session, including the custom
transport fixtures that could not run in the earlier session described above.
These results supplement the historical validation table; they do not imply
new sanitizer, leak-tool, macOS or Windows execution.

| Validation | Current Linux result |
| --- | --- |
| Native C fixtures and runnable examples, including advanced TLS | 57/57 passed with Rustls and native TLS enabled |
| Rustls Ring advanced adapters | 8/8 passed, including every advertised external signature scheme |
| Rustls Ring native consumers | Advanced TLS passed with and without WebSocket support; external identity example, general integration and error-output coverage passed |
| Wrapper-core and C Rust tests | Full crate suites passed with TLS 1.2, native TLS and proxy features |
| Client each-feature test matrix | 36/36 feature configurations passed |
| Wrapper-core and C each-feature test matrix | 33/33 feature configurations passed |
| Installed static/shared CMake and pkg-config consumers | Seven feature profiles passed, 49 consumer checks |
| Generated C/C++ header, exported symbols and ABI containment | Passed; existing public C record layouts and loader identity preserved |
| MSRV | Rust 1.88 workspace/all-target checks passed |
| Formatting and fixture harness | Both workspace formatting checks, diff whitespace check and 26 Python fixture tests passed |
| Rustls production linkage | `ldd` confirms the Ring-only wrapper links neither libssl nor libcrypto; OpenSSL EVP is confined to host consumers |

The advanced adapters also passed with the AWS-LC provider. The external
identity algorithm fixture checks each provider's advertised RSA-PSS,
RSA-PKCS#1, ECDSA and Ed25519 subset using actual handshakes. Native consumers
prove shared-profile failure isolation and owner retention while synchronous
callbacks are active. The required OpenSSL consumer job is wired into CI;
remote CI and macOS/Windows execution remain pending.

### Deferred TLS execution evidence

Deferred verification, selection and signing have current Linux x86_64
execution evidence. These results supplement the preceding historical tables;
sanitizer and leak-tool execution was not repeated for these additions.

| Validation | Current Linux result |
| --- | --- |
| Native C fixtures and runnable examples | 60/60 passed with Rustls, native TLS, TLS 1.2 and proxies enabled; no skips |
| Advanced Rust TLS fixtures | 15/15 passed with AWS-LC and with Ring without WebSocket support |
| Deferred C completion ownership | Seven tests passed, including abandonment, retained limits, expiry, late buffers, concurrent completion/cancellation and failed-constructor ownership; also passed without TLS features |
| Wrapper-core and C Rust tests | Full crate suites passed with TLS 1.2, native TLS and proxies |
| Wrapper-core and C each-feature matrix | 33/33 configurations passed |
| Installed static/shared CMake and pkg-config consumers | Seven profiles passed, 49 checks |
| TLS-only native consumers | Synchronous/deferred advanced fixtures and both EVP signer examples passed with Ring and WebSocket disabled |
| C/C++ headers, generated declarations, exports and existing ABI containment | Passed; existing public C records and declarations preserved |
| Strict workspace Clippy, formatting and MSRV | Passed across all targets; Rust 1.88 supported |
| Rustls production linkage | Ring-only shared library links neither libssl nor libcrypto |

The deferred fixtures cover immediate and delayed responses, cancellation at
all three stages, original deadlines, future construction/poll/destruction
panics, network-wait cancellation before runtime shutdown, retained cancelled
tokens, shared profiles, reconnects, independent proxy/redirect policies and
custom byte streams. A mixed-profile regression checks that synchronous and
deferred TLS callbacks share the driver thread. Standard pins and external
signature proof remain authoritative. Dedicated CI consumers require the
host-side EVP signer and exercise TLS-only feature gating. macOS/Windows and
remote CI execution remain pending.

### Deferred TLS cancellation regression

Pending verification, selection and signing futures that panic on destruction
now preserve their typed terminal TLS failure through cancellation. Linux
regressions cover both MQTT versions, HTTPS proxies and MQTT 5 redirects,
timeout precedence, pending-operation failure, concurrent and repeated close
callers, private panic payloads and isolation between clients sharing profiles.
Immediate close reports its admitted shutdown failure; joining reports teardown.

The full wrapper-core/C Rust suites and 60 native C fixtures/examples passed.
Advanced TLS tests passed with AWS-LC (21 cases) and Ring without WebSocket
support (20 cases; HTTPS proxy case disabled). The wrapper-core/C each-feature
test and strict Clippy matrices passed all 33 configurations. Workspace
all-target Clippy and Rust 1.88 checks passed. The four TLS-only native advanced
fixtures and signer examples passed with Ring and WebSocket disabled; the
production shared library links neither libssl nor libcrypto.
Earlier sanitizer/leak-tool and
macOS/Windows results do not validate this regression fix; remote CI is pending.

### Graceful-close timeout cancellation regression

The outer runtime now retains the client TLS monitor across cancellation of the
entire backend future. It checks destruction failures after dropping that future
and before reconciling closure. The admitted graceful-close operation keeps its
timeout result; terminal TLS destruction failures produce `DriverTerminated`,
leave the lifecycle `Failed`, and fail pending operations with the typed callback
error and ambiguous delivery status.

Regressions cover pending verification, identity selection and signing across
both MQTT versions, HTTPS proxies and MQTT 5 redirects (15 failing-destructor
scenarios). Six normal-destructor scenarios still finish immediate shutdown on
graceful timeout. Subprocess checks verify that destruction panics remain private.

Current Linux validation: the complete native-wrapper workspace passed 298 tests
with the existing Mosquitto will test ignored. Advanced TLS passed 24 tests with
AWS-LC, TLS 1.2, native TLS and proxies enabled, and 22 tests with Ring and
WebSocket disabled. The native C advanced/deferred TLS and close-options fixtures
passed all three tests. The wrapper-core/C each-feature test matrix passed all
33 configurations. Strict Clippy and Rust 1.88 checks passed across the whole
workspace and all targets with TLS 1.2, native TLS and proxies enabled. Formatting
and diff checks passed. macOS/Windows and remote CI execution remain pending.
