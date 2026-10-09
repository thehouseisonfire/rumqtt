#ifndef RUMQTTC_H
#define RUMQTTC_H

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#  if defined(RUMQTTC_STATIC)
#    define RUMQTTC_API
#  elif defined(RUMQTTC_BUILDING)
#    define RUMQTTC_API __declspec(dllexport)
#  else
#    define RUMQTTC_API __declspec(dllimport)
#  endif
#elif defined(__GNUC__) || defined(__clang__)
#  define RUMQTTC_API __attribute__((visibility("default")))
#else
#  define RUMQTTC_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

#define RUMQTTC_ABI_VERSION_MAJOR 0u
#define RUMQTTC_ABI_VERSION_MINOR 1u
#define RUMQTTC_ABI_VERSION ((RUMQTTC_ABI_VERSION_MAJOR << 16) | RUMQTTC_ABI_VERSION_MINOR)

typedef uint32_t rumqttc_status_t;
#define RUMQTTC_OK 0u
#define RUMQTTC_INVALID_ARGUMENT 1u
#define RUMQTTC_INVALID_STATE 2u
#define RUMQTTC_CONFIG_ERROR 3u
#define RUMQTTC_BACKPRESSURE 4u
#define RUMQTTC_TIMEOUT 5u
#define RUMQTTC_DISCONNECTED 6u
#define RUMQTTC_PROTOCOL_ERROR 7u
#define RUMQTTC_BROKER_REJECTED 8u
#define RUMQTTC_AMBIGUOUS 9u
#define RUMQTTC_INTERNAL_ERROR 10u
#define RUMQTTC_WOULD_BLOCK 11u
#define RUMQTTC_PERSISTENCE_ERROR 12u
#define RUMQTTC_AUTHENTICATION_ERROR 13u
#define RUMQTTC_REDIRECT_ERROR 14u
#define RUMQTTC_WEBSOCKET_HANDSHAKE_ERROR 15u
#define RUMQTTC_LOCAL_REJECTED 16u

typedef uint32_t rumqttc_protocol_t;
#define RUMQTTC_PROTOCOL_V4 1u
#define RUMQTTC_PROTOCOL_V5 2u

#define RUMQTTC_PUBLISH_ADMISSION_REQUIRE_NEGOTIATED_CAPABILITIES 0u
#define RUMQTTC_PUBLISH_ADMISSION_EVENT_LOOP_VALIDATED 1u
#define RUMQTTC_DEFAULT_PUBLISH_MAX_OUTSTANDING 1024u
#define RUMQTTC_DEFAULT_PUBLISH_MAX_BYTES (16u * 1024u * 1024u)

/* Stable local reasons; broker ACK reasons are reported separately. */
#define RUMQTTC_PUBLISH_FAILURE_CAPABILITIES_PENDING 1u
#define RUMQTTC_PUBLISH_FAILURE_RECOVERY_PENDING 2u
#define RUMQTTC_PUBLISH_FAILURE_REQUEST_CHANNEL_FULL 3u
#define RUMQTTC_PUBLISH_FAILURE_COUNT_EXHAUSTED 4u
#define RUMQTTC_PUBLISH_FAILURE_BYTES_EXHAUSTED 5u
#define RUMQTTC_PUBLISH_FAILURE_TOO_LARGE 6u
#define RUMQTTC_PUBLISH_FAILURE_RETAIN_UNAVAILABLE 7u
#define RUMQTTC_PUBLISH_FAILURE_MAXIMUM_QOS 8u
#define RUMQTTC_PUBLISH_FAILURE_TOPIC_ALIAS_ZERO 9u
#define RUMQTTC_PUBLISH_FAILURE_TOPIC_ALIAS_MAXIMUM 10u
#define RUMQTTC_PUBLISH_FAILURE_TOPIC_ALIAS_UNMAPPED 11u
#define RUMQTTC_PUBLISH_FAILURE_TOPIC_ALIAS_REPLAY_UNAVAILABLE 12u
#define RUMQTTC_PUBLISH_FAILURE_SESSION_RESET 13u
#define RUMQTTC_PUBLISH_FAILURE_REDIRECTED 14u
#define RUMQTTC_PUBLISH_FAILURE_BROKER_ONLY_SESSION_RESUME 15u
#define RUMQTTC_PUBLISH_FAILURE_QOS0_NOT_FLUSHED 16u
#define RUMQTTC_PUBLISH_FAILURE_PERSISTENCE 17u
#define RUMQTTC_PUBLISH_FAILURE_RECEIVER_TERMINATED 18u
#define RUMQTTC_PUBLISH_FAILURE_RESTORE_BUDGET_EXCEEDED 19u


/* MQTT packet-type values; these are terminal observations, not ACK commands. */
typedef uint32_t rumqttc_acknowledgement_kind_t;
#define RUMQTTC_ACKNOWLEDGEMENT_PUBACK 4u
#define RUMQTTC_ACKNOWLEDGEMENT_PUBREC 5u
#define RUMQTTC_ACKNOWLEDGEMENT_PUBCOMP 7u
#define RUMQTTC_ACKNOWLEDGEMENT_SUBACK 9u
#define RUMQTTC_ACKNOWLEDGEMENT_UNSUBACK 11u

typedef uint32_t rumqttc_protocol_options_t;
#define RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL 0u
#define RUMQTTC_PROTOCOL_OPTIONS_V5 5u

typedef uint32_t rumqttc_retain_forward_rule_t;
#define RUMQTTC_RETAIN_ON_EVERY_SUBSCRIBE 0u
#define RUMQTTC_RETAIN_ON_NEW_SUBSCRIBE 1u
#define RUMQTTC_RETAIN_NEVER 2u

typedef uint32_t rumqttc_qos_t;
#define RUMQTTC_QOS_0 0u
#define RUMQTTC_QOS_1 1u
#define RUMQTTC_QOS_2 2u

typedef uint32_t rumqttc_ack_mode_t;
#define RUMQTTC_ACK_AUTOMATIC 0u
#define RUMQTTC_ACK_MANUAL 1u

typedef uint32_t rumqttc_event_kind_t;
#define RUMQTTC_EVENT_CONNECTED 1u
#define RUMQTTC_EVENT_DISCONNECTED 2u
#define RUMQTTC_EVENT_INCOMING_PUBLISH 3u
#define RUMQTTC_EVENT_OUTGOING 4u
#define RUMQTTC_EVENT_GRACEFUL_SHUTDOWN 5u
#define RUMQTTC_EVENT_DRIVER_TERMINATED 6u
#define RUMQTTC_EVENT_IMMEDIATE_SHUTDOWN 7u
#define RUMQTTC_EVENT_AUTHENTICATION 8u
#define RUMQTTC_EVENT_REDIRECT 9u
#define RUMQTTC_EVENT_BROKER_DISCONNECT 10u
#define RUMQTTC_EVENT_CONNECTION_REJECTED 11u
#define RUMQTTC_AUTH_EXCHANGE_INITIAL 1u
#define RUMQTTC_AUTH_EXCHANGE_REAUTHENTICATION 2u
#define RUMQTTC_AUTH_STAGE_STARTED 1u
#define RUMQTTC_AUTH_STAGE_CONTINUE 2u
#define RUMQTTC_AUTH_STAGE_SUCCEEDED 3u
#define RUMQTTC_AUTH_STAGE_FAILED 4u
#define RUMQTTC_REDIRECT_SOURCE_CONNACK 1u
#define RUMQTTC_REDIRECT_SOURCE_DISCONNECT 2u
#define RUMQTTC_REDIRECT_REASON_USE_ANOTHER_SERVER 1u
#define RUMQTTC_REDIRECT_REASON_SERVER_MOVED 2u
#define RUMQTTC_REDIRECT_REJECT 0u
#define RUMQTTC_REDIRECT_FOLLOW 1u
#define RUMQTTC_REDIRECT_TRANSPORT_TCP 0u
#define RUMQTTC_REDIRECT_TRANSPORT_TLS 1u
#define RUMQTTC_REDIRECT_TRANSPORT_WS 2u
#define RUMQTTC_REDIRECT_TRANSPORT_WSS 3u
#define RUMQTTC_REDIRECT_TARGET_TCP 1u
#define RUMQTTC_REDIRECT_TARGET_WEBSOCKET 2u
#define RUMQTTC_REDIRECT_DECISION_FOLLOW 1u
#define RUMQTTC_REDIRECT_DECISION_REJECT 2u

/* Disconnect phases returned by rumqttc_event_disconnected. */
#define RUMQTTC_CONNECTION_PHASE_NONE 0u
#define RUMQTTC_CONNECTION_PHASE_ATTEMPT 1u
#define RUMQTTC_CONNECTION_PHASE_ESTABLISHED 2u
#define RUMQTTC_DELIVERY_NOT_APPLICABLE 0u
#define RUMQTTC_DELIVERY_NOT_ADMITTED 1u
#define RUMQTTC_DELIVERY_REJECTED 2u
#define RUMQTTC_DELIVERY_AMBIGUOUS 3u

/* Outgoing activity values returned by rumqttc_event_outgoing_kind. */
#define RUMQTTC_OUTGOING_PUBLISH 1u
#define RUMQTTC_OUTGOING_SUBSCRIBE 2u
#define RUMQTTC_OUTGOING_UNSUBSCRIBE 3u
#define RUMQTTC_OUTGOING_ACKNOWLEDGEMENT 4u
#define RUMQTTC_OUTGOING_PING 5u
#define RUMQTTC_OUTGOING_DISCONNECT 6u
#define RUMQTTC_OUTGOING_AWAIT_ACKNOWLEDGEMENT 7u
#define RUMQTTC_OUTGOING_OTHER 8u

/* MQTT 5 selectors accepted by rumqttc_event_v5_scalar. */
#define RUMQTTC_V5_SCALAR_PAYLOAD_FORMAT 1u
#define RUMQTTC_V5_SCALAR_TOPIC_ALIAS 2u
#define RUMQTTC_V5_SCALAR_MESSAGE_EXPIRY 3u
#define RUMQTTC_CONNACK_SCALAR_SESSION_EXPIRY 1u
#define RUMQTTC_CONNACK_SCALAR_RECEIVE_MAXIMUM 2u
#define RUMQTTC_CONNACK_SCALAR_MAXIMUM_QOS 3u
#define RUMQTTC_CONNACK_SCALAR_RETAIN_AVAILABLE 4u
#define RUMQTTC_CONNACK_SCALAR_MAXIMUM_PACKET_SIZE 5u
#define RUMQTTC_CONNACK_SCALAR_TOPIC_ALIAS_MAXIMUM 6u
#define RUMQTTC_CONNACK_SCALAR_WILDCARD_AVAILABLE 7u
#define RUMQTTC_CONNACK_SCALAR_SUBSCRIPTION_IDS_AVAILABLE 8u
#define RUMQTTC_CONNACK_SCALAR_SHARED_SUBSCRIPTION_AVAILABLE 9u
#define RUMQTTC_CONNACK_SCALAR_SERVER_KEEP_ALIVE 10u
#define RUMQTTC_CONNACK_STRING_ASSIGNED_CLIENT_ID 1u
#define RUMQTTC_CONNACK_STRING_REASON 2u
#define RUMQTTC_CONNACK_STRING_RESPONSE_INFORMATION 3u
#define RUMQTTC_CONNACK_STRING_SERVER_REFERENCE 4u
#define RUMQTTC_CONNACK_STRING_AUTHENTICATION_METHOD 5u
#define RUMQTTC_EVENT_PROPERTIES_CONNACK 1u
#define RUMQTTC_EVENT_PROPERTIES_BROKER_DISCONNECT 2u
#define RUMQTTC_EVENT_PROPERTIES_AUTHENTICATION 3u

typedef uint32_t rumqttc_completion_kind_t;
#define RUMQTTC_COMPLETION_QOS0_FLUSHED 1u
#define RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED 2u
#define RUMQTTC_COMPLETION_QOS2_COMPLETED 3u
#define RUMQTTC_COMPLETION_SUBSCRIBE 4u
#define RUMQTTC_COMPLETION_UNSUBSCRIBE 5u
#define RUMQTTC_COMPLETION_ACKNOWLEDGED 6u
#define RUMQTTC_COMPLETION_DIAGNOSTICS 7u
#define RUMQTTC_COMPLETION_GRACEFUL_SHUTDOWN 8u
#define RUMQTTC_COMPLETION_IMMEDIATE_SHUTDOWN 9u
#define RUMQTTC_COMPLETION_AUTHENTICATED 10u
#define RUMQTTC_COMPLETION_ORDERED_SHUTDOWN 11u

/* Native ordered shutdown failure; operation delivery can remain ambiguous. */
#define RUMQTTC_ORDERED_FAILURE_TIMEOUT 1u
#define RUMQTTC_ORDERED_FAILURE_TRANSPORT 2u
#define RUMQTTC_ORDERED_FAILURE_PROTOCOL 3u
#define RUMQTTC_ORDERED_FAILURE_PERSISTENCE 4u
#define RUMQTTC_ORDERED_FAILURE_PUBLISH 5u
#define RUMQTTC_ORDERED_FAILURE_SUPERSEDED_BY_IMMEDIATE 6u
#define RUMQTTC_ORDERED_FAILURE_SUPERSEDED 7u
#define RUMQTTC_ORDERED_FAILURE_RECEIVER_TERMINATED 8u
#define RUMQTTC_ORDERED_FAILURE_SESSION_RESET 9u
#define RUMQTTC_ORDERED_FAILURE_REDIRECTED 10u
#define RUMQTTC_ORDERED_FAILURE_REPLAY_UNAVAILABLE 11u
#define RUMQTTC_ORDERED_PHASE_OPEN 0u
#define RUMQTTC_ORDERED_PHASE_ADMITTED_DRAIN 1u
#define RUMQTTC_ORDERED_PHASE_APPROACHING 2u
#define RUMQTTC_ORDERED_PHASE_DRAINING 3u
#define RUMQTTC_ORDERED_PHASE_FLUSHING 4u
#define RUMQTTC_ORDERED_PHASE_COMPLETED 5u
#define RUMQTTC_ORDERED_PHASE_TIMED_OUT 6u
#define RUMQTTC_ORDERED_PHASE_FAILED 7u

/* Cached diagnostics, absent before fence admission. local_queued_publishes excludes channels and in-flight state.
 * remaining_at_capture_ms is measured at capture, never an ABI representation of Instant. */
typedef struct rumqttc_ordered_shutdown_diagnostics_t {
    uint32_t struct_size;
    uint32_t phase;
    uint8_t present;
    uint8_t fence_present;
    uint8_t deadline_present;
    uint8_t local_count_present;
    uint32_t reserved;
    uint64_t fence_sequence;
    uint64_t remaining_at_capture_ms;
    uint64_t local_queued_publishes;
    uint64_t snapshot_age_ms;
} rumqttc_ordered_shutdown_diagnostics_t;
#define RUMQTTC_ORDERED_SHUTDOWN_DIAGNOSTICS_INIT \
    { sizeof(rumqttc_ordered_shutdown_diagnostics_t), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 }

typedef uint32_t rumqttc_error_kind_t;
#define RUMQTTC_ERROR_NONE 0u
#define RUMQTTC_ERROR_CONFIGURATION 1u
#define RUMQTTC_ERROR_ADMISSION 2u
#define RUMQTTC_ERROR_BACKPRESSURE 3u
#define RUMQTTC_ERROR_NETWORK 4u
#define RUMQTTC_ERROR_TLS 5u
#define RUMQTTC_ERROR_PROTOCOL 6u
#define RUMQTTC_ERROR_AUTHENTICATION 7u
#define RUMQTTC_ERROR_PERSISTENCE 8u
#define RUMQTTC_ERROR_TIMEOUT 9u
#define RUMQTTC_ERROR_SHUTDOWN 10u
#define RUMQTTC_ERROR_INTERNAL 11u
#define RUMQTTC_STORE_FAILURE_LOAD 1u
#define RUMQTTC_STORE_FAILURE_SAVE 2u
#define RUMQTTC_STORE_FAILURE_CLEAR 3u
#define RUMQTTC_STORE_FAILURE_CORRUPT 4u
#define RUMQTTC_STORE_FAILURE_VERSION 5u
#define RUMQTTC_STORE_FAILURE_PROTOCOL 6u
#define RUMQTTC_STORE_FAILURE_OVERSIZED 7u
#define RUMQTTC_STORE_FAILURE_TIMEOUT 8u
#define RUMQTTC_STORE_FAILURE_PANIC 9u
#define RUMQTTC_STORE_FAILURE_IN_USE 10u
#define RUMQTTC_STORE_FAILURE_PUBLISH_BUDGET_EXCEEDED 12u
#define RUMQTTC_AUTH_FAILURE_REJECTED 1u
#define RUMQTTC_AUTH_FAILURE_PANIC 2u
#define RUMQTTC_AUTH_FAILURE_TIMEOUT 3u
#define RUMQTTC_AUTH_FAILURE_INVALID_RESPONSE 4u
#define RUMQTTC_AUTH_FAILURE_OVERLAPPING 5u
#define RUMQTTC_AUTH_FAILURE_CONNECTION_CLOSED 6u
#define RUMQTTC_AUTH_FAILURE_METHOD 7u
#define RUMQTTC_AUTH_FAILURE_BROKER_REJECTED 8u
#define RUMQTTC_REDIRECT_FAILURE_CALLBACK 1u
#define RUMQTTC_REDIRECT_FAILURE_DISABLED 2u
#define RUMQTTC_REDIRECT_FAILURE_REJECTED 3u
#define RUMQTTC_REDIRECT_FAILURE_INVALID_REFERENCE 4u
#define RUMQTTC_REDIRECT_FAILURE_UNSUPPORTED_TARGET 5u
#define RUMQTTC_REDIRECT_FAILURE_LOOP 6u
#define RUMQTTC_REDIRECT_FAILURE_ATTEMPT_LIMIT 7u
#define RUMQTTC_REDIRECT_FAILURE_DNS 8u
#define RUMQTTC_REDIRECT_FAILURE_TIMEOUT 9u
#define RUMQTTC_REDIRECT_FAILURE_TRANSPORT 10u

/* Bits describe the loaded library. Unknown bits must be ignored. */
#define RUMQTTC_CAP_PROTOCOL_V4 (UINT64_C(1) << 0)
#define RUMQTTC_CAP_PROTOCOL_V5 (UINT64_C(1) << 1)
#define RUMQTTC_CAP_RUSTLS (UINT64_C(1) << 2)
#define RUMQTTC_CAP_NATIVE_TLS (UINT64_C(1) << 3)
#define RUMQTTC_CAP_WEBSOCKET (UINT64_C(1) << 4)
#define RUMQTTC_CAP_HTTP_PROXY (UINT64_C(1) << 5)
#define RUMQTTC_CAP_SOCKS5_PROXY (UINT64_C(1) << 6)
#define RUMQTTC_CAP_UNIX_SOCKET (UINT64_C(1) << 7)
#define RUMQTTC_CAP_SYSTEM_SRV (UINT64_C(1) << 8)
#define RUMQTTC_CAP_SCRAM (UINT64_C(1) << 9)
#define RUMQTTC_CAP_TRACING (UINT64_C(1) << 10)
#define RUMQTTC_CAP_SESSION_STORE_CALLBACKS (UINT64_C(1) << 11)
#define RUMQTTC_CAP_AUTH_CALLBACKS (UINT64_C(1) << 12)
#define RUMQTTC_CAP_TRANSPORT_CALLBACKS (UINT64_C(1) << 13)
#define RUMQTTC_CAP_WEBSOCKET_CALLBACKS (UINT64_C(1) << 14)
#define RUMQTTC_CAP_ORDERED_SHUTDOWN (UINT64_C(1) << 15)
#define RUMQTTC_CAP_SHARED_EXECUTION (UINT64_C(1) << 16)

#define RUMQTTC_TRANSPORT_MAX_TRANSFER 16384u
#define RUMQTTC_TRANSPORT_BASE 1u
#define RUMQTTC_TRANSPORT_ESTABLISHED 2u
#define RUMQTTC_TRANSPORT_NETWORK_APPLIED 1u
#define RUMQTTC_TRANSPORT_NETWORK_NOT_APPLICABLE 2u
#define RUMQTTC_TRANSPORT_READ 1u
#define RUMQTTC_TRANSPORT_WRITE 2u
#define RUMQTTC_TRANSPORT_FLUSH 3u
#define RUMQTTC_TRANSPORT_SHUTDOWN 4u
#define RUMQTTC_TRANSPORT_SUCCESS 0u
#define RUMQTTC_TRANSPORT_FAILURE_CONNECT 1u
#define RUMQTTC_TRANSPORT_FAILURE_NETWORK_OPTIONS 2u
#define RUMQTTC_TRANSPORT_FAILURE_COMPOSITION 3u
#define RUMQTTC_TRANSPORT_FAILURE_INVALID_RESULT 4u
#define RUMQTTC_TRANSPORT_FAILURE_ABANDONED 5u
#define RUMQTTC_TRANSPORT_FAILURE_IO 6u
#define RUMQTTC_TRANSPORT_FAILURE_TIMEOUT 7u
#define RUMQTTC_TRANSPORT_FAILURE_PANIC 8u
#define RUMQTTC_TRANSPORT_FAILURE_RESOURCE_LIMIT 9u


typedef uint32_t rumqttc_tls_backend_t;
#define RUMQTTC_TLS_BACKEND_RUSTLS 0u
#define RUMQTTC_TLS_BACKEND_NATIVE 1u
typedef uint32_t rumqttc_tls_root_policy_t;
#define RUMQTTC_TLS_ROOTS_PLATFORM 0u
#define RUMQTTC_TLS_ROOTS_PEM 1u
#define RUMQTTC_TLS_ROOTS_PLATFORM_AND_PEM 2u

typedef uint32_t rumqttc_tls_version_policy_t;
#define RUMQTTC_TLS_VERSION_DEFAULT 0u
#define RUMQTTC_TLS_VERSION_12_ONLY 1u
#define RUMQTTC_TLS_VERSION_13_ONLY 2u
#define RUMQTTC_TLS_VERSION_12_OR_13 3u

typedef uint32_t rumqttc_tls_pin_target_t;
#define RUMQTTC_TLS_PIN_LEAF_CERTIFICATE 0u
#define RUMQTTC_TLS_PIN_LEAF_SPKI 1u
#define RUMQTTC_TLS_MAX_PINS 32u
#define RUMQTTC_INCOMING_PACKET_LIMIT_DEFAULT 0u
#define RUMQTTC_INCOMING_PACKET_LIMIT_UNLIMITED 1u
#define RUMQTTC_TOPIC_ALIAS_DISABLED 0u
#define RUMQTTC_TOPIC_ALIAS_MONOTONIC 1u
#define RUMQTTC_TOPIC_ALIAS_LRU 2u
#define RUMQTTC_WEBSOCKET_HEADER_ADD 0u
#define RUMQTTC_WEBSOCKET_HEADER_REPLACE 1u
#define RUMQTTC_WEBSOCKET_HEADER_REMOVE 2u
#define RUMQTTC_PROXY_HTTP 1u
#define RUMQTTC_PROXY_HTTPS 2u
#define RUMQTTC_PROXY_SOCKS5 3u
#define RUMQTTC_PROXY_DNS_REMOTE 0u
#define RUMQTTC_STORE_LOAD 1u
#define RUMQTTC_STORE_SAVE 2u
#define RUMQTTC_STORE_CLEAR 3u
#define RUMQTTC_STORE_FOUND 0u
#define RUMQTTC_STORE_NOT_FOUND 1u
#define RUMQTTC_STORE_FAILED 2u
#define RUMQTTC_SRV_SUCCESS 0u
#define RUMQTTC_SRV_FAILED 2u
#define RUMQTTC_BROKER_SESSION_STRICT 0u
#define RUMQTTC_BROKER_SESSION_ALLOW_BROKER_ONLY 1u
#define RUMQTTC_AUTH_REQUEST_START 1u
#define RUMQTTC_AUTH_REQUEST_CONTINUE 2u
#define RUMQTTC_AUTH_REQUEST_SUCCESS 3u
#define RUMQTTC_AUTH_REQUEST_FAILED 4u
#define RUMQTTC_AUTH_ACTION_COMPLETE 0u
#define RUMQTTC_AUTH_ACTION_SEND 1u
#define RUMQTTC_AUTH_ACTION_REJECT 2u

typedef struct rumqttc_execution_context_t rumqttc_execution_context_t;
#define RUMQTTC_EXECUTION_OPEN 0u
#define RUMQTTC_EXECUTION_CLOSING 1u
#define RUMQTTC_EXECUTION_QUIESCENT 2u
typedef struct rumqttc_execution_options_t {
    uint32_t struct_size;
    uint32_t worker_threads;
    uint32_t max_blocking_threads;
    uint32_t reserved;
    size_t client_capacity;
} rumqttc_execution_options_t;
#define RUMQTTC_EXECUTION_OPTIONS_INIT { sizeof(rumqttc_execution_options_t), 2u, 32u, 0u, 1024u }

/* Legacy is the unconfigured default. The initializer opts into classified retries.
 * The initial cycle is free; retry_limit counts subsequently started cycles.
 * Unlimited requires retry_limit=0. Configuration is copied and affects future clients only. */
#define RUMQTTC_RECONNECT_LEGACY 0u
#define RUMQTTC_RECONNECT_CLASSIFIED 1u
#define RUMQTTC_RECONNECT_JITTER_NONE 0u
#define RUMQTTC_RECONNECT_JITTER_FULL 1u
#define RUMQTTC_RECONNECT_BUDGET_FINITE 0u
#define RUMQTTC_RECONNECT_BUDGET_UNLIMITED 1u
#define RUMQTTC_RECONNECT_PHASE_INITIAL 0u
#define RUMQTTC_RECONNECT_PHASE_ATTEMPTING 1u
#define RUMQTTC_RECONNECT_PHASE_CONNECTED 2u
#define RUMQTTC_RECONNECT_PHASE_WAITING 3u
#define RUMQTTC_RECONNECT_PHASE_STOPPED 4u
#define RUMQTTC_RECONNECT_STOP_NONE 0u
#define RUMQTTC_RECONNECT_STOP_SHUTDOWN 1u
#define RUMQTTC_RECONNECT_STOP_TERMINAL_FAILURE 2u
#define RUMQTTC_RECONNECT_STOP_EXHAUSTED 3u

typedef struct rumqttc_reconnect_options_t {
    uint32_t struct_size;
    uint32_t mode;
    uint32_t jitter;
    uint32_t budget_kind;
    uint32_t multiplier;
    uint32_t reserved;
    uint64_t initial_delay_ms;
    uint64_t maximum_delay_ms;
    uint64_t retry_limit;
    uint64_t stability_interval_ms;
} rumqttc_reconnect_options_t;
#define RUMQTTC_RECONNECT_OPTIONS_INIT \
    { sizeof(rumqttc_reconnect_options_t), 1u, 1u, 1u, 2u, 0u, 1000u, 60000u, 0u, 30000u }

/* Relative durations describe capture time. Completion snapshots are immutable;
 * direct client snapshots remain available after termination while the handle lives.
 * Lifetime cycle counts include the initial cycle, but not internal SRV candidate dials. */
typedef struct rumqttc_reconnect_diagnostics_t {
    uint32_t struct_size;
    uint32_t mode;
    uint32_t phase;
    uint32_t budget_kind;
    uint64_t cycles_started;
    uint64_t retries_since_reset;
    uint64_t retry_limit;
    uint64_t reset_count;
    uint8_t delay_present;
    uint8_t stability_present;
    uint8_t last_failure_present;
    uint8_t reserved;
    uint32_t stop_reason;
    uint64_t remaining_delay_at_capture_ms;
    uint64_t remaining_stability_at_capture_ms;
    uint64_t snapshot_age_ms;
} rumqttc_reconnect_diagnostics_t;
#define RUMQTTC_RECONNECT_DIAGNOSTICS_INIT \
    { sizeof(rumqttc_reconnect_diagnostics_t), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 }

typedef struct rumqttc_config_t rumqttc_config_t;
typedef struct rumqttc_tls_profile_t rumqttc_tls_profile_t;
typedef struct rumqttc_tls_verifier_registration_t rumqttc_tls_verifier_registration_t;
typedef struct rumqttc_tls_identity_registration_t rumqttc_tls_identity_registration_t;
typedef struct rumqttc_client_t rumqttc_client_t;
typedef struct rumqttc_event_t rumqttc_event_t;
typedef struct rumqttc_completion_t rumqttc_completion_t;
typedef struct rumqttc_error_t rumqttc_error_t;
typedef struct rumqttc_store_registration_t rumqttc_store_registration_t;
typedef struct rumqttc_callback_completion_t rumqttc_callback_completion_t;
typedef struct rumqttc_resolver_registration_t rumqttc_resolver_registration_t;
typedef struct rumqttc_auth_registration_t rumqttc_auth_registration_t;
typedef struct rumqttc_websocket_registration_t rumqttc_websocket_registration_t;
typedef struct rumqttc_websocket_response_t rumqttc_websocket_response_t;
typedef struct rumqttc_transport_registration_t rumqttc_transport_registration_t;
typedef struct rumqttc_transport_stream_t rumqttc_transport_stream_t;

/*
 * Input views are borrowed only for the duration of a call and are copied
 * before work is queued. {NULL, 0} is valid; NULL with nonzero length is not.
 * Views returned by event/error accessors remain valid until their owner is
 * destroyed and must not be used concurrently with that owner.
 *
 * Every handle returned through **out is owned by the caller and must be
 * released with its matching rumqttc_*_destroy function. Destroy functions
 * accept NULL. Client destruction consumes the handle only on success; after
 * a timeout the handle remains valid for retry. rumqttc_client_abandon is the
 * explicit non-waiting escape hatch. Every optional error_out is initialized
 * to NULL on entry and, on failure, receives a newly owned error when it is
 * non-NULL.
 *
 * Multi-output accessors accept NULL for fields the caller does not need, but
 * require at least one output. Every supplied output is initialized before
 * validation. Single-output accessors continue to require their output.
 */

typedef struct rumqttc_bytes_view_t {
    const uint8_t *data;
    size_t len;
} rumqttc_bytes_view_t;

typedef struct rumqttc_string_view_t {
    const char *data;
    size_t len;
} rumqttc_string_view_t;

typedef struct rumqttc_acknowledgement_details_t {
    uint32_t struct_size;
    rumqttc_protocol_t protocol;
    rumqttc_acknowledgement_kind_t packet_kind;
    uint16_t packet_id;
    uint8_t present;
    uint8_t reason_present;
    uint8_t reason;
    uint8_t properties_present;
    uint8_t recovered;
    uint8_t reserved[5];
} rumqttc_acknowledgement_details_t;

#define RUMQTTC_ACKNOWLEDGEMENT_DETAILS_INIT \
    { sizeof(rumqttc_acknowledgement_details_t), 0, 0, 0, 0, 0, 0, 0, 0, { 0, 0, 0, 0, 0 } }

typedef struct rumqttc_user_property_t {
    uint32_t struct_size;
    rumqttc_string_view_t name;
    rumqttc_string_view_t value;
} rumqttc_user_property_t;

typedef struct rumqttc_v5_will_properties_t {
    uint32_t struct_size;
    uint8_t will_delay_present;
    uint8_t payload_format_present;
    uint8_t message_expiry_present;
    uint8_t content_type_present;
    uint8_t response_topic_present;
    uint8_t correlation_data_present;
    uint8_t reserved[2];
    uint32_t will_delay_interval;
    uint32_t payload_format_indicator;
    uint32_t message_expiry_interval;
    rumqttc_string_view_t content_type;
    rumqttc_string_view_t response_topic;
    rumqttc_bytes_view_t correlation_data;
    const rumqttc_user_property_t *user_properties;
    size_t user_property_count;
} rumqttc_v5_will_properties_t;

typedef struct rumqttc_last_will_t {
    uint32_t struct_size;
    rumqttc_string_view_t topic;
    rumqttc_bytes_view_t payload;
    rumqttc_qos_t qos;
    uint8_t retain;
    uint8_t reserved[3];
    rumqttc_protocol_options_t protocol_options;
    const rumqttc_v5_will_properties_t *v5_properties;
} rumqttc_last_will_t;

typedef struct rumqttc_v5_connect_properties_t {
    uint32_t struct_size;
    uint8_t session_expiry_present;
    uint8_t receive_maximum_present;
    uint8_t maximum_packet_size_present;
    uint8_t topic_alias_maximum_present;
    uint8_t request_response_info_present;
    uint8_t request_problem_info_present;
    uint8_t authentication_method_present;
    uint8_t authentication_data_present;
    uint8_t reserved[4];
    uint32_t session_expiry_interval;
    uint32_t receive_maximum;
    uint32_t maximum_packet_size;
    uint32_t topic_alias_maximum;
    uint8_t request_response_information;
    uint8_t request_problem_information;
    uint8_t reserved_tail[2];
    rumqttc_string_view_t authentication_method;
    rumqttc_bytes_view_t authentication_data;
    const rumqttc_user_property_t *user_properties;
    size_t user_property_count;
} rumqttc_v5_connect_properties_t;

/* Borrowed only during prepare(). Header values are octets, not UTF-8 strings.
 * Names are sorted; duplicate values preserve their order. This is not wire order. */
typedef struct rumqttc_websocket_header_t {
    uint32_t struct_size;
    rumqttc_string_view_t name;
    rumqttc_bytes_view_t value;
} rumqttc_websocket_header_t;

typedef struct rumqttc_websocket_request_t {
    uint32_t struct_size;
    uint32_t protocol;
    uint64_t attempt;
    uint64_t remaining_ns;
    rumqttc_string_view_t method;
    rumqttc_string_view_t version;
    rumqttc_string_view_t uri;
    rumqttc_string_view_t path_and_query;
    rumqttc_string_view_t client_id;
    rumqttc_string_view_t broker_host;
    uint32_t broker_port;
    uint8_t tls_authority_present;
    uint8_t reserved_flags[3];
    rumqttc_string_view_t dial_target;
    rumqttc_string_view_t tls_authority;
    const rumqttc_websocket_header_t *headers;
    size_t header_count;
    uint64_t reserved[2];
} rumqttc_websocket_request_t;

typedef struct rumqttc_websocket_vtable_t {
    uint32_t struct_size;
    void (*prepare)(void *, const rumqttc_websocket_request_t *, rumqttc_callback_completion_t *);
    void (*destroy)(void *);
    uint64_t reserved[2];
} rumqttc_websocket_vtable_t;

#define RUMQTTC_WEBSOCKET_VTABLE_INIT { sizeof(rumqttc_websocket_vtable_t), NULL, NULL, {0, 0} }
#define RUMQTTC_WEBSOCKET_FAILURE_REJECTED 1u
#define RUMQTTC_WEBSOCKET_FAILURE_ABANDONED 2u
#define RUMQTTC_WEBSOCKET_FAILURE_INVALID_RESPONSE 3u
#define RUMQTTC_WEBSOCKET_FAILURE_RESOURCE_LIMIT 4u
#define RUMQTTC_WEBSOCKET_FAILURE_TIMEOUT 5u
#define RUMQTTC_WEBSOCKET_FAILURE_PANIC 6u
#define RUMQTTC_WEBSOCKET_MAX_HEADERS 128u
#define RUMQTTC_WEBSOCKET_MAX_PATH_BYTES 8192u
#define RUMQTTC_WEBSOCKET_MAX_REQUEST_BYTES 65536u
#define RUMQTTC_WEBSOCKET_MAX_RESPONSE_EDITS 256u

typedef struct rumqttc_websocket_header_edit_t {
    uint32_t struct_size;
    uint32_t operation;
    rumqttc_string_view_t name;
    rumqttc_string_view_t value;
    uint64_t reserved[2];
} rumqttc_websocket_header_edit_t;

typedef struct rumqttc_v5_disconnect_properties_t {
    uint32_t struct_size;
    uint32_t reason_code;
    uint8_t session_expiry_present;
    uint8_t reason_string_present;
    uint8_t server_reference_present;
    uint8_t reserved[5];
    uint32_t session_expiry_interval;
    rumqttc_string_view_t reason_string;
    rumqttc_string_view_t server_reference;
    const rumqttc_user_property_t *user_properties;
    size_t user_property_count;
} rumqttc_v5_disconnect_properties_t;

typedef struct rumqttc_disconnect_options_t {
    uint32_t struct_size;
    rumqttc_protocol_options_t protocol_options;
    const rumqttc_v5_disconnect_properties_t *v5_properties;
    uint64_t reserved[2];
} rumqttc_disconnect_options_t;

/* Only client-originated PUBACK/PUBREC reasons are accepted. 0x10 is server-only. */
typedef struct rumqttc_v5_acknowledgement_options_t {
    uint32_t struct_size;
    uint32_t reason_code;
    uint8_t reason_string_present;
    uint8_t reserved[7];
    rumqttc_string_view_t reason_string;
    const rumqttc_user_property_t *user_properties;
    size_t user_property_count;
} rumqttc_v5_acknowledgement_options_t;

typedef struct rumqttc_acknowledgement_options_t {
    uint32_t struct_size;
    uint32_t protocol_options;
    const rumqttc_v5_acknowledgement_options_t *v5_options;
    uint64_t reserved[2];
} rumqttc_acknowledgement_options_t;

typedef struct rumqttc_tls_pem_identity_t {
    uint32_t struct_size;
    uint32_t reserved;
    rumqttc_bytes_view_t certificate;
    rumqttc_bytes_view_t private_key;
    uint64_t reserved_tail[2];
} rumqttc_tls_pem_identity_t;

typedef struct rumqttc_tls_pkcs12_identity_t {
    uint32_t struct_size;
    uint32_t reserved;
    rumqttc_bytes_view_t identity;
    rumqttc_bytes_view_t password;
    uint64_t reserved_tail[2];
} rumqttc_tls_pkcs12_identity_t;

typedef struct rumqttc_tls_options_t {
    uint32_t struct_size;
    rumqttc_tls_backend_t backend;
    rumqttc_tls_root_policy_t root_policy;
    uint32_t reserved;
    rumqttc_bytes_view_t ca_pem;
    const rumqttc_tls_pem_identity_t *pem_identity;
    const rumqttc_tls_pkcs12_identity_t *pkcs12_identity;
    const rumqttc_bytes_view_t *alpn_protocols;
    size_t alpn_protocol_count;
    uint64_t reserved_tail[2];
} rumqttc_tls_options_t;

/* Profile pin digests are SHA-256 of the complete leaf certificate DER or
 * complete DER SubjectPublicKeyInfo. Any one pin may match; standard chain,
 * validity, hostname and handshake-signature checks always remain required. */
typedef struct rumqttc_tls_pin_t {
    uint32_t struct_size;
    rumqttc_tls_pin_target_t target;
    uint8_t sha256[32];
    uint64_t reserved[2];
} rumqttc_tls_pin_t;

typedef struct rumqttc_tls_profile_options_t {
    uint32_t struct_size;
    rumqttc_tls_version_policy_t version_policy;
    const rumqttc_tls_options_t *tls;
    const rumqttc_tls_pin_t *pins;
    size_t pin_count;
    uint64_t reserved[2];
} rumqttc_tls_profile_options_t;

/* Each mask uses bit (1u << selector). Capabilities describe enforceable
 * policies, not availability of a particular protocol on the host or server.
 * Disabled backends return zero masks. */
typedef struct rumqttc_tls_backend_capabilities_t {
    uint32_t struct_size;
    uint32_t version_policy_mask;
    uint32_t root_policy_mask;
    uint32_t pin_target_mask;
    uint64_t reserved[2];
} rumqttc_tls_backend_capabilities_t;

/* Proxy and broker transport/TLS settings are independent. The supported DNS
 * policy resolves the broker hostname at the proxy; numeric broker addresses
 * can be supplied through the ordinary broker setter. Credential byte views
 * must contain UTF-8 because the core proxy clients use text credentials. */
typedef struct rumqttc_tls_advanced_capabilities_t {
  uint32_t struct_size;
  uint32_t sni_policy_mask;
  uint32_t resumption_policy_mask;
  uint32_t feature_mask;
  uint32_t max_signature_bytes;
  uint64_t reserved[2];
} rumqttc_tls_advanced_capabilities_t;
typedef struct rumqttc_tls_profile_extensions_t {
  uint32_t struct_size;
  uint32_t sni_policy;
  uint32_t resumption_policy;
  const uint16_t *cipher_suites;
  size_t cipher_suite_count;
  const struct rumqttc_tls_verifier_registration_t *verifier;
  const struct rumqttc_tls_identity_registration_t *external_identity;
  uint64_t reserved[2];
} rumqttc_tls_profile_extensions_t;
typedef struct rumqttc_tls_verification_request_t {
  uint32_t struct_size;
  uint32_t layer;
  struct rumqttc_string_view_t server_name;
  const struct rumqttc_bytes_view_t *certificates;
  size_t certificate_count;
  struct rumqttc_bytes_view_t ocsp_response;
  uint64_t unix_time;
  uint64_t remaining_ns;
  uint64_t reserved[2];
} rumqttc_tls_verification_request_t;
typedef struct rumqttc_tls_identity_request_t {
  uint32_t struct_size;
  uint32_t layer;
  struct rumqttc_string_view_t server_name;
  const struct rumqttc_bytes_view_t *issuer_hints;
  size_t issuer_hint_count;
  const uint16_t *signature_schemes;
  size_t signature_scheme_count;
  uint64_t remaining_ns;
  uint64_t reserved[2];
} rumqttc_tls_identity_request_t;
typedef struct rumqttc_tls_signing_request_t {
  uint32_t struct_size;
  uint32_t layer;
  struct rumqttc_string_view_t server_name;
  size_t identity_index;
  struct rumqttc_bytes_view_t key_id;
  uint32_t signature_scheme;
  struct rumqttc_bytes_view_t message;
  uint64_t remaining_ns;
  uint64_t reserved[2];
} rumqttc_tls_signing_request_t;
typedef struct rumqttc_tls_verifier_vtable_t {
  uint32_t struct_size;
  uint32_t (*verify)(void*, const struct rumqttc_tls_verification_request_t*);
  void (*destroy)(void*);
  uint64_t reserved[2];
} rumqttc_tls_verifier_vtable_t;
typedef struct rumqttc_tls_identity_vtable_t {
  uint32_t struct_size;
  uint32_t (*select)(void*, const struct rumqttc_tls_identity_request_t*, size_t*);
  uint32_t (*sign)(void*, const struct rumqttc_tls_signing_request_t*, uint8_t*, size_t, size_t*);
  void (*destroy)(void*);
  uint64_t reserved[2];
} rumqttc_tls_identity_vtable_t;
/* Deferred callbacks receive borrowed requests and completion handles.
 * Copy inputs needed later and retain completion before returning. Calls are
 * serialized per handshake on its driver and may overlap across clients.
 * cancel(data, operation_id) runs once for unresolved cancelled work, outside
 * locks; signal cancellation promptly. Dropping the last host token without
 * answering reports ABANDONED. Late/duplicate completions return INVALID_STATE
 * before reading response buffers. Live operations, including retained completed
 * or cancelled tokens, count against max_retained_operations (1..65536).
 * All callbacks/destructors must return promptly, be thread-safe and not unwind.
 * Successful registration owns data; failed construction leaves it with the
 * caller. Destroy runs once after profiles, clients, calls and tokens release.
 * Release all tokens before unloading the library. */
typedef struct rumqttc_tls_async_verifier_vtable_t {
  uint32_t struct_size;
  uint32_t max_retained_operations;
  void (*verify)(void *data, uint64_t operation_id, const rumqttc_tls_verification_request_t *request, rumqttc_callback_completion_t *completion);
  void (*cancel)(void *data, uint64_t operation_id);
  void (*destroy)(void *data);
  uint64_t reserved[2];
} rumqttc_tls_async_verifier_vtable_t;
typedef struct rumqttc_tls_async_identity_vtable_t {
  uint32_t struct_size;
  uint32_t max_retained_operations;
  void (*select)(void *data, uint64_t operation_id, const rumqttc_tls_identity_request_t *request, rumqttc_callback_completion_t *completion);
  void (*sign)(void *data, uint64_t operation_id, const rumqttc_tls_signing_request_t *request, rumqttc_callback_completion_t *completion);
  void (*cancel)(void *data, uint64_t operation_id);
  void (*destroy)(void *data);
  uint64_t reserved[2];
} rumqttc_tls_async_identity_vtable_t;

typedef struct rumqttc_tls_external_identity_t {
  uint32_t struct_size;
  struct rumqttc_bytes_view_t certificate_pem;
  struct rumqttc_bytes_view_t key_id;
  const uint16_t *signature_schemes;
  size_t signature_scheme_count;
  uint64_t reserved[2];
} rumqttc_tls_external_identity_t;

typedef struct rumqttc_proxy_options_t {
    uint32_t struct_size;
    uint32_t protocol;
    uint32_t dns_policy;
    uint32_t reserved;
    rumqttc_string_view_t host;
    uint32_t port;
    rumqttc_bytes_view_t username;
    rumqttc_bytes_view_t password;
    uint8_t credentials_present;
    uint8_t reserved_tail[7];
    const rumqttc_tls_options_t *tls;
} rumqttc_proxy_options_t;

/* Request and its views are borrowed for the callback call. Copy fields needed
 * after returning. The completion is borrowed; retain it for deferred work.
 * Callbacks run on the driver thread, serialized per client, and may overlap
 * across clients. They may admit nonblocking client operations but must not
 * wait for this driver's MQTT progress. Completion may run on another thread.
 * A cancelled, late, or duplicate completion returns INVALID_STATE. The
 * registration's destroy callback runs once on the thread releasing the last
 * owner after all clients, calls, and retained completions release it; release
 * them before unloading the library. Failed registration leaves user_data with
 * the caller. A successful registration owns user_data until destroy.
 * Saves and clears must atomically replace or remove the entire checkpoint. */
typedef struct rumqttc_store_request_t {
    uint32_t struct_size;
    uint32_t operation;
    rumqttc_protocol_t protocol;
    uint32_t checkpoint_format_version;
    rumqttc_string_view_t scope;
    rumqttc_string_view_t client_id;
    rumqttc_bytes_view_t checkpoint;
} rumqttc_store_request_t;

typedef struct rumqttc_store_vtable_t {
    uint32_t struct_size;
    void (*load)(void *user_data, const rumqttc_store_request_t *request, rumqttc_callback_completion_t *completion);
    void (*save)(void *user_data, const rumqttc_store_request_t *request, rumqttc_callback_completion_t *completion);
    void (*clear)(void *user_data, const rumqttc_store_request_t *request, rumqttc_callback_completion_t *completion);
    void (*destroy)(void *user_data);
    uint64_t reserved[2];
} rumqttc_store_vtable_t;

typedef struct rumqttc_resolver_request_t {
    uint32_t struct_size;
    rumqttc_string_view_t owner;
} rumqttc_resolver_request_t;

typedef struct rumqttc_srv_record_t {
    uint32_t struct_size;
    uint32_t priority;
    uint32_t weight;
    uint32_t port;
    uint32_t reserved;
    rumqttc_string_view_t target;
} rumqttc_srv_record_t;

typedef struct rumqttc_resolver_vtable_t {
    uint32_t struct_size;
    void (*resolve)(void *user_data, const rumqttc_resolver_request_t *request, rumqttc_callback_completion_t *completion);
    void (*destroy)(void *user_data);
    uint64_t reserved[2];
} rumqttc_resolver_vtable_t;

/* All request views, including ordered User Properties and secret data, are
 * borrowed only during respond/failed. Retain completion for deferred work;
 * completion may run on another thread. Requests for one client are serialized
 * and may overlap across clients. The failed notification has no completion
 * and must return promptly. Cancellation rejects late completion. */
typedef struct rumqttc_auth_request_t {
    uint32_t struct_size;
    uint32_t exchange;
    uint32_t stage;
    uint8_t reason_code_present;
    uint8_t properties_present;
    uint8_t method_present;
    uint8_t data_present;
    uint8_t reason_string_present;
    uint8_t reserved[3];
    uint32_t reason_code;
    uint64_t generation;
    rumqttc_string_view_t client_id;
    rumqttc_string_view_t method;
    rumqttc_string_view_t auth_method;
    rumqttc_bytes_view_t data;
    rumqttc_string_view_t reason_string;
    const rumqttc_user_property_t *user_properties;
    size_t user_property_count;
} rumqttc_auth_request_t;

typedef struct rumqttc_auth_response_t {
    uint32_t struct_size;
    uint32_t action;
    uint8_t method_present;
    uint8_t data_present;
    uint8_t reason_string_present;
    uint8_t reserved[5];
    rumqttc_string_view_t method;
    rumqttc_bytes_view_t data;
    rumqttc_string_view_t reason_string;
    const rumqttc_user_property_t *user_properties;
    size_t user_property_count;
    uint64_t reserved_tail[2];
} rumqttc_auth_response_t;

typedef struct rumqttc_auth_vtable_t {
    uint32_t struct_size;
    void (*respond)(void *user_data, const rumqttc_auth_request_t *request, rumqttc_callback_completion_t *completion);
    void (*failed)(void *user_data, const rumqttc_auth_request_t *request, uint32_t failure);
    void (*destroy)(void *user_data);
    uint64_t reserved[2];
} rumqttc_auth_vtable_t;

typedef struct rumqttc_v5_publish_properties_t {
    uint32_t struct_size;
    rumqttc_string_view_t response_topic;
    uint8_t response_topic_present;
    uint8_t correlation_data_present;
    uint8_t content_type_present;
    uint8_t payload_format_present;
    rumqttc_bytes_view_t correlation_data;
    rumqttc_string_view_t content_type;
    uint32_t payload_format_indicator;
    uint32_t topic_alias;
    uint8_t message_expiry_present;
    uint8_t reserved[3];
    uint32_t message_expiry_interval;
    const rumqttc_user_property_t *user_properties;
    size_t user_property_count;
} rumqttc_v5_publish_properties_t;

typedef struct rumqttc_publish_options_t {
    uint32_t struct_size;
    rumqttc_qos_t qos;
    uint8_t retain;
    uint8_t reserved[3];
    rumqttc_protocol_options_t protocol_options;
    const rumqttc_v5_publish_properties_t *v5_properties;
} rumqttc_publish_options_t;

typedef struct rumqttc_v5_subscription_options_t {
    uint32_t struct_size;
    uint8_t no_local;
    uint8_t retain_as_published;
    uint8_t reserved[2];
    rumqttc_retain_forward_rule_t retain_forward_rule;
} rumqttc_v5_subscription_options_t;

typedef struct rumqttc_subscription_t {
    uint32_t struct_size;
    rumqttc_string_view_t filter;
    rumqttc_qos_t qos;
    rumqttc_protocol_options_t protocol_options;
    const rumqttc_v5_subscription_options_t *v5_options;
} rumqttc_subscription_t;

typedef struct rumqttc_v5_subscribe_properties_t {
    uint32_t struct_size;
    uint8_t subscription_identifier_present;
    uint8_t reserved[3];
    uint32_t subscription_identifier;
    const rumqttc_user_property_t *user_properties;
    size_t user_property_count;
} rumqttc_v5_subscribe_properties_t;

typedef struct rumqttc_subscribe_options_t {
    uint32_t struct_size;
    rumqttc_protocol_options_t protocol_options;
    const rumqttc_v5_subscribe_properties_t *v5_properties;
} rumqttc_subscribe_options_t;

typedef struct rumqttc_v5_unsubscribe_properties_t {
    uint32_t struct_size;
    const rumqttc_user_property_t *user_properties;
    size_t user_property_count;
} rumqttc_v5_unsubscribe_properties_t;

typedef struct rumqttc_unsubscribe_options_t {
    uint32_t struct_size;
    rumqttc_protocol_options_t protocol_options;
    const rumqttc_v5_unsubscribe_properties_t *v5_properties;
} rumqttc_unsubscribe_options_t;

typedef struct rumqttc_diagnostics_t {
    uint32_t struct_size;
    uint8_t connected;
    uint8_t disconnecting;
    uint8_t outbound_drained;
    uint8_t reserved;
    uint64_t pending_requests;
    uint64_t queued_requests;
    uint32_t inflight_publishes;
    uint32_t max_inflight_publishes;
    uint64_t pending_subscribes;
    uint64_t pending_unsubscribes;
} rumqttc_diagnostics_t;

/* C11/C++17-compatible defaults for every extensible public record. */
#define RUMQTTC_USER_PROPERTY_INIT \
    { sizeof(rumqttc_user_property_t), { NULL, 0 }, { NULL, 0 } }
#define RUMQTTC_V5_WILL_PROPERTIES_INIT \
    { sizeof(rumqttc_v5_will_properties_t), 0, 0, 0, 0, 0, 0, { 0, 0 }, \
      0, 0, 0, { NULL, 0 }, { NULL, 0 }, { NULL, 0 }, NULL, 0 }
#define RUMQTTC_LAST_WILL_INIT \
    { sizeof(rumqttc_last_will_t), { NULL, 0 }, { NULL, 0 }, RUMQTTC_QOS_0, \
      0, { 0, 0, 0 }, RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL, NULL }
#define RUMQTTC_V5_CONNECT_PROPERTIES_INIT \
    { sizeof(rumqttc_v5_connect_properties_t), 0, 0, 0, 0, 0, 0, 0, 0, \
      { 0, 0, 0, 0 }, 0, 0, 0, 0, 0, 0, { 0, 0 }, \
      { NULL, 0 }, { NULL, 0 }, NULL, 0 }
#define RUMQTTC_WEBSOCKET_HEADER_EDIT_INIT \
    { sizeof(rumqttc_websocket_header_edit_t), RUMQTTC_WEBSOCKET_HEADER_ADD, \
      { NULL, 0 }, { NULL, 0 }, { 0, 0 } }
#define RUMQTTC_V5_DISCONNECT_PROPERTIES_INIT \
    { sizeof(rumqttc_v5_disconnect_properties_t), 0, 0, 0, 0, { 0, 0, 0, 0, 0 }, \
      0, { NULL, 0 }, { NULL, 0 }, NULL, 0 }
#define RUMQTTC_DISCONNECT_OPTIONS_INIT \
    { sizeof(rumqttc_disconnect_options_t), RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL, \
      NULL, { 0, 0 } }
#define RUMQTTC_V5_ACKNOWLEDGEMENT_OPTIONS_INIT \
    { sizeof(rumqttc_v5_acknowledgement_options_t), 0, 0, { 0, 0, 0, 0, 0, 0, 0 }, \
      { NULL, 0 }, NULL, 0 }
#define RUMQTTC_ACKNOWLEDGEMENT_OPTIONS_INIT \
    { sizeof(rumqttc_acknowledgement_options_t), RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL, \
      NULL, { 0, 0 } }
#define RUMQTTC_TLS_PEM_IDENTITY_INIT \
    { sizeof(rumqttc_tls_pem_identity_t), 0, { NULL, 0 }, { NULL, 0 }, { 0, 0 } }
#define RUMQTTC_TLS_PKCS12_IDENTITY_INIT \
    { sizeof(rumqttc_tls_pkcs12_identity_t), 0, { NULL, 0 }, { NULL, 0 }, { 0, 0 } }
#define RUMQTTC_TLS_PIN_INIT \
    { sizeof(rumqttc_tls_pin_t), RUMQTTC_TLS_PIN_LEAF_CERTIFICATE, { 0 }, { 0, 0 } }

#define RUMQTTC_TLS_SNI_DEFAULT 0u
#define RUMQTTC_TLS_SNI_ENABLED 1u
#define RUMQTTC_TLS_SNI_DISABLED 2u
#define RUMQTTC_TLS_RESUMPTION_DEFAULT 0u
#define RUMQTTC_TLS_RESUMPTION_DISABLED 1u
#define RUMQTTC_TLS_ADVANCED_CIPHERS 1u
#define RUMQTTC_TLS_ADVANCED_VERIFIER 2u
#define RUMQTTC_TLS_ADVANCED_EXTERNAL_IDENTITY 4u
#define RUMQTTC_TLS_ADVANCED_DEFERRED_VERIFIER 8u
#define RUMQTTC_TLS_ADVANCED_DEFERRED_IDENTITY 16u
#define RUMQTTC_TLS_LAYER_BROKER 0u
#define RUMQTTC_TLS_LAYER_PROXY 1u
#define RUMQTTC_TLS_LAYER_REDIRECT 2u
#define RUMQTTC_TLS_STAGE_VERIFY 0u
#define RUMQTTC_TLS_STAGE_SELECT 1u
#define RUMQTTC_TLS_STAGE_SIGN 2u
#define RUMQTTC_TLS_CALLBACK_OK 0u
#define RUMQTTC_TLS_CALLBACK_REJECTED 1u
#define RUMQTTC_TLS_CALLBACK_FAILED 2u
#define RUMQTTC_TLS_CALLBACK_INVALID_RESPONSE 3u
#define RUMQTTC_TLS_CALLBACK_INVALID_SIGNATURE 4u
#define RUMQTTC_TLS_CALLBACK_RESOURCE_LIMIT 5u
#define RUMQTTC_TLS_CALLBACK_PANIC 6u
#define RUMQTTC_TLS_CALLBACK_TIMEOUT 7u
#define RUMQTTC_TLS_CALLBACK_TRANSIENT 8u
#define RUMQTTC_TLS_CALLBACK_ABANDONED 9u
#define RUMQTTC_TLS_IDENTITY_DECLINE SIZE_MAX
#define RUMQTTC_TLS_PROFILE_EXTENSIONS_INIT \
    { sizeof(rumqttc_tls_profile_extensions_t), 0, 0, NULL, 0, NULL, NULL, { 0, 0 } }
#define RUMQTTC_TLS_ADVANCED_CAPABILITIES_INIT \
    { sizeof(rumqttc_tls_advanced_capabilities_t), 0, 0, 0, 0, { 0, 0 } }
#define RUMQTTC_TLS_ASYNC_VERIFIER_VTABLE_INIT \
    { sizeof(rumqttc_tls_async_verifier_vtable_t), 64, NULL, NULL, NULL, { 0, 0 } }
#define RUMQTTC_TLS_ASYNC_IDENTITY_VTABLE_INIT \
    { sizeof(rumqttc_tls_async_identity_vtable_t), 64, NULL, NULL, NULL, NULL, { 0, 0 } }
#define RUMQTTC_TLS_VERIFIER_VTABLE_INIT \
    { sizeof(rumqttc_tls_verifier_vtable_t), NULL, NULL, { 0, 0 } }
#define RUMQTTC_TLS_IDENTITY_VTABLE_INIT \
    { sizeof(rumqttc_tls_identity_vtable_t), NULL, NULL, NULL, { 0, 0 } }
#define RUMQTTC_TLS_EXTERNAL_IDENTITY_INIT \
    { sizeof(rumqttc_tls_external_identity_t), { NULL, 0 }, { NULL, 0 }, NULL, 0, { 0, 0 } }

#define RUMQTTC_TLS_PROFILE_OPTIONS_INIT \
    { sizeof(rumqttc_tls_profile_options_t), RUMQTTC_TLS_VERSION_DEFAULT, NULL, NULL, 0, { 0, 0 } }
#define RUMQTTC_TLS_BACKEND_CAPABILITIES_INIT \
    { sizeof(rumqttc_tls_backend_capabilities_t), 0, 0, 0, { 0, 0 } }
#define RUMQTTC_TLS_OPTIONS_INIT \
    { sizeof(rumqttc_tls_options_t), RUMQTTC_TLS_BACKEND_RUSTLS, \
      RUMQTTC_TLS_ROOTS_PLATFORM, 0, { NULL, 0 }, NULL, NULL, NULL, 0, { 0, 0 } }
#define RUMQTTC_PROXY_OPTIONS_INIT \
    { sizeof(rumqttc_proxy_options_t), RUMQTTC_PROXY_HTTP, \
      RUMQTTC_PROXY_DNS_REMOTE, 0, { NULL, 0 }, 0, \
      { NULL, 0 }, { NULL, 0 }, 0, { 0, 0, 0, 0, 0, 0, 0 }, NULL }
#define RUMQTTC_STORE_VTABLE_INIT \
    { sizeof(rumqttc_store_vtable_t), NULL, NULL, NULL, NULL, { 0, 0 } }
#define RUMQTTC_RESOLVER_VTABLE_INIT \
    { sizeof(rumqttc_resolver_vtable_t), NULL, NULL, { 0, 0 } }
#define RUMQTTC_AUTH_VTABLE_INIT \
    { sizeof(rumqttc_auth_vtable_t), NULL, NULL, NULL, { 0, 0 } }
#define RUMQTTC_AUTH_RESPONSE_INIT \
    { sizeof(rumqttc_auth_response_t), RUMQTTC_AUTH_ACTION_COMPLETE, 0, 0, 0, \
      { 0, 0, 0, 0, 0 }, { NULL, 0 }, { NULL, 0 }, { NULL, 0 }, NULL, 0, { 0, 0 } }
#define RUMQTTC_SRV_RECORD_INIT \
    { sizeof(rumqttc_srv_record_t), 0, 0, 0, 0, { NULL, 0 } }
#define RUMQTTC_V5_PUBLISH_PROPERTIES_INIT \
    { sizeof(rumqttc_v5_publish_properties_t), { NULL, 0 }, 0, 0, 0, 0, \
      { NULL, 0 }, { NULL, 0 }, 0, 0, 0, { 0, 0, 0 }, 0, NULL, 0 }
#define RUMQTTC_PUBLISH_OPTIONS_INIT \
    { sizeof(rumqttc_publish_options_t), RUMQTTC_QOS_0, 0, { 0, 0, 0 }, \
      RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL, NULL }
#define RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT \
    { sizeof(rumqttc_v5_subscription_options_t), 0, 0, { 0, 0 }, \
      RUMQTTC_RETAIN_ON_EVERY_SUBSCRIBE }
#define RUMQTTC_SUBSCRIPTION_INIT \
    { sizeof(rumqttc_subscription_t), { NULL, 0 }, RUMQTTC_QOS_0, \
      RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL, NULL }
#define RUMQTTC_V5_SUBSCRIBE_PROPERTIES_INIT \
    { sizeof(rumqttc_v5_subscribe_properties_t), 0, { 0, 0, 0 }, 0, NULL, 0 }
#define RUMQTTC_SUBSCRIBE_OPTIONS_INIT \
    { sizeof(rumqttc_subscribe_options_t), \
      RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL, NULL }
#define RUMQTTC_V5_UNSUBSCRIBE_PROPERTIES_INIT \
    { sizeof(rumqttc_v5_unsubscribe_properties_t), NULL, 0 }
#define RUMQTTC_UNSUBSCRIBE_OPTIONS_INIT \
    { sizeof(rumqttc_unsubscribe_options_t), \
      RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL, NULL }
#define RUMQTTC_DIAGNOSTICS_INIT \
    { sizeof(rumqttc_diagnostics_t), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 }

/* v4-only Session Present mismatch policy and committed CONNACK diagnostics. */
#define RUMQTTC_SESSION_PRESENT_MISMATCH_ERROR 0
#define RUMQTTC_SESSION_PRESENT_MISMATCH_ACCEPT_AS_CLEAN 1
#define RUMQTTC_CONNACK_DIAGNOSTIC_NONE 0
#define RUMQTTC_CONNACK_DIAGNOSTIC_SESSION_PRESENT_MISMATCH_ACCEPTED_AS_CLEAN 1
#define RUMQTTC_CONNACK_DIAGNOSTIC_BROKER_ONLY_SESSION_RESUME 2

RUMQTTC_API uint32_t rumqttc_abi_version(void);
RUMQTTC_API const char *rumqttc_library_version(void);
RUMQTTC_API uint64_t rumqttc_library_capabilities(void);

/* Shared execution is opt-in and available in every build. NULL options selects
 * defaults; counts must be nonzero. Every retained handle is independently
 * released. Configurations/clients retain the context, so releasing a handle
 * does not stop attached clients. Final-owner release requests cleanup without
 * waiting; retain an observation handle when explicit teardown is required.
 * Shutdown requests immediate cleanup and rejects later starts. Join requires
 * shutdown first; TIMEOUT only ends observation, and cleanup remains retryable.
 * QUIESCENT follows successful join of all context execution, including native
 * blocking work. It does not release host-retained tokens, configurations,
 * streams, registrations, or host workers. Release/join all independent owners
 * before unloading the library. Client joining does not stop context peers.
 * Nonzero blocking waits and context construction are forbidden in callbacks
 * and on context workers; nonblocking admission/polling/shutdown remain allowed.
 * A synchronous callback cannot be preempted by a deadline. */
RUMQTTC_API rumqttc_status_t rumqttc_execution_context_new(const rumqttc_execution_options_t *options, rumqttc_execution_context_t **out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_execution_context_retain(const rumqttc_execution_context_t *context, rumqttc_execution_context_t **out, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_execution_context_release(rumqttc_execution_context_t *context);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_execution_context(rumqttc_config_t *config, const rumqttc_execution_context_t *context, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_execution_context(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_execution_context_request_shutdown(const rumqttc_execution_context_t *context, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_execution_context_state(const rumqttc_execution_context_t *context, uint32_t *out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_execution_context_try_join(const rumqttc_execution_context_t *context, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_execution_context_join_timeout_ms(const rumqttc_execution_context_t *context, uint64_t timeout_ms, rumqttc_error_t **error_out);

RUMQTTC_API rumqttc_status_t rumqttc_config_new(rumqttc_protocol_t protocol, rumqttc_config_t **out, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_config_destroy(rumqttc_config_t *config);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_broker(rumqttc_config_t *config, rumqttc_string_view_t host, uint16_t port, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_client_id(rumqttc_config_t *config, rumqttc_string_view_t client_id, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_username(rumqttc_config_t *config, rumqttc_string_view_t username, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_username(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_password(rumqttc_config_t *config, rumqttc_bytes_view_t password, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_password(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_transport_tcp(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_transport_tls(rumqttc_config_t *config, rumqttc_bytes_view_t ca, rumqttc_bytes_view_t certificate, rumqttc_bytes_view_t private_key, rumqttc_error_t **error_out);
/* Explicit TLS backend and trust policy. All inputs are copied before return.
 * PEM roots replace platform trust; PLATFORM_AND_PEM augments it.
 * Private key, PKCS#12 data, and password
 * are wiped when wrapper-owned copies are dropped; caller and TLS-library copies
 * have independent lifetimes. The old TLS setter always selects Rustls. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_transport_tls_with_options(rumqttc_config_t *config, const rumqttc_tls_options_t *options, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_transport_websocket(rumqttc_config_t *config, rumqttc_string_view_t url, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_transport_wss(rumqttc_config_t *config, rumqttc_string_view_t url, rumqttc_bytes_view_t ca, rumqttc_bytes_view_t certificate, rumqttc_bytes_view_t private_key, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_transport_wss_with_options(rumqttc_config_t *config, rumqttc_string_view_t url, const rumqttc_tls_options_t *options, rumqttc_error_t **error_out);
/* Immutable TLS profiles copy all inputs and validate credentials/policy without
 * networking. NULL options.tls selects default Rustls/platform roots. Platform
 * trust is consulted at validation and again at client start. Configuration
 * setters take independent owned copies; profiles may be destroyed immediately
 * afterwards and reused across configurations. Destruction must not race access.
 * Zero pins disables pinning; otherwise pin_count must be at most MAX_PINS.
 * Pins are Rustls-only and disable TLS resumption to revalidate each reconnect.
 * No callback, key export, or global provider installation is performed. */
/* Callbacks are synchronous, thread-safe and must not unwind. All views are
 * borrowed for the call only. Successful registration takes data ownership;
 * destroy runs once after the last profile/configuration/handshake reference.
 * Return CALLBACK_OK, REJECTED, FAILED, TIMEOUT or TRANSIENT. select sets an
 * index or IDENTITY_DECLINE; sign writes the unhashed-message signature into
 * the supplied buffer (at most 4096 bytes). Remaining time is advisory: callbacks
 * must return promptly and may not wait for their own driver to make progress.
 * Verification supplements standard trust/name/signature validation and pins.
 * Pins and callbacks disable resumption. SNI never changes hostname checks.
 * Zero cipher count retains defaults. Algorithms use IANA u16 identifiers.
 * Queries accept NULL/zero to retrieve count; insufficient capacity writes no
 * elements and returns INVALID_ARGUMENT with the required count.
 */
/* Deferred registrations use the existing profile extension fields. Native TLS
 * rejects these registrations when constructing a profile. Answers share the
 * original connection deadline. Hooks disable resumption.
 * Complete with OK/REJECTED/FAILED/TIMEOUT/TRANSIENT; SIZE_MAX deliberately
 * declines selection. Successful signatures are copied and cryptographically
 * checked against the selected leaf. Invalid responses leave a pending token
 * available for correction; a successful completion consumes its answer once. */
RUMQTTC_API uint32_t rumqttc_tls_verifier_registration_new_async(const rumqttc_tls_async_verifier_vtable_t *table,
    void *data, rumqttc_tls_verifier_registration_t **out, rumqttc_error_t **error_out);
RUMQTTC_API uint32_t rumqttc_tls_identity_registration_new_async(const rumqttc_tls_async_identity_vtable_t *table,
    void *data, const rumqttc_tls_external_identity_t *identities, size_t count,
    rumqttc_tls_identity_registration_t **out, rumqttc_error_t **error_out);
RUMQTTC_API uint32_t rumqttc_callback_tls_verify_complete(rumqttc_callback_completion_t *token, uint32_t reason);
RUMQTTC_API uint32_t rumqttc_callback_tls_select_complete(rumqttc_callback_completion_t *token, uint32_t reason, size_t index);
RUMQTTC_API uint32_t rumqttc_callback_tls_sign_complete(rumqttc_callback_completion_t *token, uint32_t reason, rumqttc_bytes_view_t signature);

RUMQTTC_API uint32_t rumqttc_tls_verifier_registration_new(const struct rumqttc_tls_verifier_vtable_t *vtable,
                                               void *data,
                                               struct rumqttc_tls_verifier_registration_t **out,
                                               struct rumqttc_error_t **error_out);

RUMQTTC_API void rumqttc_tls_verifier_registration_destroy(struct rumqttc_tls_verifier_registration_t *registration);

RUMQTTC_API uint32_t rumqttc_tls_identity_registration_new(const struct rumqttc_tls_identity_vtable_t *vtable,
                                               void *data,
                                               const struct rumqttc_tls_external_identity_t *identities,
                                               size_t identity_count,
                                               struct rumqttc_tls_identity_registration_t **out,
                                               struct rumqttc_error_t **error_out);

RUMQTTC_API void rumqttc_tls_identity_registration_destroy(struct rumqttc_tls_identity_registration_t *registration);

RUMQTTC_API uint32_t rumqttc_tls_profile_new_with_extensions(const struct rumqttc_tls_profile_options_t *options,
                                                 const struct rumqttc_tls_profile_extensions_t *extensions,
                                                 struct rumqttc_tls_profile_t **out,
                                                 struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_tls_advanced_capabilities(uint32_t backend,
                                           struct rumqttc_tls_advanced_capabilities_t *out,
                                           struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_tls_supported_cipher_suites(uint32_t backend,
                                             uint16_t *out,
                                             size_t capacity,
                                             size_t *count,
                                             struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_tls_supported_signature_schemes(uint32_t backend,
                                                 uint16_t *out,
                                                 size_t capacity,
                                                 size_t *count,
                                                 struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_error_tls_callback_failure(const struct rumqttc_error_t *error,
                                            uint8_t *present,
                                            uint32_t *stage,
                                            uint32_t *reason,
                                            uint32_t *layer);

RUMQTTC_API rumqttc_status_t rumqttc_tls_backend_capabilities(rumqttc_tls_backend_t backend, rumqttc_tls_backend_capabilities_t *out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_tls_profile_new(const rumqttc_tls_profile_options_t *options, rumqttc_tls_profile_t **out, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_tls_profile_destroy(rumqttc_tls_profile_t *profile);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_transport_tls_with_profile(rumqttc_config_t *config, const rumqttc_tls_profile_t *profile, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_transport_wss_with_profile(rumqttc_config_t *config, rumqttc_string_view_t url, const rumqttc_tls_profile_t *profile, rumqttc_error_t **error_out);
/* Requires HTTPS and options.tls == NULL; only the proxy layer gets this profile. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_proxy_with_tls_profile(rumqttc_config_t *config, const rumqttc_proxy_options_t *options, const rumqttc_tls_profile_t *profile, rumqttc_error_t **error_out);
/* Enables finite, isolated MQTT 5 redirects with TLS (1) or WSS (3).
 * Use the existing redirect setter to disable following. Origin policy is not inherited. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_redirect_policy_with_tls_profile(rumqttc_config_t *config, uint32_t max_attempts, uint32_t transport, const rumqttc_tls_profile_t *profile, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_proxy(rumqttc_config_t *config, const rumqttc_proxy_options_t *options, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_proxy(rumqttc_config_t *config, rumqttc_error_t **error_out);
/* Custom transport contract. All records are size-versioned; initialize
 * reserved fields to zero. Registration creation copies the vtable and takes
 * user_data ownership only on success. Configurations, streams, and retained
 * completions independently keep their registration alive.
 *
 * BASE supplies raw bytes before native proxy/TLS/WebSocket negotiation.
 * ESTABLISHED supplies MQTT-ready bytes and requires TCP with no native proxy,
 * TLS or WebSocket layers (including redirect profiles). A successful connect
 * must acknowledge socket settings as APPLIED or, only when no settings were
 * requested, NOT_APPLICABLE. Native code never configures a foreign socket.
 *
 * Connect receives the actual dial target (a proxy endpoint when applicable),
 * a client-local attempt generation, a registration-wide operation_id, and the
 * remaining native connection deadline in nanoseconds at callback entry.
 * Deadline budget includes subsequent negotiation; callbacks must not block.
 *
 * Each stream permits one read and one serialized write/flush/shutdown.
 * Read requests and buffered writes are bounded to 16384 bytes. A successful
 * zero-length read means EOF. A write must report 1..input.len bytes; short
 * transfers are legal. Write acceptance is buffered; flush or the next write
 * observes host errors. Shutdown drains and flushes writes before closing.
 *
 * Request records and metadata views are borrowed only during the callback.
 * Retain the borrowed completion before returning to finish on another thread.
 * Retaining a write completion also retains its input bytes. All read results
 * are copied during completion. Never destroy the callback's borrowed handle.
 * Destroy each retained completion after work finishes. Dropping the last host
 * token without completing wakes the observer with ABANDONED.
 *
 * cancel(user_data, operation_id) runs after cancellation outside locks. It
 * must promptly stop/wake host work; retained tokens and buffers remain valid
 * until the host releases them. Late/duplicate completions return INVALID_STATE
 * before inspecting response views. Streams are single-use, even after failed
 * construction. Each registration bounds live operations (including cancelled
 * retained work) with max_retained_operations, from 1 to 65536. Retain clones
 * refer to the same operation and do not consume another slot.
 *
 * Callbacks may overlap across clients and read/write directions and must be
 * thread-safe, return promptly, and permit reentrant nonblocking admission.
 * Never wait for MQTT completion on the driver thread. Destructors must not
 * block or unwind. Release all foreign owners before unloading the library.
 */
typedef struct rumqttc_transport_connect_request_t {
  uint32_t struct_size;
  uint32_t protocol;
  uint64_t generation;
  uint64_t operation_id;
  uint64_t remaining_timeout_ns;
  struct rumqttc_string_view_t target;
  struct rumqttc_string_view_t client_id;
  uint8_t send_buffer_present;
  uint8_t receive_buffer_present;
  uint8_t tcp_nodelay;
  uint8_t mptcp;
  uint8_t reserved_flags[4];
  uint32_t send_buffer_size;
  uint32_t receive_buffer_size;
  struct rumqttc_string_view_t local_address;
  struct rumqttc_string_view_t bind_device;
  uint64_t reserved[2];
} rumqttc_transport_connect_request_t;

typedef struct rumqttc_transport_io_request_t {
  uint32_t struct_size;
  uint32_t operation;
  uint64_t generation;
  uint64_t operation_id;
  size_t read_limit;
  struct rumqttc_bytes_view_t input;
  uint64_t reserved[2];
} rumqttc_transport_io_request_t;

typedef struct rumqttc_transport_vtable_t {
  uint32_t struct_size;
  uint32_t mode;
  uint32_t max_retained_operations;
  uint32_t reserved_flags;
  void (*connect)(void*,
                  const struct rumqttc_transport_connect_request_t*,
                  struct rumqttc_callback_completion_t*);
  void (*cancel)(void*, uint64_t);
  void (*destroy)(void*);
  uint64_t reserved[2];
} rumqttc_transport_vtable_t;

typedef struct rumqttc_transport_stream_vtable_t {
  uint32_t struct_size;
  uint32_t mode;
  void (*perform)(void*,
                  const struct rumqttc_transport_io_request_t*,
                  struct rumqttc_callback_completion_t*);
  void (*cancel)(void*, uint64_t);
  void (*destroy)(void*);
  uint64_t reserved[2];
} rumqttc_transport_stream_vtable_t;

typedef struct rumqttc_transport_response_t {
  uint32_t struct_size;
  uint32_t result;
  const struct rumqttc_transport_stream_t *stream;
  uint32_t network_handling;
  uint32_t reserved_flags;
  struct rumqttc_bytes_view_t bytes;
  size_t count;
  uint64_t reserved[2];
} rumqttc_transport_response_t;

#define RUMQTTC_TRANSPORT_VTABLE_INIT \
    { sizeof(rumqttc_transport_vtable_t), RUMQTTC_TRANSPORT_BASE, 256u, 0u, NULL, NULL, NULL, {0, 0} }
#define RUMQTTC_TRANSPORT_STREAM_VTABLE_INIT \
    { sizeof(rumqttc_transport_stream_vtable_t), RUMQTTC_TRANSPORT_BASE, NULL, NULL, NULL, {0, 0} }
#define RUMQTTC_TRANSPORT_RESPONSE_INIT \
    { sizeof(rumqttc_transport_response_t), RUMQTTC_TRANSPORT_SUCCESS, NULL, 0u, 0u, {NULL, 0}, 0, {0, 0} }

RUMQTTC_API rumqttc_status_t rumqttc_transport_registration_new(const struct rumqttc_transport_vtable_t *vtable,
                                            void *user_data,
                                            struct rumqttc_transport_registration_t **out,
                                            struct rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_transport_registration_destroy(struct rumqttc_transport_registration_t *registration);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_transport_connector(struct rumqttc_config_t *config,
                                                const struct rumqttc_transport_registration_t *registration,
                                                struct rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_transport_connector(struct rumqttc_config_t *config,
                                                  struct rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_transport_stream_new(const struct rumqttc_callback_completion_t *completion,
                                      const struct rumqttc_transport_stream_vtable_t *vtable,
                                      void *user_data,
                                      struct rumqttc_transport_stream_t **out,
                                      struct rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_transport_stream_destroy(struct rumqttc_transport_stream_t *stream);
RUMQTTC_API rumqttc_status_t rumqttc_callback_transport_complete(struct rumqttc_callback_completion_t *completion,
                                             const struct rumqttc_transport_response_t *response);
RUMQTTC_API rumqttc_status_t rumqttc_error_transport_failure(const struct rumqttc_error_t *error,
                                         uint8_t *present_out,
                                         uint32_t *failure_out);

RUMQTTC_API rumqttc_status_t rumqttc_store_registration_new(const rumqttc_store_vtable_t *vtable, void *user_data, rumqttc_store_registration_t **out, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_store_registration_destroy(rumqttc_store_registration_t *registration);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_session_store(rumqttc_config_t *config, const rumqttc_store_registration_t *registration, rumqttc_string_view_t scope, uint64_t timeout_ms, size_t max_checkpoint_size, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_session_store(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v4_session_present_mismatch_policy(rumqttc_config_t *config, uint32_t policy, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_broker_session_resume_policy(rumqttc_config_t *config, uint32_t policy, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_redirect_policy(rumqttc_config_t *config, uint32_t policy, uint32_t max_attempts, uint32_t transport, const rumqttc_tls_options_t *tls, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_scram(rumqttc_config_t *config, rumqttc_string_view_t username, rumqttc_bytes_view_t password, uint64_t timeout_ms, uint32_t max_iterations, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_v5_scram(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_auth_registration_new(const rumqttc_auth_vtable_t *vtable, void *user_data, rumqttc_auth_registration_t **out, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_auth_registration_destroy(rumqttc_auth_registration_t *registration);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_authenticator(rumqttc_config_t *config, const rumqttc_auth_registration_t *registration, rumqttc_string_view_t method, uint64_t timeout_ms, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_v5_authenticator(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_callback_completion_retain(const rumqttc_callback_completion_t *completion, rumqttc_callback_completion_t **out);
RUMQTTC_API void rumqttc_callback_completion_destroy(rumqttc_callback_completion_t *completion);
/* A found checkpoint above this client's max_checkpoint_size is rejected before
 * its bytes are read or copied. RUMQTTC_OK means the completion was accepted;
 * the client then reports a persistence/oversized failure. */
RUMQTTC_API rumqttc_status_t rumqttc_callback_store_load_complete(rumqttc_callback_completion_t *completion, uint32_t result, rumqttc_bytes_view_t checkpoint);
RUMQTTC_API rumqttc_status_t rumqttc_callback_store_write_complete(rumqttc_callback_completion_t *completion, uint32_t result);
RUMQTTC_API rumqttc_status_t rumqttc_resolver_registration_new(const rumqttc_resolver_vtable_t *vtable, void *user_data, rumqttc_resolver_registration_t **out, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_resolver_registration_destroy(rumqttc_resolver_registration_t *registration);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_srv_resolver(rumqttc_config_t *config, const rumqttc_resolver_registration_t *registration, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_v5_srv_resolver(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_callback_srv_complete(rumqttc_callback_completion_t *completion, uint32_t result, const rumqttc_srv_record_t *records, size_t record_count);
RUMQTTC_API rumqttc_status_t rumqttc_callback_auth_complete(rumqttc_callback_completion_t *completion, const rumqttc_auth_response_t *response);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_keep_alive_seconds(rumqttc_config_t *config, uint64_t seconds, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_connection_timeout_seconds(rumqttc_config_t *config, uint64_t seconds, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_request_capacity(rumqttc_config_t *config, uint32_t capacity, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_event_capacity(rumqttc_config_t *config, uint32_t capacity, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_event_delivery_timeout_ms(rumqttc_config_t *config, uint64_t milliseconds, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_ack_mode(rumqttc_config_t *config, rumqttc_ack_mode_t mode, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_incoming_packet_limit(rumqttc_config_t *config, uint32_t bytes, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_emit_outgoing_events(rumqttc_config_t *config, uint8_t enabled, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v4_clean_session(rumqttc_config_t *config, uint8_t clean_session, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_session(rumqttc_config_t *config, uint8_t clean_start, uint8_t expiry_present, uint32_t expiry_seconds, rumqttc_error_t **error_out);
/* Copies every supplied view. V5 selector/properties require a V5 config. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_last_will(rumqttc_config_t *config, const rumqttc_last_will_t *will, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_last_will(rumqttc_config_t *config, rumqttc_error_t **error_out);
/* Zero means legacy single-request processing or adaptive read batching. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_max_request_batch(rumqttc_config_t *config, uint32_t count, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_read_batch_size(rumqttc_config_t *config, uint32_t count, rumqttc_error_t **error_out);
/* Zero disables pending retransmission throttling. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_pending_throttle_us(rumqttc_config_t *config, uint64_t microseconds, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_local_incoming_packet_limit_bytes(rumqttc_config_t *config, uint32_t bytes, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_local_incoming_packet_limit_mode(rumqttc_config_t *config, uint32_t mode, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v4_outgoing_packet_limit_bytes(rumqttc_config_t *config, uint64_t bytes, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_reset_v4_outgoing_packet_limit(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v4_inflight_limit(rumqttc_config_t *config, uint16_t limit, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_advertised_max_packet_size_bytes(rumqttc_config_t *config, uint32_t bytes, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_v5_advertised_max_packet_size(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_outgoing_inflight_upper_limit(rumqttc_config_t *config, uint16_t limit, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_v5_outgoing_inflight_upper_limit(rumqttc_config_t *config, rumqttc_error_t **error_out);
/* Explicit presence flags preserve absent and present-empty fields. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_connect_properties(rumqttc_config_t *config, const rumqttc_v5_connect_properties_t *properties, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_v5_connect_properties(rumqttc_config_t *config, rumqttc_error_t **error_out);
/* MQTT 5 only. Configuration edits affect future clients; limits must be nonzero.
 * The strict admission default is unchanged. Event-loop validation defers negotiated
 * rejection while connected as well as offline. Admission is neither broker ACK nor durability. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_publish_admission_policy(rumqttc_config_t *config, uint32_t policy, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_publish_budget(rumqttc_config_t *config, size_t max_outstanding, size_t max_bytes, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_reset_v5_publish_budget(rumqttc_config_t *config, rumqttc_error_t **error_out);
/* Coherent native usage; all scalar outputs are optional, but at least one is required.
 * Counters include restored PUBLISH/PUBREL work and survive reconnect queue transfers.
 * Bytes charge retained publish data, not process RSS. Outputs initialize to zero on failure. */
RUMQTTC_API rumqttc_status_t rumqttc_client_v5_publish_budget_snapshot(rumqttc_client_t *client, size_t *outstanding_out, size_t *retained_bytes_out, size_t *max_outstanding_out, size_t *max_bytes_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_v5_topic_alias_policy(rumqttc_config_t *config, uint32_t policy, rumqttc_error_t **error_out);
/* Unix path bytes use the platform's native Unix encoding; no UTF-8 conversion. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_unix_broker(rumqttc_config_t *config, rumqttc_bytes_view_t path, rumqttc_error_t **error_out);
/* prepare() runs once after static edits on each prepared attempt. Return promptly;
 * copy request views for deferred work, retain the borrowed completion, and destroy
 * each retained handle. Do not wait for MQTT progress from the same driver.
 * One completion wins. Late/duplicate calls return INVALID_STATE before reading
 * response inputs. Dropping the last host token abandons pending work.
 * Owners survive reconnects and retained tokens; destroy(user_data) runs once
 * after all registrations/configurations/clients/tokens release their references.
 * Failed registration leaves user_data with the caller. */
RUMQTTC_API rumqttc_status_t rumqttc_websocket_registration_new(const rumqttc_websocket_vtable_t *vtable, void *data, rumqttc_websocket_registration_t **out, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_websocket_registration_destroy(rumqttc_websocket_registration_t *registration);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_websocket_handshake(rumqttc_config_t *config, const rumqttc_websocket_registration_t *registration, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_websocket_handshake(rumqttc_config_t *config, rumqttc_error_t **error_out);
/* Builders copy inputs and serialize concurrent edits. Destruction must not race
 * use. Completion copies the builder; success means accepted, not connected.
 * Path/query, explicit HTTP authority, and unprotected headers may change. Empty
 * header values differ from removal. Dial routing and TLS authority remain fixed. */
RUMQTTC_API rumqttc_status_t rumqttc_websocket_response_new(rumqttc_websocket_response_t **out, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_websocket_response_destroy(rumqttc_websocket_response_t *response);
/* Copies ASCII host[:port] or [IPv6][:port], with a numeric port in 0..65535
 * and no user information. Updates URI authority and Host together. Scheme,
 * dial/proxy destination, TLS SNI, and certificate verification remain tied to
 * the configured endpoint. A successful
 * call replaces any prior override; an invalid or oversized value leaves it intact. */
RUMQTTC_API rumqttc_status_t rumqttc_websocket_response_set_authority(rumqttc_websocket_response_t *response, rumqttc_string_view_t authority, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_websocket_response_set_path_and_query(rumqttc_websocket_response_t *response, rumqttc_string_view_t path, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_websocket_response_header_edit(rumqttc_websocket_response_t *response, uint32_t operation, rumqttc_string_view_t name, rumqttc_bytes_view_t value, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_callback_websocket_complete(rumqttc_callback_completion_t *completion, const rumqttc_websocket_response_t *response);
RUMQTTC_API rumqttc_status_t rumqttc_callback_websocket_reject(rumqttc_callback_completion_t *completion);
RUMQTTC_API rumqttc_status_t rumqttc_error_websocket_failure(const rumqttc_error_t *error, uint8_t *present_out, uint32_t *failure_out);

/* Copies ordered edits; append retains duplicates, replace and remove act in order. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_websocket_header_edits(rumqttc_config_t *config, const rumqttc_websocket_header_edit_t *edits, size_t count, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_tcp_send_buffer_size_bytes(rumqttc_config_t *config, uint32_t bytes, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_tcp_receive_buffer_size_bytes(rumqttc_config_t *config, uint32_t bytes, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_tcp_buffer_sizes(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_tcp_nodelay(rumqttc_config_t *config, uint8_t enabled, rumqttc_error_t **error_out);
/* Numeric IPv4/IPv6 socket address with port, for example 127.0.0.1:0. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_local_bind_address(rumqttc_config_t *config, rumqttc_string_view_t address, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_local_bind_address(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_bind_device(rumqttc_config_t *config, rumqttc_string_view_t device, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_bind_device(rumqttc_config_t *config, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_mptcp(rumqttc_config_t *config, uint8_t enabled, rumqttc_error_t **error_out);

RUMQTTC_API rumqttc_status_t rumqttc_client_start(const rumqttc_config_t *config, rumqttc_client_t **out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_close_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_close_now_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, rumqttc_error_t **error_out);
/* The first admitted options win; incompatible later close options fail. */
RUMQTTC_API rumqttc_status_t rumqttc_client_close_with_options_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, const rumqttc_disconnect_options_t *options, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_close_now_with_options_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, const rumqttc_disconnect_options_t *options, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_destroy_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_client_abandon(rumqttc_client_t *client);

/* Ordered admission is nonblocking even for tracked forms. BACKPRESSURE leaves
 * no fence. A timeout starts at successful admission; completion wait timeouts
 * do not cancel work. Dropping a completion does not cancel its fence.
 * QoS 0 means local flush; QoS 1/2 mean successful terminal acknowledgement.
 * Subscriptions and independent inbound ACKs are outside this publish-only scope.
 * All symbols remain available when support is disabled (CONFIG_ERROR).
 * Owner destruction/abandonment explicitly aborts ordered shutdown. */
RUMQTTC_API rumqttc_status_t rumqttc_client_try_disconnect_after_queued(rumqttc_client_t *client, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_try_disconnect_after_queued_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_try_disconnect_after_queued_with_options(rumqttc_client_t *client, const rumqttc_disconnect_options_t *options, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_try_disconnect_after_queued_with_options_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, const rumqttc_disconnect_options_t *options, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_disconnect_after_queued_tracked(rumqttc_client_t *client, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_disconnect_after_queued_timeout_ms_tracked(rumqttc_client_t *client, uint64_t timeout_ms, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_disconnect_after_queued_with_options_tracked(rumqttc_client_t *client, const rumqttc_disconnect_options_t *options, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_disconnect_after_queued_with_options_timeout_ms_tracked(rumqttc_client_t *client, uint64_t timeout_ms, const rumqttc_disconnect_options_t *options, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
/* Ordered closers coalesce matching payloads, preserve the first native deadline,
 * and use independent caller budgets for observation and joining. A timeout
 * retains the client/join owner; cleanup may continue after operation timeout. */
RUMQTTC_API rumqttc_status_t rumqttc_client_close_after_queued_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_close_after_queued_with_options_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, const rumqttc_disconnect_options_t *options, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_ordered_disconnect_failure(const rumqttc_error_t *error, uint8_t *present_out, uint32_t *failure_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_ordered_shutdown_diagnostics(const rumqttc_completion_t *completion, rumqttc_ordered_shutdown_diagnostics_t *out, rumqttc_error_t **error_out);

RUMQTTC_API rumqttc_status_t rumqttc_client_try_publish(rumqttc_client_t *client, rumqttc_string_view_t topic, rumqttc_bytes_view_t payload, const rumqttc_publish_options_t *options, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_publish_tracked(rumqttc_client_t *client, rumqttc_string_view_t topic, rumqttc_bytes_view_t payload, const rumqttc_publish_options_t *options, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_try_subscribe(rumqttc_client_t *client, const rumqttc_subscription_t *subscriptions, size_t count, const rumqttc_subscribe_options_t *options, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_subscribe_tracked(rumqttc_client_t *client, const rumqttc_subscription_t *subscriptions, size_t count, const rumqttc_subscribe_options_t *options, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_try_unsubscribe(rumqttc_client_t *client, const rumqttc_string_view_t *filters, size_t count, const rumqttc_unsubscribe_options_t *options, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_unsubscribe_tracked(rumqttc_client_t *client, const rumqttc_string_view_t *filters, size_t count, const rumqttc_unsubscribe_options_t *options, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_try_acknowledge(rumqttc_client_t *client, rumqttc_event_t *event, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_acknowledge_tracked(rumqttc_client_t *client, rumqttc_event_t *event, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
/* Options and their strings/properties are copied during the call. NULL selects default success.
 * Admission is nonblocking. Rejection/backpressure leaves the event token available for retry.
 * Explicit V5 options require MQTT 5, even when every value is default. Completion means local
 * ACK flush, including a negative ACK; it does not prove broker receipt or request redelivery.
 * The event remains readable after acknowledgement. */
RUMQTTC_API rumqttc_status_t rumqttc_client_try_acknowledge_with_options(rumqttc_client_t *client, rumqttc_event_t *event, const rumqttc_acknowledgement_options_t *options, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_acknowledge_with_options_tracked(rumqttc_client_t *client, rumqttc_event_t *event, const rumqttc_acknowledgement_options_t *options, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_diagnostics_tracked(rumqttc_client_t *client, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_try_reauthenticate(rumqttc_client_t *client, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_reauthenticate_tracked(rumqttc_client_t *client, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);

RUMQTTC_API rumqttc_status_t rumqttc_completion_poll(const rumqttc_completion_t *completion, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_wait_timeout_ms(const rumqttc_completion_t *completion, uint64_t timeout_ms, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_operation_id(const rumqttc_completion_t *completion, uint64_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_kind(const rumqttc_completion_t *completion, rumqttc_completion_kind_t *out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_result_count(const rumqttc_completion_t *completion, size_t *out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_result_at(const rumqttc_completion_t *completion, size_t index, uint8_t *success_out, rumqttc_qos_t *qos_out, uint8_t *reason_present_out, uint8_t *reason_out, rumqttc_error_t **error_out);
/* Optional scalar outputs; at least one must be non-NULL. Values are zeroed on error.
 * present_out=0 means no current accepted connection. The ordinary diagnostics
 * record and Connected event retain their existing layout and raw semantics. */
RUMQTTC_API rumqttc_status_t rumqttc_completion_connack_session_diagnostics(const rumqttc_completion_t *completion, uint8_t *present_out, uint8_t *raw_session_present_out, uint8_t *session_resumed_out, uint32_t *diagnostic_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_diagnostics(const rumqttc_completion_t *completion, rumqttc_diagnostics_t *out, rumqttc_error_t **error_out);
/* Terminal ACK contents, including broker rejection. Pending returns WOULD_BLOCK.
 * The metadata query succeeds with present=0 for a terminal outcome without an ACK;
 * ACK-specific accessors then return INVALID_STATE. Broker rejection itself does
 * not make these accessors fail. Metadata uses the native decoded property presence,
 * not byte-for-byte wire encoding. Ordinary poll/wait retain their existing behavior.
 * Views are immutable and owned by this completion handle. They survive client and
 * returned-error destruction and repeated observations, but expire when this
 * completion handle is destroyed. Copy with rumqttc_string_copy BEFORE destruction;
 * caller-owned copies remain usable afterward. Concurrent destruction is forbidden.
 * All supplied outputs are initialized on failure; struct_size is preserved and
 * only the declared known record extent is cleared. At least one scalar/view output
 * is required. No contents are added to automatic logs or formatted errors.
 * V4 has no scalar reason/properties; SUBACK codes are retained, UNSUBACK has no
 * per-filter results. result_count reports their availability separately from count.
 * result_count/at reject non-filter packets; invalid indexes return INVALID_ARGUMENT.
 * QoS 2 exposes rejected PUBREC or terminal PUBCOMP; successful PUBREC and PUBREL
 * remain internal. recovered=1 preserves the native recovered terminal distinction. */
RUMQTTC_API rumqttc_status_t rumqttc_completion_acknowledgement(const rumqttc_completion_t *completion, rumqttc_acknowledgement_details_t *out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_acknowledgement_result_count(const rumqttc_completion_t *completion, uint8_t *present_out, size_t *count_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_acknowledgement_result_at(const rumqttc_completion_t *completion, size_t index, uint8_t *reason_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_acknowledgement_reason_string(const rumqttc_completion_t *completion, uint8_t *present_out, rumqttc_string_view_t *value_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_acknowledgement_user_property_count(const rumqttc_completion_t *completion, size_t *count_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_acknowledgement_user_property_at(const rumqttc_completion_t *completion, size_t index, rumqttc_string_view_t *name_out, rumqttc_string_view_t *value_out, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_completion_destroy(rumqttc_completion_t *completion);

RUMQTTC_API rumqttc_status_t rumqttc_client_event_try_recv(rumqttc_client_t *client, rumqttc_event_t **event_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_event_recv_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, rumqttc_event_t **event_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_kind(const rumqttc_event_t *event, rumqttc_event_kind_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_event_connected(const rumqttc_event_t *event, rumqttc_protocol_t *protocol_out, uint8_t *session_present_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_connack_reason(const rumqttc_event_t *event, uint8_t *reason_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_connack_v5_scalar(const rumqttc_event_t *event, uint32_t property, uint8_t *present_out, uint64_t *value_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_connack_v5_string(const rumqttc_event_t *event, uint32_t property, uint8_t *present_out, rumqttc_string_view_t *value_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_connack_v5_authentication_data(const rumqttc_event_t *event, uint8_t *present_out, rumqttc_bytes_view_t *value_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_user_property_count(const rumqttc_event_t *event, uint32_t property_class, size_t *count_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_user_property_at(const rumqttc_event_t *event, uint32_t property_class, size_t index, rumqttc_string_view_t *name_out, rumqttc_string_view_t *value_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_outgoing_packet_id(const rumqttc_event_t *event, uint8_t *present_out, uint16_t *packet_id_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_authentication(const rumqttc_event_t *event, uint32_t *exchange_out, uint32_t *stage_out, uint8_t *failure_present_out, uint32_t *failure_out, rumqttc_string_view_t *method_out);
/* Broker AUTH packet details are event-owned. Presence flags distinguish an
 * absent property bag from present empty strings or binary data. Copy borrowed
 * views with rumqttc_string_copy/rumqttc_bytes_copy before destroying event. */
RUMQTTC_API rumqttc_status_t rumqttc_event_authentication_details(const rumqttc_event_t *event, uint8_t *reason_present_out, uint8_t *reason_out, uint8_t *properties_present_out, uint8_t *method_present_out, rumqttc_string_view_t *method_out, uint8_t *data_present_out, rumqttc_bytes_view_t *data_out, uint8_t *reason_string_present_out, rumqttc_string_view_t *reason_string_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_broker_disconnect(const rumqttc_event_t *event, uint8_t *reason_out, uint8_t *expiry_present_out, uint32_t *expiry_seconds_out, uint8_t *reason_string_present_out, rumqttc_string_view_t *reason_string_out, uint8_t *server_reference_present_out, rumqttc_string_view_t *server_reference_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_redirect(const rumqttc_event_t *event, uint32_t *source_out, uint32_t *reason_out, uint8_t *failure_present_out, uint32_t *failure_out, uint8_t *reference_present_out, rumqttc_string_view_t *reference_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_redirect_target(const rumqttc_event_t *event, uint8_t *present_out, uint32_t *kind_out, rumqttc_string_view_t *value_out, uint16_t *port_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_redirect_diagnostics(const rumqttc_event_t *event, uint32_t *decision_out, uint64_t *attempts_out, uint8_t *limit_present_out, uint64_t *limit_out, uint64_t *visited_out, uint8_t *loop_out, uint8_t *candidate_present_out, uint64_t *candidate_index_out, uint64_t *candidate_count_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_disconnected(const rumqttc_event_t *event, uint32_t *phase_out, rumqttc_error_t **event_error_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_publish(const rumqttc_event_t *event, rumqttc_string_view_t *topic_out, rumqttc_bytes_view_t *payload_out, rumqttc_qos_t *qos_out, uint8_t *retain_out, uint8_t *duplicate_out, uint8_t *ack_available_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_v5_response_topic(const rumqttc_event_t *event, uint8_t *present_out, rumqttc_string_view_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_event_v5_correlation_data(const rumqttc_event_t *event, uint8_t *present_out, rumqttc_bytes_view_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_event_v5_content_type(const rumqttc_event_t *event, uint8_t *present_out, rumqttc_string_view_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_event_v5_scalar(const rumqttc_event_t *event, uint32_t property, uint8_t *present_out, uint64_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_event_v5_subscription_identifier_count(const rumqttc_event_t *event, size_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_event_v5_subscription_identifier_at(const rumqttc_event_t *event, size_t index, uint64_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_event_v5_user_property_count(const rumqttc_event_t *event, size_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_event_v5_user_property_at(const rumqttc_event_t *event, size_t index, rumqttc_string_view_t *name_out, rumqttc_string_view_t *value_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_outgoing_kind(const rumqttc_event_t *event, uint32_t *out);
RUMQTTC_API void rumqttc_event_destroy(rumqttc_event_t *event);

RUMQTTC_API rumqttc_status_t rumqttc_error_status(const rumqttc_error_t *error, rumqttc_status_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_error_kind(const rumqttc_error_t *error, rumqttc_error_kind_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_error_code(const rumqttc_error_t *error, rumqttc_string_view_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_error_message(const rumqttc_error_t *error, rumqttc_string_view_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_error_source_chain(const rumqttc_error_t *error, rumqttc_string_view_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_error_flags(const rumqttc_error_t *error, uint8_t *retryable_out, uint8_t *ambiguous_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_broker_reason(const rumqttc_error_t *error, uint8_t *present_out, uint8_t *reason_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_publish_failure(const rumqttc_error_t *error, uint8_t *present_out, uint32_t *failure_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_store_failure(const rumqttc_error_t *error, uint8_t *present_out, uint32_t *failure_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_auth_failure(const rumqttc_error_t *error, uint8_t *present_out, uint32_t *failure_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_redirect_failure(const rumqttc_error_t *error, uint8_t *present_out, uint32_t *failure_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_context(const rumqttc_error_t *error, rumqttc_protocol_t *protocol_out, uint32_t *phase_out, uint8_t *generation_present_out, uint64_t *generation_out, uint32_t *delivery_status_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_operation_id(const rumqttc_error_t *error, uint8_t *present_out, uint64_t *operation_id_out);
RUMQTTC_API void rumqttc_error_destroy(rumqttc_error_t *error);

RUMQTTC_API rumqttc_status_t rumqttc_bytes_copy(rumqttc_bytes_view_t view, uint8_t *buffer, size_t capacity, size_t *required_out);
RUMQTTC_API rumqttc_status_t rumqttc_string_copy(rumqttc_string_view_t view, char *buffer, size_t capacity, size_t *required_out);

/* Application-controlled MQTT 5 redirects. Registrations own user_data only after
 * successful construction and destroy it once after the last config/client owner.
 * decide runs synchronously on the driver and returns RUMQTTC_OK or a failure status.
 * request and response are borrowed during decide. Retain request to inspect it later;
 * response cannot be retained, destroyed, or completed later. Select using this request
 * (or a retained handle to this same request). Credential/profile setters copy/retain
 * inputs immediately; TLS/WSS requires a TLS profile. Response defaults to rejection.
 * A failed response setter makes the entire decision invalid, even if its error is ignored.
 * Callbacks/destructors must be thread-safe, prompt, and never unwind or wait for MQTT
 * completion. Do not join/destroy the active client from decide. Timeouts reject late
 * results but cannot preempt blocked foreign code or guarantee shutdown latency.
 * Successful policy setters replace each other. Failure leaves the old policy intact.
 * Views returned by request accessors remain valid while that request handle is alive;
 * caller-owned output records must use the initializer macros. Outputs remain unchanged
 * on accessor failure. Empty optional reference views have length zero; port zero means
 * no explicitly advertised port. SRV resolution and candidate choice remain native.
 * Only explicit session reuse can retain recovery state. Same ID/scope may preserve live
 * state; changed keys load their own checkpoint, never migrate the origin checkpoint.
 * Authority reuse retains the existing enhanced-auth mechanism and CONNECT method/data;
 * supplied username/password are independent. Network reuse retains proxy configuration
 * and WebSocket modifiers together. Target TLS credentials always come from its profile.
 * Limits: 256 references / 64 KiB copied request reference+identity data, 256 KiB copied
 * response data, and 65535 bytes per client ID, scope, username or password. */
typedef struct rumqttc_redirect_registration_t rumqttc_redirect_registration_t;
typedef struct rumqttc_redirect_request_t rumqttc_redirect_request_t;
typedef struct rumqttc_redirect_response_t rumqttc_redirect_response_t;
typedef struct rumqttc_redirect_vtable_t {
  uint32_t struct_size;
  uint32_t (*decide)(void*,
                     const struct rumqttc_redirect_request_t*,
                     struct rumqttc_redirect_response_t*);
  void (*destroy)(void*);
  uint64_t reserved[2];
} rumqttc_redirect_vtable_t;
typedef struct rumqttc_redirect_request_info_t {
  uint32_t struct_size;
  uint32_t source;
  uint32_t reason;
  uint64_t attempt;
  uint64_t remaining_ns;
  struct rumqttc_string_view_t client_id;
  struct rumqttc_string_view_t store_scope;
  size_t reference_count;
  uint64_t reserved[2];
} rumqttc_redirect_request_info_t;
typedef struct rumqttc_redirect_reference_t {
  uint32_t struct_size;
  uint32_t kind;
  uint32_t scheme;
  uint32_t port;
  struct rumqttc_string_view_t raw;
  struct rumqttc_string_view_t host;
  struct rumqttc_string_view_t websocket_resource;
  struct rumqttc_string_view_t srv_owner;
  uint64_t reserved[2];
} rumqttc_redirect_reference_t;

#define RUMQTTC_REDIRECT_VTABLE_INIT { sizeof(rumqttc_redirect_vtable_t), NULL, NULL, {0, 0} }
#define RUMQTTC_REDIRECT_REQUEST_INFO_INIT { sizeof(rumqttc_redirect_request_info_t), 0, 0, 0, 0, {NULL, 0}, {NULL, 0}, 0, {0, 0} }
#define RUMQTTC_REDIRECT_REFERENCE_INIT { sizeof(rumqttc_redirect_reference_t), 0, 0, 0, {NULL, 0}, {NULL, 0}, {NULL, 0}, {NULL, 0}, {0, 0} }
#define RUMQTTC_REDIRECT_REFERENCE_AUTHORITY 1u
#define RUMQTTC_REDIRECT_REFERENCE_URI 2u
#define RUMQTTC_REDIRECT_REFERENCE_SRV 3u
#define RUMQTTC_REDIRECT_SCHEME_NONE 0u
#define RUMQTTC_REDIRECT_SCHEME_MQTT 1u
#define RUMQTTC_REDIRECT_SCHEME_MQTTS 2u
#define RUMQTTC_REDIRECT_SCHEME_WS 3u
#define RUMQTTC_REDIRECT_SCHEME_WSS 4u
#define RUMQTTC_REDIRECT_CLIENT_ID_FRESH 0u
#define RUMQTTC_REDIRECT_CLIENT_ID_REUSE 1u
#define RUMQTTC_REDIRECT_CLIENT_ID_REPLACE 2u
#define RUMQTTC_REDIRECT_SESSION_ISOLATED 0u
#define RUMQTTC_REDIRECT_SESSION_REUSE 1u
#define RUMQTTC_REDIRECT_FAILURE_POLICY_CALLBACK 11u
#define RUMQTTC_REDIRECT_FAILURE_POLICY_PANIC 12u
#define RUMQTTC_REDIRECT_FAILURE_POLICY_TIMEOUT 13u
#define RUMQTTC_REDIRECT_FAILURE_INVALID_RESPONSE 14u
#define RUMQTTC_REDIRECT_FAILURE_RESOURCE_LIMIT 15u
#define RUMQTTC_REDIRECT_FAILURE_STORE_IN_USE 16u
#define RUMQTTC_STORE_FAILURE_KEY_MISMATCH 11u

RUMQTTC_API uint32_t rumqttc_redirect_registration_new(const struct rumqttc_redirect_vtable_t *vtable,
                                           void *data,
                                           uint32_t max_attempts,
                                           uint64_t decision_timeout_ms,
                                           struct rumqttc_redirect_registration_t **out,
                                           struct rumqttc_error_t **error_out);

RUMQTTC_API void rumqttc_redirect_registration_destroy(struct rumqttc_redirect_registration_t *registration);

RUMQTTC_API uint32_t rumqttc_config_set_v5_redirect_authority(struct rumqttc_config_t *config,
                                                  const struct rumqttc_redirect_registration_t *registration,
                                                  struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_redirect_request_retain(const struct rumqttc_redirect_request_t *request,
                                         struct rumqttc_redirect_request_t **out,
                                         struct rumqttc_error_t **error_out);

RUMQTTC_API void rumqttc_redirect_request_destroy(struct rumqttc_redirect_request_t *request);

RUMQTTC_API uint32_t rumqttc_redirect_request_info(const struct rumqttc_redirect_request_t *request,
                                       struct rumqttc_redirect_request_info_t *out,
                                       struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_redirect_request_reference(const struct rumqttc_redirect_request_t *request,
                                            size_t index,
                                            struct rumqttc_redirect_reference_t *out,
                                            struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_redirect_response_follow(struct rumqttc_redirect_response_t *response,
                                          const struct rumqttc_redirect_request_t *request,
                                          size_t index,
                                          uint32_t transport,
                                          const struct rumqttc_tls_profile_t *tls,
                                          struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_redirect_response_reject(struct rumqttc_redirect_response_t *response,
                                          struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_redirect_response_set_client_id(struct rumqttc_redirect_response_t *response,
                                                 uint32_t policy,
                                                 struct rumqttc_string_view_t id,
                                                 struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_redirect_response_set_credentials(struct rumqttc_redirect_response_t *response,
                                                   uint8_t username_present,
                                                   struct rumqttc_string_view_t username,
                                                   uint8_t password_present,
                                                   struct rumqttc_bytes_view_t password,
                                                   struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_redirect_response_set_reuse(struct rumqttc_redirect_response_t *response,
                                             uint8_t authentication_authority,
                                             uint8_t network_credentials,
                                             struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_redirect_response_set_session(struct rumqttc_redirect_response_t *response,
                                               uint32_t policy,
                                               struct rumqttc_string_view_t scope,
                                               struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_event_redirect_selected_reference(const struct rumqttc_event_t *event,
                                                   uint8_t *present_out,
                                                   struct rumqttc_string_view_t *out,
                                                   struct rumqttc_error_t **error_out);

/* Additive reconnect policy and owned observations. Last-error accessors allocate
 * independent rumqttc_error_t handles (NULL when absent); destroy them normally.
 * out and error_out must not alias. Exhaustion has code RECONNECT_EXHAUSTED,
 * is nonretryable, and never implies non-delivery or safe resubmission. */
RUMQTTC_API rumqttc_status_t rumqttc_config_set_reconnect_policy(rumqttc_config_t *config,
                                                               const rumqttc_reconnect_options_t *options,
                                                               rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_clear_reconnect_policy(rumqttc_config_t *config,
                                                                 rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_reconnect_diagnostics(rumqttc_client_t *client,
                                                               rumqttc_reconnect_diagnostics_t *out,
                                                               rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_reconnect_diagnostics(const rumqttc_completion_t *completion,
                                                                   rumqttc_reconnect_diagnostics_t *out,
                                                                   rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_reconnect_last_error(rumqttc_client_t *client,
                                                              rumqttc_error_t **out,
                                                              rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_reconnect_last_error(const rumqttc_completion_t *completion,
                                                                  rumqttc_error_t **out,
                                                                  rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_reconnect_exhaustion(const rumqttc_error_t *error,
                                                             uint8_t *present_out,
                                                             uint64_t *cycles_started_out,
                                                             uint64_t *retries_since_reset_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_reconnect_last_error(const rumqttc_error_t *error,
                                                             rumqttc_error_t **out,
                                                             rumqttc_error_t **error_out);

/* Runtime configuration updates: additive records and independently owned handles. */
RUMQTTC_API uint32_t rumqttc_error_configuration_revision(const struct rumqttc_error_t *error,
                                                        uint8_t *present_out, uint64_t *revision_out);
#define RUMQTTC_COMPLETION_CONFIGURATION_STAGED 12u
#define RUMQTTC_CAP_RUNTIME_CONFIGURATION (UINT64_C(1) << 17)
#define RUMQTTC_CONFIG_FIELD_MAX_REQUEST_BATCH 1u
#define RUMQTTC_CONFIG_FIELD_READ_BATCH_SIZE 2u
#define RUMQTTC_CONFIG_FIELD_PENDING_THROTTLE 3u
#define RUMQTTC_CONFIG_FIELD_CREDENTIALS 4u
#define RUMQTTC_CONFIG_FIELD_BROKER_TLS 5u
#define RUMQTTC_CONFIG_FIELD_NETWORK 6u
#define RUMQTTC_CONFIG_FIELD_CONNECTION_TIMEOUT 7u
#define RUMQTTC_CONFIG_UNCHANGED 0u
#define RUMQTTC_CONFIG_CLEAR 2u
#define RUMQTTC_CONFIG_DESIRED 0u
#define RUMQTTC_CONFIG_EFFECTIVE 1u
#define RUMQTTC_CONFIG_ACTIVATION_UNCHANGED 0u
#define RUMQTTC_CONFIG_ACTIVATION_STAGED 1u
#define RUMQTTC_CONFIG_ACTIVATION_ACTIVATED 2u
#define RUMQTTC_CONFIG_ACTIVATION_SUPERSEDED 3u
#define RUMQTTC_CONFIG_ACTIVATION_CLOSED 4u
#define RUMQTTC_CONFIG_ACTIVATION_UNAVAILABLE_AFTER_REDIRECT 5u
#define RUMQTTC_CONFIG_ROUTE_ORIGIN 0u
#define RUMQTTC_CONFIG_ROUTE_REDIRECT_TRANSITION 1u
#define RUMQTTC_CONFIG_ROUTE_TEMPORARY_TARGET 2u
#define RUMQTTC_CONFIG_ROUTE_PERMANENT_TARGET 3u
#define RUMQTTC_CONFIG_ATTEMPT_NONE 0u
#define RUMQTTC_CONFIG_ATTEMPT_PENDING 1u
#define RUMQTTC_CONFIG_ATTEMPT_SUCCEEDED 2u
#define RUMQTTC_CONFIG_ATTEMPT_FAILED 3u
#define RUMQTTC_CONFIG_ATTEMPT_CANCELLED 4u
#define RUMQTTC_CONFIG_STATUS_CLOSED 1u
#define RUMQTTC_CONFIG_STATUS_ATTEMPT_REVISION_PRESENT 2u
#define RUMQTTC_CONFIG_STATUS_SUCCESSFUL_REVISION_PRESENT 4u
#define RUMQTTC_CONFIG_PROFILE_USERNAME_PRESENT 1u
#define RUMQTTC_CONFIG_PROFILE_PASSWORD_PRESENT 2u
#define RUMQTTC_CONFIG_PROFILE_TLS_PRESENT 4u
#define RUMQTTC_CONFIG_PROFILE_TLS_IDENTITY_PRESENT 8u
#define RUMQTTC_RUNTIME_NETWORK_SEND_BUFFER 1u
#define RUMQTTC_RUNTIME_NETWORK_RECEIVE_BUFFER 2u
#define RUMQTTC_RUNTIME_NETWORK_LOCAL_ADDRESS 4u
#define RUMQTTC_RUNTIME_NETWORK_BIND_DEVICE 8u
#define RUMQTTC_MAX_CONFIGURATION_UPDATE_BYTES (1024u * 1024u)
#define RUMQTTC_MAX_PENDING_CONFIGURATION_BYTES (4u * 1024u * 1024u)
#define RUMQTTC_MAX_PENDING_CONFIGURATION_UPDATES 16u

typedef struct rumqttc_runtime_update_t rumqttc_runtime_update_t;

typedef struct rumqttc_configuration_receipt_t rumqttc_configuration_receipt_t;

typedef struct rumqttc_configuration_snapshot_t rumqttc_configuration_snapshot_t;

typedef struct rumqttc_runtime_network_options_t {
  uint32_t struct_size;
  uint32_t present_fields;
  uint32_t send_buffer_size;
  uint32_t receive_buffer_size;
  uint8_t tcp_nodelay;
  uint8_t mptcp;
  uint8_t reserved[6];
  struct rumqttc_string_view_t local_address;
  struct rumqttc_string_view_t bind_device;
  uint64_t reserved_tail[2];
} rumqttc_runtime_network_options_t;

typedef struct rumqttc_configuration_receipt_status_t {
  uint32_t struct_size;
  uint32_t tuning_state;
  uint32_t connection_state;
  uint32_t reserved;
  uint64_t revision;
} rumqttc_configuration_receipt_status_t;

typedef struct rumqttc_configuration_status_t {
  uint32_t struct_size;
  uint32_t flags;
  uint64_t revision;
  uint64_t desired_tuning_revision;
  uint64_t effective_tuning_revision;
  uint64_t desired_connection_revision;
  uint64_t effective_connection_revision;
  uint64_t attempt;
  uint64_t attempt_revision;
  uint64_t successful_connection_revision;
  uint32_t route;
  uint32_t attempt_route;
  uint32_t attempt_outcome;
  uint32_t reserved;
  uint64_t effective_read_batch_size;
  uint64_t effective_age_ns;
  uint64_t observation_age_ns;
  uint64_t snapshot_age_ns;
} rumqttc_configuration_status_t;

typedef struct rumqttc_runtime_tuning_t {
  uint32_t struct_size;
  uint32_t reserved;
  uint64_t max_request_batch;
  uint64_t read_batch_size;
  uint64_t pending_throttle_ns;
} rumqttc_runtime_tuning_t;

typedef struct rumqttc_connection_profile_summary_t {
  uint32_t struct_size;
  uint32_t flags;
  uint32_t tls_backend;
  uint32_t tls_pin_count;
  uint64_t connection_timeout_ms;
} rumqttc_connection_profile_summary_t;

#define RUMQTTC_RUNTIME_NETWORK_OPTIONS_INIT \
    { sizeof(rumqttc_runtime_network_options_t), 0, 0, 0, 0, 0, {0}, {NULL, 0}, {NULL, 0}, {0} }
#define RUMQTTC_CONFIGURATION_RECEIPT_STATUS_INIT \
    { sizeof(rumqttc_configuration_receipt_status_t), 0, 0, 0, 0 }
#define RUMQTTC_CONFIGURATION_STATUS_INIT \
    { sizeof(rumqttc_configuration_status_t), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 }
#define RUMQTTC_RUNTIME_TUNING_INIT \
    { sizeof(rumqttc_runtime_tuning_t), 0, 0, 0, 0 }
#define RUMQTTC_CONNECTION_PROFILE_SUMMARY_INIT \
    { sizeof(rumqttc_connection_profile_summary_t), 0, 0, 0, 0 }

/* Setters copy inputs; updates are reusable and snapshot independently at admission.
 * field_action selects UNCHANGED or CLEAR; use typed setters for replacement.
 * Network replacement supplies the whole network group, including omitted defaults.
 * Completion proves staging only. Receipts/snapshots survive client destruction.
 * Network accessor views are borrowed until snapshot destruction. No secrets are returned.
 * Effective tuning/read batches are cached at a poll boundary; ages use monotonic time.
 * Admission uses separate finite update capacity; activation may wait indefinitely
 * for an idle native poll. Update and handle destruction must not race one another.
 * Broker TLS clearing/transport changes and redirected-target profile updates fail.
 */

RUMQTTC_API uint32_t rumqttc_runtime_update_new(struct rumqttc_runtime_update_t **out,
                                    struct rumqttc_error_t **error_out);

RUMQTTC_API void rumqttc_runtime_update_destroy(struct rumqttc_runtime_update_t *update);

RUMQTTC_API uint32_t rumqttc_runtime_update_set_max_request_batch(struct rumqttc_runtime_update_t *update,
                                                      uint32_t count,
                                                      struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_runtime_update_set_read_batch_size(struct rumqttc_runtime_update_t *update,
                                                    uint32_t count,
                                                    struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_runtime_update_set_pending_throttle_ns(struct rumqttc_runtime_update_t *update,
                                                        uint64_t nanoseconds,
                                                        struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_runtime_update_set_connection_timeout_ms(struct rumqttc_runtime_update_t *update,
                                                          uint64_t milliseconds,
                                                          struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_runtime_update_set_credentials(struct rumqttc_runtime_update_t *update,
                                                uint8_t username_present,
                                                struct rumqttc_string_view_t username,
                                                uint8_t password_present,
                                                struct rumqttc_bytes_view_t password,
                                                struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_runtime_update_set_broker_tls_profile(struct rumqttc_runtime_update_t *update,
                                                       const struct rumqttc_tls_profile_t *profile,
                                                       struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_runtime_update_set_network(struct rumqttc_runtime_update_t *update,
                                            const struct rumqttc_runtime_network_options_t *options,
                                            struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_runtime_update_field_action(struct rumqttc_runtime_update_t *update,
                                             uint32_t field,
                                             uint32_t action,
                                             struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_client_update_configuration_tracked(struct rumqttc_client_t *client,
                                                     const struct rumqttc_runtime_update_t *update,
                                                     struct rumqttc_completion_t **completion_out,
                                                     struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_completion_configuration_receipt(const struct rumqttc_completion_t *completion,
                                                  struct rumqttc_configuration_receipt_t **out,
                                                  struct rumqttc_error_t **error_out);

RUMQTTC_API void rumqttc_configuration_receipt_destroy(struct rumqttc_configuration_receipt_t *receipt);

RUMQTTC_API uint32_t rumqttc_configuration_receipt_status(const struct rumqttc_configuration_receipt_t *receipt,
                                              struct rumqttc_configuration_receipt_status_t *out,
                                              struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_client_configuration_snapshot(struct rumqttc_client_t *client,
                                               struct rumqttc_configuration_snapshot_t **out,
                                               struct rumqttc_error_t **error_out);

RUMQTTC_API void rumqttc_configuration_snapshot_destroy(struct rumqttc_configuration_snapshot_t *snapshot);

RUMQTTC_API uint32_t rumqttc_configuration_snapshot_status(const struct rumqttc_configuration_snapshot_t *snapshot,
                                               struct rumqttc_configuration_status_t *out,
                                               struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_configuration_snapshot_tuning(const struct rumqttc_configuration_snapshot_t *snapshot,
                                               uint32_t selected,
                                               struct rumqttc_runtime_tuning_t *out,
                                               struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_configuration_snapshot_profile(const struct rumqttc_configuration_snapshot_t *snapshot,
                                                uint32_t selected,
                                                struct rumqttc_connection_profile_summary_t *out,
                                                struct rumqttc_error_t **error_out);

RUMQTTC_API uint32_t rumqttc_configuration_snapshot_network(const struct rumqttc_configuration_snapshot_t *snapshot,
                                                uint32_t selected,
                                                struct rumqttc_runtime_network_options_t *out,
                                                struct rumqttc_error_t **error_out);

#ifdef __cplusplus
}
#endif

#endif
