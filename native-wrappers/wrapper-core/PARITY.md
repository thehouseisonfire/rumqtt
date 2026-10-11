# Native client parity inventory

This matrix records public mutable MqttOptions/NetworkOptions setters, all
public synchronous/asynchronous client methods (merged by method name), and
every native Event, Outgoing, and AuthEvent variant. Equivalent builder methods
delegate to these setters. Startup mappings are independent of running clients;
the [runtime applicability audit](runtime-configuration.md) defines the narrow
set of post-start changes and their activation boundaries.
Codec internals are not a native-client API. The source audit parses all feature
branches, including disabled ones, so API additions require an explicit review.

| Protocol | Category | Native names | Status | Wrapper mapping / decision | Test reference |
| --- | --- | --- | --- | --- | --- |
| v4, v5 | option | set_last_will | supported | WC-01 CommonConfig.last_will with explicit protocol properties | config_wire, value_config |
| v4, v5 | option | set_client_id, set_transport, set_keep_alive | supported | CommonConfig owns immutable connection inputs; WC-13 independent TLS backends and ownership | config, tls::tls_input_ownership_is_released_on_every_driver_exit, tls::malformed_tls_credentials_and_alpn_fail_without_network_or_secret_disclosure, tls::valid_pkcs12_with_wrong_password_fails_without_disclosing_identity, tls::platform_roots_validate_an_isolated_process_trust_store, tls::tls_failure_panic_output_is_redacted, transport_composition::disabled_transports_fail_before_opening_a_socket |
| v4, v5 | option | protocol_compatibility_mut, set_protocol_compatibility | supported | Protocol-specific flat V4Config.session_present_mismatch_policy and V5Config.broker_session_resume_policy map into native ProtocolCompatibility without duplicated policy storage; raw/effective state is mapped through existing diagnostics | backend config_tests, protocol_compatibility, session_store, parity_inventory |
| v4 | option | try_set_client_id | supported | Fallible validation before start | config |
| v4, v5 | option | set_auth, clear_auth, set_username, set_credentials | supported | Optional owned username/password; coherent zeroizing next-origin-attempt rotations | config, configuration_update |
| v5 | option | set_password | supported | Password-only CONNECT is legal only in v5 | config |
| v4 | option | set_clean_session, try_set_clean_session, set_session_mode, try_set_session_mode | supported | V4Config.clean_session and client-id validation | config, session_store |
| v5 | option | set_clean_start, set_session_mode, set_session_expiry_interval | supported | V5Config.clean_start and CONNECT session expiry | config_wire, session_store |
| v4, v5 | option | set_request_channel_capacity, set_max_request_batch, set_read_batch_size, set_pending_throttle | supported | WC-03 distinct controls; capacity construction-only, tuning updates at safe poll boundaries | backend config_tests, lifecycle, configuration_update |
| v4 | option | set_max_packet_size, set_inflight, try_set_inflight | supported | WC-03 local input limit and v4 output/inflight limits | runtime_limits::outgoing_inflight_obeys_local_and_broker_limits, runtime_limits::oversized_outgoing_publish_reports_protocol_failure_and_resolves_on_close, runtime_limits::incoming_decoder_limit_is_independent_of_advertised_maximum |
| v5 | option | set_max_packet_size, set_incoming_packet_size_limit, set_local_incoming_packet_size_limit, set_unlimited_incoming_packet_size, set_outgoing_inflight_upper_limit | supported | WC-03 independent local and advertised limits | runtime_limits::outgoing_inflight_obeys_local_and_broker_limits, runtime_limits::oversized_outgoing_publish_reports_protocol_failure_and_resolves_on_close, runtime_limits::incoming_decoder_limit_is_independent_of_advertised_maximum |
| v5 | option | set_connect_properties, set_receive_maximum, set_topic_alias_max, set_request_response_info, set_request_problem_info, set_user_properties, set_authentication_method, set_authentication_data | supported | WC-04 nested V5ConnectProperties preserves presence/order | config_wire, value_config, topic_alias::rejected_connect_properties_preserve_reason_before_first_generation |
| v5 | option | set_topic_alias_policy | supported | WC-04 native alias map remains authoritative across connection generations | topic_alias::automatic_and_explicit_aliases_replay_concrete_topics_under_new_connack_limits, topic_alias::alias_rejection_retains_broker_reason_and_connection_generation, backend::v5::config_tests::unrecoverable_alias_notice_retains_the_native_terminal_reason |
| v4, v5 | option | set_ack_mode | supported | Automatic or manual ACK token ownership | protocol_parity |
| v4, v5 | option | set_session_store, set_session_store_arc, clear_session_store, set_session_store_scope, clear_session_store_scope | supported | WC-02 optional owned store and explicit stable scope | session_store::restart_recovers_mixed_subscriptions_publishes_and_incoming_qos2, session_store::store_completion_races_shutdown_and_abandonment_without_retaining_owner, backend session tests |
| v5 | option | set_broker_session_resume_policy | supported | Strict default; explicit AllowBrokerOnly | backend config_tests, session_store |
| v5 | option | set_authenticator, set_async_authenticator, set_auth_manager | supported | WC-05 owned nonblocking callback or per-client SCRAM | authentication::broker_authentication_method_change_retains_typed_failure, authentication::explicit_callback_rejection_is_terminal_and_releases_owner, authentication::authentication_reconnect_and_pending_challenge_shutdown_release_exchange, authentication::scram_verifies_server_proof_for_initial_authentication_and_reauthentication |
| v5 | option | set_redirect_policy, clear_redirect_policy | supported | WC-06 reject/fixed isolated follow or synchronous application authority with explicit scoped reuse | redirect_policy::redirect_reference_forms_select_isolated_endpoints_for_both_sources, redirect_policy::rejected_redirects_preserve_context_and_resolve_pending_operations, redirect_policy::redirect_loops_and_attempt_exhaustion_are_terminal, redirect_policy::isolated_redirect_never_reads_or_writes_the_origin_store_scope, transport_composition::disabled_redirect_transports_fail_before_driver_start |
| v5 | option | set_srv_resolver, clear_srv_resolver | supported | WC-06 owned async resolver; native seeded weighted-selection tests supplement broker priority coverage | redirect_policy::srv_lookup_failures_have_no_origin_revision_and_release_owner, redirect_policy::srv_priority_precedes_weight_and_selected_endpoint_is_reported; rumqttc-v5 srv::tests::inclusive_zero_draw_can_select_a_zero_weight_record, srv::tests::higher_weight_is_selected_first_materially_more_often |
| v4, v5 | option | set_proxy | supported | WC-07 HTTP CONNECT, HTTPS proxy, SOCKS5; native has no SOCKS4 | transport_composition::proxy_tls_and_websocket_compositions_reconnect_for_both_protocols, transport_composition::proxy_negotiation_failures_and_shutdown_resolve_pending_work, transport_composition::proxy_and_broker_tls_trust_policies_are_independent, transport_composition::proxy_failure_process_output_is_redacted |
| v4, v5 | option | set_request_modifier | supported | WC-09 static edits then bounded dynamic path/query/authority/header edits; isolated redirects clear origin authority | transport_composition::proxy_tls_and_websocket_compositions_reconnect_for_both_protocols, redirect_policy::websocket_redirect_uses_target_uri_and_clears_origin_header_edits, transport_composition::disabled_transports_fail_before_opening_a_socket |
| v4, v5 | option | set_fallible_request_modifier | supported | Owned async callback with explicit URI/Host authority overrides; GET/HTTP1.1, scheme and upgrade fields fixed; native deadline and typed failure | websocket_handshake::dynamic_tokens_and_request_targets_refresh_across_ws_and_wss_reconnects, websocket_handshake::pending_handshakes_use_native_deadline_and_close_cancels_them, websocket_redaction::wrapper_diagnostics_redact_handshake_credentials |
| v4, v5 | option | set_connection_deadline | not applicable | Internal native attempt metadata; generated per attempt, not a host configuration override | custom_transport_memory, backend::transport::tests |
| v4, v5 | option | set_socket_connector | supported | Owned custom base/established streams, explicit socket policy and native deadline; C retained-operation/cancellation contract; terminal failures stop SRV fallback and retain redirect detail | custom_transport, custom_transport_memory, custom_transport_native_tls, custom_transport_redirect, transport_composition, transport::proof, native_custom_transport, native_custom_transport_matrix |
| v4, v5 | option | set_connection_timeout, set_tcp_send_buffer_size, set_tcp_recv_buffer_size, set_tcp_nodelay, set_bind_addr, set_bind_device, set_mptcp | supported | WC-12 network values; unsupported platform-specific settings fail eagerly | platform_network::bind_address_reaches_broker_for_both_protocols, platform_network::bind_device_failure_reaches_socket_and_mptcp_can_connect, platform_network::unsupported_network_controls_fail_before_driver_start; rumqttc-core tests::network_controls_are_applied_to_the_created_socket |
| v5 | option | set_network_options, set_connect_timeout | supported | CommonConfig.network plus one total connection-timeout input | backend config_tests |
| v4, v5 | operation | builder | supported | NativeClient.start owns dedicated construction; start_in retains explicit shared execution with the same driver lifecycle | config, lifecycle, execution, native_execution |
| v4, v5 | operation | from_sender, from_senders | intentionally omitted | Foreign channels bypass managed admission/completion; explicitly out of scope | lifecycle |
| v5 | option | PublishAdmissionPolicy, publish_budget | supported | Strict default; native event-loop validation is selectable. Both modes enforce finite count/data bounds across replay and checkpoint recovery. | publish_admission, native_publish_admission, native publish_budget tests |
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
| v5 | event | Event.Redirect | supported | Accepted/rejected source/reason/reference; resolved SRV endpoint precedes Connected | redirect_policy::redirect_reference_forms_select_isolated_endpoints_for_both_sources, redirect_policy::srv_priority_precedes_weight_and_selected_endpoint_is_reported, redirect_policy::srv_lookup_failures_have_no_origin_revision_and_release_owner |
| v4, v5 | event | Packet.ConnAck, Packet.Publish | supported | Connected details / IncomingPublish with owned properties | config_wire, protocol_parity, authentication |
| v5 | event | Packet.Disconnect | supported | BrokerDisconnect retains properties across poll cleanup | authentication |
| v4 | event | Packet.Disconnect | not applicable | MQTT 3.1.1 servers cannot send DISCONNECT | docs/spec/mqtt-v3.1.1.md |
| v4, v5 | event | Packet.PubAck, Packet.PubRec, Packet.PubRel, Packet.PubComp, Packet.SubAck, Packet.UnsubAck | intentionally omitted | No raw ACK stream; tracked terminal outcomes retain v4 identifiers/SUBACK codes and complete v5 terminal ACK details, including rejection/recovery; successful intermediate PUBREC/PUBREL remain internal | completion unit tests, native_acknowledgement_results, protocol_parity, session_store |
| v4, v5 | event | Packet.Connect, Packet.Subscribe, Packet.Unsubscribe, Packet.PingReq, Packet.PingResp | intentionally omitted | Client-origin packets and keepalive internals have no second raw event stream | protocol_parity, backend event mapping |
| v5 | event | Packet.Auth | intentionally omitted | Raw AUTH challenges belong to the configured authority, lifecycle events stay observable | authentication |
| v4, v5 | operation | disconnect_after_queued, disconnect_after_queued_with_timeout, try_disconnect_after_queued, try_disconnect_after_queued_with_timeout | supported | Optional ordered-shutdown feature; OrderedDisconnect and OrderedShutdown completion delegate to the native fence, with bounded closer and retained cleanup | ordered_shutdown, native_ordered_shutdown, docs/recipes/ordered-shutdown.md |
| v5 | operation | disconnect_after_queued_with_properties, disconnect_after_queued_with_properties_timeout, try_disconnect_after_queued_with_properties, try_disconnect_after_queued_with_properties_timeout | supported | OrderedDisconnectWithOptions owns MQTT 5 DISCONNECT properties; matching closers retain the original deadline | ordered_shutdown, native_ordered_shutdown |

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

## Application redirect authorities (TODO31)

Wrapper-core and C support synchronous advertised-reference approval and explicit
scoped session reuse. Fixed reject/follow policies retain their defaults.
`redirect_policy.rs` exercises both packet sources, later-reference selection,
copied credentials, exact keys/conflicts, permanent origin-lease release, nested
temporary restoration and obsolete target-lease release, QoS 1/2 replay with
original packet IDs, renegotiated aliases, stale manual-ACK tokens,
sync/async effective auth identities, independent auth/header reuse,
panic/invalid/oversized/late responses and reentrant shutdown. Adapter tests reject mismatched native keys
before host I/O and verify transactional target leasing with weak cache pruning.
`native_redirect_authority.c` verifies C copied inputs, retained snapshots,
registration ownership/sharing, stale/invalid choices, typed rejection/timeout/callback
failures, scoped checkpoint failures and cancellation/late completion. C auth contexts
report the replaced target identity, and approved/isolated WebSocket headers are
verified on the wire independently of authentication-authority reuse.
`native_tls_profiles.c` also exercises application TLS/WSS selection after
destroying the selected C profile inside the callback, proving retained policy
through the eventual handshake. Existing native session, manual-ACK,
alias, redirect/SRV, TLS profile and cancellation suites remain the reconciliation
and transport evidence; MQTT reconciliation is never delegated to the application.
The runnable `redirect_authority.c` demonstrates exact allowlist approval.

Requests are inspectable after teardown; response builders cannot be retained or
deferred. The decision budget rejects late results without preempting host code.
There is no chain-wide deadline, replacement enhanced-auth authority, selective
proxy/header reuse or automatic checkpoint migration. Foreign callbacks must not
unwind or block. New native C execution in this change is on Linux; remote CI and
macOS/Windows execution remain pending.

## Execution placement parity

Dedicated startup remains the default. Shared execution is available through
wrapper-core `ExecutionContext`/`NativeClient::start_in` and the standard C
context API. Python has a private `benchmark-testing` hook for the real asyncio
measurements; production Python and JavaScript retain dedicated placement.
`RUMQTTC_TEST_EXECUTION=shared` runs the existing backend behavioral suites with
shared placement, including completions, reconnect, overload, manual ACK,
shutdown, TLS, WebSocket and proxy profiles. Tests permit shared tasks to migrate
between workers. The `execution` suite additionally puts peers on one worker
and verifies capacity, start/shutdown races, peer progress and panic containment.
Native C fixtures verify callback blocking-wait rejection, retained-token
release ordering and unload after explicit context teardown. Pumping and foreign
reactor integration remain unsupported.
## MQTT 5 publish admission (TODO32)

MQTT 5 publish admission adds C policy/limit setters, usage snapshots,
structured local failure reasons and `LOCAL_REJECTED`, preserving C record
layouts and the strict policy default. JavaScript/Python inherit finite budgets
without new selectors. See [the resource and retry contract](publish-admission.md).
Rust tests cover negotiated deferral, QoS 2 lifetime, cancellation, shutdown,
checkpoint count/byte preflight and recovery gating after redirects or public
session-key changes. Continuously active producers retain large payloads and
properties through repeated outages, failed establishment and session resume;
count/data usage stays bounded and ACK progress wakes blocked admission.
Native terminal tests check reclamation before result observation, with dropped
observers, and on sender destruction. Rendezvous and sustained registration
traffic regressions cover admission wakeups and fair driver progress.

`native_publish_admission_offline` executes public C APIs using a held transport
callback: strict default, aliases, channel/count/byte rejection, config isolation,
observer release and shutdown reclamation all run without sockets.
`native_publish_admission` compares both policies before CONNACK, while connected
and during reconnect with QoS/retain combinations, aliases, full channels and
count/byte exhaustion. Broker barriers verify dropped observers, failed alias
binding, QoS 2 through PUBCOMP, changed capabilities, session loss and unsent
alias replay failure. Local failures retain structured reasons/delivery/retry
status without broker reasons, and unrelated valid operations continue.

Linux execution passed the native C suite and examples (63 tests), C ABI/header/
export and C/C++ consumer checks, including the expanded admission fixture.
See [TODO32](../../TODO32.md) for feature-matrix and lint evidence.
macOS/Windows native execution remains in platform CI.

## Core reconnect policy (TODO34)

C and wrapper-core expose classified timing, jitter, finite/unlimited budgets,
stable-connection reset, typed native eligibility and owned retry observations.
Legacy startup remains the default. C additions preserve existing records and
export names; `RECONNECT_EXHAUSTED` distinguishes terminal exhaustion from wait
timeouts and retains the last failure and counters. Python/JavaScript continue
using legacy defaults without new public policy controls.

`reconnect_policy` covers both protocols, retained publish replay/completions,
first-connection exhaustion, backoff close/diagnostics, idle reset, shared-worker
independence, native ordered deadline cleanup, MQTT 5 SRV fallback within
one cycle, and established redirects subject to backoff and budgets.
`reconnect_startup` deterministically admits graceful/immediate shutdown before
the first poll on a shared worker, covering both protocols and policies.
`socks_reconnect` covers typed transient proxy replies, recovery and exhaustion,
and terminal policy/protocol replies in both protocols.
`reconnect_stability` covers MQTT 5 DISCONNECT under event backpressure, including
queued packets, synchronous observations and deferred native error cleanup.
`native_reconnect` and the retry example cover the real C surface,
validation rollback, recovery, exhaustion, retained owned errors/snapshots and
terminal authentication refusal. Native redirect limits and replay remain
client-owned. Optional decision callbacks and pause/resume/request-attempt are
not implemented; controlled reconnect remains deferred under TODO35.

## Runtime configuration (TODO35 first stage)

The [applicability audit](runtime-configuration.md) classifies all setter families.
Wrapper-core and C stage bounded atomic partial updates while retaining native
polls. Tuning and next-origin-attempt profiles have independent desired/effective
revisions; receipts and snapshots retain only redacted observations. Controlled
reconnect and general live option mutation remain deferred. Explicit session
abandonment and fresh recovery are supported as described below.

`configuration_update` verifies v4/v5 idle staging, partial merge/supersession,
rollback, in-progress attempts, retry/authentication failure revisions, temporary
origin restoration and isolated/permanent targets, fresh trust and mutual-TLS
identity/cache rotation, retired startup verifier release with custom connectors,
origin verifier release after permanent retirement while preserving snapshots and
tuning updates, and concurrent updates alongside shared-execution peer
progress. Held target CONNACK tests cover permanent-move success, failure and
cancellation; unit tests verify that prepared origin profiles survive until target
success, alongside input count/byte accounting, receipt termination and startup
password-owner release while connector closures remain alive.
Blocked-destructor tests cover abandoned successful and failed preparations for
both protocols, ensuring auxiliary work remains reserved through owner destruction.
A tuning preparation captured before a permanent move cannot restore retired origin
password owners after it completes.
A capacity-one event queue holds the Redirect event until Connected delivery times
out; origin owners are released before teardown and the receipt remains
`UnavailableAfterRedirect` after the overflow.
A saturated blocking-pool regression holds one preparation and fifteen queued
updates while MQTT v4/v5 transmit graceful or ordered DISCONNECT without a watchdog
deadline. Shutdown resolves configuration completions and finalizes staged receipts
before draining; detached preparation remains tracked until cleanup finishes.
`redirect_policy` also covers SRV error/timeout/unusable-answer attribution without
rewriting origin attempt history; native C `srv-failure` verifies absent origin
revision through the owned error accessor.
The native C `configuration_rotation` example fixture checks old/new credentials
on the wire for both protocols. C-ABI behavior tests cover independent builder,
receipt and snapshot ownership, invalid outputs and unsupported-update rollback.
Execution on macOS/Windows remains pending; Linux verification is recorded in
TODO35.md.

## Explicit session abandonment and recovery

Wrapper-core supports `Command::RecoverSession`, retained `Completion::SessionRecovered`,
operation-local `RecoverySnapshot` and typed `RecoveryFailure`. C exposes both nonblocking
operation-ID and tracked admissions, completion kind 13, size-versioned progress and a failure
accessor. JavaScript/Python only recognize the additional core completion variant internally;
no public host recovery API is added in this slice.

The managed driver uses audited native event-loop coordination and abandonment/establishment
transitions. These hidden support hooks are not exposed as independent reset/drain operations
or raw state/packet-ID/alias mutation. Eligibility is running and disconnected; producers and
shutdown share the barrier. Queued, replayed and protocol-owned work is explicitly retired,
ACK/alias generations invalidated, and pinned durable clearing awaited before fresh CONNECT.
Long-term configuration and ordinary reconnect semantics remain intact. Selective discard,
terminal-client revival, arbitrary live configuration changes and store administration remain
outside this API. See the [C recovery contract](../c/README.md#explicit-session-recovery).

`session_recovery` tests cover candidate quiescence, queued and inflight ownership,
manual ACK/alias invalidation, durable restart, partial clear failures, retained observers,
producer races, retry-budget preservation, redirects and all shutdown modes. Native v4/v5
ownership tests exercise replay, scheduler and both request channels directly.
`native_session_recovery` exercises both C admission forms, typed errors, size/wrong-kind
validation, retained snapshots, clean/persistent wire policy and terminal store clearing
failure. The operator example and recovery fixture are included in every C package profile.

## Structured diagnostics (TODO37)

Wrapper-core `ClientHandle::diagnostics_snapshot()` and C
`rumqttc_client_diagnostics_snapshot()` acquire an owned observation without
operation admission. The single native cache is published during preparation
and after completed native polls, before event delivery. Legacy tracked
`DiagnosticsSnapshot` remains its existing scalar projection and independently
retains its established retry/ordered overlays. Python/JavaScript keep their
existing diagnostics output; the new public language APIs are wrapper-core/C.

| Native field(s) | Classification | Wrapper/C observation |
| --- | --- | --- |
| `connected`, `disconnecting`, `disconnect_complete` | supported | Native status flags; independent of wrapper lifecycle and termination |
| `queues.pending_replay_len`, `queued_len`, `requests_rx_len`, `control_requests_rx_len`, `immediate_disconnect_rx_len` | supported | Separate queue fields; channel lengths are sequential producer observations |
| `queues.pending_len` | redundant, supported | Replay + scheduler; legacy `pending_requests` retains this definition |
| `outbound.inflight`, `max_inflight`, `packet_identifiers_in_use` | supported | Inflight window and reserved identifiers, including non-publish operations |
| `outbound.publish_window_full`, `collision`, `collision_notice`, `outbound_drained` | derived, supported | Named outbound flags; drained excludes inbound ACK state |
| `outbound.pending_subscribe`, `pending_unsubscribe`, `outgoing_publish`, `outgoing_publish_notices` | supported | Pending operations and overlapping publish/notice counts |
| `outbound.outgoing_pubrel`, `outgoing_pubrel_replay`, `outgoing_pubrel_notices` | supported | QoS 2 second-phase and overlapping replay/notice counts |
| `outbound.incoming_puback`, `incoming_pub`, `incoming_pubrec` | supported | Inbound acknowledgement/duplicate tracking; not outgoing completion |
| `session.connack.raw_session_present`, `session_resumed`, `diagnostic` | optional, supported | Presence flag, raw/effective semantics, existing compatibility reason values |
| `session.session_store_configured`, `session_store_loaded`, `session_store_clear_pending`, `local_session_state_matches_client_id` | supported | Store lifecycle and identity agreement; no store owner/checkpoint is exposed |
| MQTT 5 `session.broker_only_session_resume` | protocol-specific, supported | Explicit presence flag; absent for v4 |
| `config.configured_read_batch_size`, `effective_read_batch_size`, `max_request_batch` | supported | Batching at native capture; separate from configuration's dated effective sample |
| MQTT 5 `redirect.selected_reference`, `policy_configured`, `attempts`, `attempt_limit`, `visited_endpoints`, `active`, `target_established`, `reason` | protocol-specific, supported | Current native redirect group, with presence flags and snapshot-owned approved reference |
| MQTT 5 `redirect.srv_owner`, `srv_candidate_index`, `srv_candidate_count`, `srv_current_target` | optional, protocol-specific, supported | Snapshot-owned owner/authority and one-based candidate position |
| `shutdown_phase`, `disconnect_fence_sequence`, `ordered_local_queued_publishes` | feature-specific, supported | Native ordered group, including Open before fence admission; wrapper fence/result is separate |
| `disconnect_deadline` | feature-specific, supported by conversion | Remaining duration at capture; Rust `Instant` representation is intentionally unavailable |
| Mutable protocol objects, packet payloads, session-store owners/checkpoints, credentials/TLS inputs/callback owners | intentionally unavailable | Outside observation scope; retained data has no mutable state or secret owners |

C group metadata reports available, feature-disabled, protocol-inapplicable,
or not-yet-observed data. Identifiers are native capture generation, fence
sequence, retry cycles, configuration revision, effective tuning revision, or
connection attempt, according to the named source. They are not interchangeable
versions. Ages continue increasing while retained values remain immutable.
Native generation/capture time are never refreshed by snapshot assembly or
wrapper overlays. Retry failure summaries preserve typed classifications/context
with a fixed code message, without arbitrary source text or opaque source owners.

`diagnostics` integration tests cover native sample immutability while tuning
stages, activation versus native batching capture, inflight/ACK progress, full
application queues and unchanged overflow deadlines/events, stalled connection
attempts, immediate abort, wrapper fence provenance, and concurrent readers
alongside shared-worker peer MQTT/keepalive/close progress. Mapping tests cover
every native field; C behavior tests cover group absence, borrowed views,
retained lifecycle, output initialization and short/extended records. Unpolled
driver cancellation and error-source owner release have unit regressions.
`reconnect_stability` verifies capture publication before delivery of a recovered
MQTT 5 failure packet blocks. Capture/read cost and retention evidence is recorded
in [diagnostics measurements](benches/diagnostics.md).
