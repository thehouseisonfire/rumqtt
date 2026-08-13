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

typedef uint32_t rumqttc_protocol_t;
#define RUMQTTC_PROTOCOL_V4 1u
#define RUMQTTC_PROTOCOL_V5 2u

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

/* Disconnect phases returned by rumqttc_event_disconnected. */
#define RUMQTTC_CONNECTION_PHASE_NONE 0u
#define RUMQTTC_CONNECTION_PHASE_ATTEMPT 1u
#define RUMQTTC_CONNECTION_PHASE_ESTABLISHED 2u

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

typedef struct rumqttc_config_t rumqttc_config_t;
typedef struct rumqttc_client_t rumqttc_client_t;
typedef struct rumqttc_event_t rumqttc_event_t;
typedef struct rumqttc_completion_t rumqttc_completion_t;
typedef struct rumqttc_error_t rumqttc_error_t;

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

typedef struct rumqttc_user_property_t {
    uint32_t struct_size;
    rumqttc_string_view_t name;
    rumqttc_string_view_t value;
} rumqttc_user_property_t;

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

RUMQTTC_API uint32_t rumqttc_abi_version(void);
RUMQTTC_API const char *rumqttc_library_version(void);

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
RUMQTTC_API rumqttc_status_t rumqttc_config_set_transport_websocket(rumqttc_config_t *config, rumqttc_string_view_t url, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_config_set_transport_wss(rumqttc_config_t *config, rumqttc_string_view_t url, rumqttc_bytes_view_t ca, rumqttc_bytes_view_t certificate, rumqttc_bytes_view_t private_key, rumqttc_error_t **error_out);
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

RUMQTTC_API rumqttc_status_t rumqttc_client_start(const rumqttc_config_t *config, rumqttc_client_t **out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_close_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_close_now_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_destroy_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_client_abandon(rumqttc_client_t *client);

RUMQTTC_API rumqttc_status_t rumqttc_client_try_publish(rumqttc_client_t *client, rumqttc_string_view_t topic, rumqttc_bytes_view_t payload, const rumqttc_publish_options_t *options, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_publish_tracked(rumqttc_client_t *client, rumqttc_string_view_t topic, rumqttc_bytes_view_t payload, const rumqttc_publish_options_t *options, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_try_subscribe(rumqttc_client_t *client, const rumqttc_subscription_t *subscriptions, size_t count, const rumqttc_subscribe_options_t *options, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_subscribe_tracked(rumqttc_client_t *client, const rumqttc_subscription_t *subscriptions, size_t count, const rumqttc_subscribe_options_t *options, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_try_unsubscribe(rumqttc_client_t *client, const rumqttc_string_view_t *filters, size_t count, const rumqttc_unsubscribe_options_t *options, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_unsubscribe_tracked(rumqttc_client_t *client, const rumqttc_string_view_t *filters, size_t count, const rumqttc_unsubscribe_options_t *options, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_try_acknowledge(rumqttc_client_t *client, rumqttc_event_t *event, uint64_t *operation_id_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_acknowledge_tracked(rumqttc_client_t *client, rumqttc_event_t *event, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_diagnostics_tracked(rumqttc_client_t *client, rumqttc_completion_t **completion_out, rumqttc_error_t **error_out);

RUMQTTC_API rumqttc_status_t rumqttc_completion_poll(const rumqttc_completion_t *completion, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_wait_timeout_ms(const rumqttc_completion_t *completion, uint64_t timeout_ms, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_operation_id(const rumqttc_completion_t *completion, uint64_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_kind(const rumqttc_completion_t *completion, rumqttc_completion_kind_t *out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_result_count(const rumqttc_completion_t *completion, size_t *out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_result_at(const rumqttc_completion_t *completion, size_t index, uint8_t *success_out, rumqttc_qos_t *qos_out, uint8_t *reason_present_out, uint8_t *reason_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_completion_diagnostics(const rumqttc_completion_t *completion, rumqttc_diagnostics_t *out, rumqttc_error_t **error_out);
RUMQTTC_API void rumqttc_completion_destroy(rumqttc_completion_t *completion);

RUMQTTC_API rumqttc_status_t rumqttc_client_event_try_recv(rumqttc_client_t *client, rumqttc_event_t **event_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_client_event_recv_timeout_ms(rumqttc_client_t *client, uint64_t timeout_ms, rumqttc_event_t **event_out, rumqttc_error_t **error_out);
RUMQTTC_API rumqttc_status_t rumqttc_event_kind(const rumqttc_event_t *event, rumqttc_event_kind_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_event_connected(const rumqttc_event_t *event, rumqttc_protocol_t *protocol_out, uint8_t *session_present_out);
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
RUMQTTC_API rumqttc_status_t rumqttc_error_message(const rumqttc_error_t *error, rumqttc_string_view_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_error_source_chain(const rumqttc_error_t *error, rumqttc_string_view_t *out);
RUMQTTC_API rumqttc_status_t rumqttc_error_flags(const rumqttc_error_t *error, uint8_t *retryable_out, uint8_t *ambiguous_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_broker_reason(const rumqttc_error_t *error, uint8_t *present_out, uint8_t *reason_out);
RUMQTTC_API rumqttc_status_t rumqttc_error_operation_id(const rumqttc_error_t *error, uint8_t *present_out, uint64_t *operation_id_out);
RUMQTTC_API void rumqttc_error_destroy(rumqttc_error_t *error);

RUMQTTC_API rumqttc_status_t rumqttc_bytes_copy(rumqttc_bytes_view_t view, uint8_t *buffer, size_t capacity, size_t *required_out);
RUMQTTC_API rumqttc_status_t rumqttc_string_copy(rumqttc_string_view_t view, char *buffer, size_t capacity, size_t *required_out);

#ifdef __cplusplus
}
#endif

#endif
