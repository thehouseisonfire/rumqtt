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
| v4, v5 | option | set_client_id, set_transport, set_keep_alive | supported | CommonConfig owns immutable connection inputs | config, config_wire |
| v4 | option | try_set_client_id | supported | Fallible validation before start | config |
| v4, v5 | option | set_auth, clear_auth, set_username, set_credentials | supported | Optional owned username/password | config |
| v5 | option | set_password | supported | Password-only CONNECT is legal only in v5 | config |
| v4 | option | set_clean_session, try_set_clean_session, set_session_mode, try_set_session_mode | supported | V4Config.clean_session and client-id validation | config, session_store |
| v5 | option | set_clean_start, set_session_mode, set_session_expiry_interval | supported | V5Config.clean_start and CONNECT session expiry | config_wire, session_store |
| v4, v5 | option | set_request_channel_capacity, set_max_request_batch, set_read_batch_size, set_pending_throttle | supported | WC-03 distinct channel, request/read batching and throttle controls | backend config_tests, lifecycle |
| v4 | option | set_max_packet_size, set_inflight, try_set_inflight | supported | WC-03 local input limit and v4 output/inflight limits | backend config_tests, value_config |
| v5 | option | set_max_packet_size, set_incoming_packet_size_limit, set_local_incoming_packet_size_limit, set_unlimited_incoming_packet_size, set_outgoing_inflight_upper_limit | supported | WC-03 independent local and advertised limits | backend config_tests, value_config |
| v5 | option | set_connect_properties, set_receive_maximum, set_topic_alias_max, set_request_response_info, set_request_problem_info, set_user_properties, set_authentication_method, set_authentication_data | supported | WC-04 nested V5ConnectProperties preserves presence/order | config_wire, value_config |
| v5 | option | set_topic_alias_policy | supported | Native alias map remains authoritative | backend config_tests, protocol_parity |
| v4, v5 | option | set_ack_mode | supported | Automatic or manual ACK token ownership | protocol_parity |
| v4, v5 | option | set_session_store, set_session_store_arc, clear_session_store, set_session_store_scope, clear_session_store_scope | supported | WC-02 optional owned store and explicit stable scope | session_store, backend session tests |
| v5 | option | set_broker_session_resume_policy | supported | Strict default; explicit AllowBrokerOnly | backend config_tests, session_store |
| v5 | option | set_authenticator, set_auth_manager | supported | WC-05 owned nonblocking callback or per-client SCRAM | authentication |
| v5 | option | set_redirect_policy, clear_redirect_policy | supported | WC-06 Reject or finite isolated-target Follow policy | authentication, value_config |
| v5 | option | set_srv_resolver, clear_srv_resolver | supported | WC-06 owned async resolver or feature-selected system resolver | authentication |
| v4, v5 | option | set_proxy | supported | WC-07 HTTP CONNECT, HTTPS proxy, SOCKS5; native has no SOCKS4 | value_config, config_wire |
| v4, v5 | option | set_request_modifier | supported | WC-09 declarative ordered header edits; arbitrary closures are not exported | value_config, config_wire, tls |
| v4, v5 | option | set_fallible_request_modifier | intentionally omitted | Dynamic handshake callbacks deferred; declarative failures are eager | value_config |
| v4, v5 | option | set_socket_connector | intentionally omitted | WC-08 explicitly allows deferral until native partial-I/O/wakeup contract is sound; Unix is supported | config_wire |
| v4, v5 | option | set_connection_timeout, set_tcp_send_buffer_size, set_tcp_recv_buffer_size, set_tcp_nodelay, set_bind_addr, set_bind_device, set_mptcp | supported | WC-12 network values; unsupported platform-specific settings fail eagerly | backend config_tests, value_config |
| v5 | option | set_network_options, set_connect_timeout | supported | CommonConfig.network plus one total connection-timeout input | backend config_tests |
| v4, v5 | operation | builder | supported | NativeClient.start owns construction/runtime/event-loop lifecycle | config, lifecycle |
| v4, v5 | operation | from_sender, from_senders | intentionally omitted | Foreign channels bypass managed admission/completion; explicitly out of scope | lifecycle |
| v4, v5 | operation | publish, try_publish, publish_tracked, try_publish_tracked | supported | PublishCommand always provides tracked terminal observation | protocol_parity, shutdown, session_store |
| v4, v5 | operation | subscribe, subscribe_many, subscribe_tracked, subscribe_many_tracked, try_subscribe, try_subscribe_many, try_subscribe_tracked, try_subscribe_many_tracked | supported | SubscribeCommand owns one or more filters; local admission differs from SUBACK | protocol_parity |
| v4, v5 | operation | unsubscribe, unsubscribe_many, unsubscribe_tracked, unsubscribe_many_tracked, try_unsubscribe, try_unsubscribe_many, try_unsubscribe_tracked, try_unsubscribe_many_tracked | supported | UnsubscribeCommand owns one or more filters and tracked result | protocol_parity |
| v5 | operation | subscribe_with_properties, subscribe_many_with_properties, subscribe_with_properties_tracked, subscribe_many_with_properties_tracked, try_subscribe_with_properties, try_subscribe_many_with_properties, try_subscribe_with_properties_tracked, try_subscribe_many_with_properties_tracked | supported | Explicit packet and per-filter v5 options | protocol_parity |
| v5 | operation | unsubscribe_with_properties, unsubscribe_many_with_properties, unsubscribe_with_properties_tracked, unsubscribe_many_with_properties_tracked, try_unsubscribe_with_properties, try_unsubscribe_many_with_properties, try_unsubscribe_with_properties_tracked, try_unsubscribe_many_with_properties_tracked | supported | Explicit v5 unsubscribe properties | protocol_parity |
| v4, v5 | operation | ack, try_ack, manual_ack, try_manual_ack, prepare_ack | supported | Generation/client-bound AckToken, ACK flush completion | protocol_parity, acknowledgement unit tests |
| v4, v5 | operation | disconnect, try_disconnect, disconnect_with_timeout, try_disconnect_with_timeout, disconnect_now, try_disconnect_now | supported | Existing graceful/immediate commands and coalescing closer | shutdown, lifecycle |
| v5 | operation | disconnect_with_properties, try_disconnect_with_properties, disconnect_with_properties_timeout, try_disconnect_with_properties_timeout, disconnect_now_with_properties, try_disconnect_now_with_properties | supported | WC-10 first-admitted payload; exact MQTT5 reason/properties | config_wire |
| v5 | operation | reauth, try_reauth, reauth_tracked, try_reauth_tracked | supported | WC-05 tracked Reauthenticate command | authentication |
| v4, v5 | event | Event.Incoming | supported | WC-11 application packets mapped below; ACK packets stay internal to completions | protocol_parity, authentication |
| v4, v5 | event | Event.Outgoing, Outgoing.Publish, Outgoing.Subscribe, Outgoing.Unsubscribe, Outgoing.PubAck, Outgoing.PubRec, Outgoing.PubRel, Outgoing.PubComp, Outgoing.AwaitAck, Outgoing.PingReq, Outgoing.PingResp, Outgoing.Disconnect | supported | Optional OutgoingEvent has coarse activity and packet id where present | backend map_outgoing, lifecycle |
| v5 | event | Outgoing.Auth | supported | Optional outgoing Other activity; auth lifecycle separately observable | authentication |
| v5 | event | Event.Auth, AuthEvent.Started, AuthEvent.Continue, AuthEvent.Succeeded, AuthEvent.Failed | supported | Owned Authentication event; callback owns challenge data | authentication |
| v5 | event | Event.Redirect | supported | Accepted/rejected source/reason/reference; resolved SRV endpoint precedes Connected | authentication |
| v4, v5 | event | Packet.ConnAck, Packet.Publish | supported | Connected details / IncomingPublish with owned properties | config_wire, protocol_parity, authentication |
| v5 | event | Packet.Disconnect | supported | BrokerDisconnect retains properties across poll cleanup | authentication |
| v4 | event | Packet.Disconnect | not applicable | MQTT 3.1.1 servers cannot send DISCONNECT | docs/spec/mqtt-v3.1.1.md |
| v4, v5 | event | Packet.PubAck, Packet.PubRec, Packet.PubRel, Packet.PubComp, Packet.SubAck, Packet.UnsubAck | intentionally omitted | Internal QoS/ACK state; tracked completions are the sole operation-result model | protocol_parity, session_store |
| v4, v5 | event | Packet.Connect, Packet.Subscribe, Packet.Unsubscribe, Packet.PingReq, Packet.PingResp | intentionally omitted | Client-origin packets and keepalive internals have no second raw event stream | protocol_parity, backend event mapping |
| v5 | event | Packet.Auth | intentionally omitted | Raw AUTH challenges belong to the configured authority, lifecycle events stay observable | authentication |
| v4, v5 | operation | disconnect_after_queued, disconnect_after_queued_with_timeout, try_disconnect_after_queued, try_disconnect_after_queued_with_timeout | intentionally omitted | Separate optional ordered-shutdown publish fence is not substituted for the existing protocol-drain close contract; requires a separately named native command/completion policy | shutdown, docs/recipes/ordered-shutdown.md |
| v5 | operation | disconnect_after_queued_with_properties, disconnect_after_queued_with_properties_timeout, try_disconnect_after_queued_with_properties, try_disconnect_after_queued_with_properties_timeout | intentionally omitted | Same optional ordered-shutdown fence decision; ordinary WC-10 payloads are supported without changing ordering semantics | config_wire, shutdown |
