#include "native_common.h"
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct logging_owner {
    atomic_uint entered, release, destroyed, calls, names;
    atomic_uintptr_t copy;
    unsigned stall;
} logging_owner;

static int equal(rumqttc_string_view_t value, const char *text) {
    return value.len == strlen(text) && memcmp(value.data, text, value.len) == 0;
}

static void check_observation_error(rumqttc_status_t status, rumqttc_error_t *error,
                                    rumqttc_status_t expected) {
    uint32_t actual = UINT32_MAX, kind = UINT32_MAX;
    uint8_t retryable = 99, ambiguous = 99;
    rumqttc_string_view_t code = {0};
    REQUIRE(status == expected && error != NULL);
    CHECK(rumqttc_error_status(error, &actual));
    CHECK(rumqttc_error_kind(error, &kind));
    CHECK(rumqttc_error_flags(error, &retryable, &ambiguous));
    CHECK(rumqttc_error_code(error, &code));
    REQUIRE(actual == expected);
    REQUIRE(kind == (expected == RUMQTTC_TIMEOUT ? RUMQTTC_ERROR_TIMEOUT : RUMQTTC_ERROR_NONE));
    REQUIRE(retryable == (expected == RUMQTTC_TIMEOUT) && ambiguous == 0);
    REQUIRE(equal(code, expected == RUMQTTC_TIMEOUT ? "TIMEOUT" : "WOULD_BLOCK"));
    rumqttc_error_destroy(error);
}

static uint32_t log_record(void *data, const rumqttc_log_record_t *record) {
    logging_owner *owner = data;
    rumqttc_log_record_info_t info = RUMQTTC_LOG_RECORD_INFO_INIT;
    rumqttc_log_field_t field = RUMQTTC_LOG_FIELD_INIT;
    rumqttc_log_record_t *copy = NULL;
    uintptr_t expected = 0;
    unsigned bit = 0;
    /* ERROR_OUT_SUCCESS: rumqttc_log_record_info */
    CHECK(rumqttc_log_record_info(record, &info, NULL));
    REQUIRE(info.client_id != 0 && info.name.len != 0 && info.field_count <= 16);
    if (equal(info.name, "mqtt.connection_attempt")) bit = 1;
    if (equal(info.name, "mqtt.connection_established")) bit = 2;
    if (equal(info.name, "mqtt.protocol_violation")) bit = 4;
    if (equal(info.name, "mqtt.wrapper.shutdown_completed")) bit = 8;
    atomic_fetch_or(&owner->names, bit);
    for (size_t i = 0; i < info.field_count; ++i) {
        /* ERROR_OUT_SUCCESS: rumqttc_log_record_field */
        CHECK(rumqttc_log_record_field(record, i, &field, NULL));
        REQUIRE(field.kind >= RUMQTTC_LOG_FIELD_BOOL && field.kind <= RUMQTTC_LOG_FIELD_TEXT);
        if (equal(field.name, "attempt_id")) REQUIRE(field.kind == RUMQTTC_LOG_FIELD_UNSIGNED);
        if (equal(field.name, "reconnect")) REQUIRE(field.kind == RUMQTTC_LOG_FIELD_BOOL);
    }
    /* ERROR_OUT_FAILURE: rumqttc_log_record_field */
    REQUIRE(rumqttc_log_record_field(record, info.field_count, &field, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_log_record_copy */
    CHECK(rumqttc_log_record_copy(record, &copy, NULL));
    if (!atomic_compare_exchange_strong(&owner->copy, &expected, (uintptr_t)copy))
        rumqttc_log_record_release(copy);
    atomic_fetch_add(&owner->calls, 1);
    atomic_store(&owner->entered, 1);
    if (owner->stall) {
        uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
        while (!atomic_load(&owner->release)) {
            REQUIRE(native_monotonic_ms() < deadline);
            native_sleep_ms(1);
        }
    }
    return RUMQTTC_OK;
}
static void destroy(void *data) { logging_owner *owner = data; atomic_fetch_add(&owner->destroyed, 1); }

/* Diagnostics remain independently observable while logging is stalled. */
static void logging_keeps_diagnostics_available(rumqttc_client_t *client) {
    rumqttc_diagnostics_snapshot_t *snapshot = NULL;
    rumqttc_error_t *last = NULL;
    rumqttc_diagnostics_group_info_t group = RUMQTTC_DIAGNOSTICS_GROUP_INFO_INIT;
    /* ERROR_OUT_FAILURE: rumqttc_client_diagnostics_snapshot */
    REQUIRE(rumqttc_client_diagnostics_snapshot(NULL, &snapshot, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_client_diagnostics_snapshot */
    CHECK(rumqttc_client_diagnostics_snapshot(client, &snapshot, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_group_info */
    REQUIRE(rumqttc_diagnostics_snapshot_group_info(NULL, RUMQTTC_DIAGNOSTICS_GROUP_QUEUES, &group, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_group_info */
    CHECK(rumqttc_diagnostics_snapshot_group_info(snapshot, RUMQTTC_DIAGNOSTICS_GROUP_QUEUES, &group, NULL));
    {
        rumqttc_diagnostics_status_t value = RUMQTTC_DIAGNOSTICS_STATUS_INIT;
        /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_status */
        REQUIRE(rumqttc_diagnostics_snapshot_status(NULL, &value, NULL) == RUMQTTC_INVALID_ARGUMENT);
        /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_status */
        CHECK(rumqttc_diagnostics_snapshot_status(snapshot, &value, NULL));
    }
    {
        rumqttc_diagnostics_queues_t value = RUMQTTC_DIAGNOSTICS_QUEUES_INIT;
        /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_queues */
        REQUIRE(rumqttc_diagnostics_snapshot_queues(NULL, &value, NULL) == RUMQTTC_INVALID_ARGUMENT);
        /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_queues */
        CHECK(rumqttc_diagnostics_snapshot_queues(snapshot, &value, NULL));
    }
    {
        rumqttc_diagnostics_outbound_t value = RUMQTTC_DIAGNOSTICS_OUTBOUND_INIT;
        /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_outbound */
        REQUIRE(rumqttc_diagnostics_snapshot_outbound(NULL, &value, NULL) == RUMQTTC_INVALID_ARGUMENT);
        /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_outbound */
        CHECK(rumqttc_diagnostics_snapshot_outbound(snapshot, &value, NULL));
    }
    {
        rumqttc_diagnostics_session_t value = RUMQTTC_DIAGNOSTICS_SESSION_INIT;
        /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_session */
        REQUIRE(rumqttc_diagnostics_snapshot_session(NULL, &value, NULL) == RUMQTTC_INVALID_ARGUMENT);
        /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_session */
        CHECK(rumqttc_diagnostics_snapshot_session(snapshot, &value, NULL));
    }
    {
        rumqttc_diagnostics_batching_t value = RUMQTTC_DIAGNOSTICS_BATCHING_INIT;
        /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_batching */
        REQUIRE(rumqttc_diagnostics_snapshot_batching(NULL, &value, NULL) == RUMQTTC_INVALID_ARGUMENT);
        /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_batching */
        CHECK(rumqttc_diagnostics_snapshot_batching(snapshot, &value, NULL));
    }
    {
        rumqttc_diagnostics_redirect_t value = RUMQTTC_DIAGNOSTICS_REDIRECT_INIT;
        /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_redirect */
        REQUIRE(rumqttc_diagnostics_snapshot_redirect(NULL, &value, NULL) == RUMQTTC_INVALID_ARGUMENT);
        /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_redirect */
        CHECK(rumqttc_diagnostics_snapshot_redirect(snapshot, &value, NULL));
    }
    {
        rumqttc_ordered_shutdown_diagnostics_t value = RUMQTTC_ORDERED_SHUTDOWN_DIAGNOSTICS_INIT;
        /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_ordered_native */
        REQUIRE(rumqttc_diagnostics_snapshot_ordered_native(NULL, &value, NULL) == RUMQTTC_INVALID_ARGUMENT);
        /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_ordered_native */
        CHECK(rumqttc_diagnostics_snapshot_ordered_native(snapshot, &value, NULL));
    }
    {
        rumqttc_ordered_shutdown_diagnostics_t value = RUMQTTC_ORDERED_SHUTDOWN_DIAGNOSTICS_INIT;
        /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_ordered_wrapper */
        REQUIRE(rumqttc_diagnostics_snapshot_ordered_wrapper(NULL, &value, NULL) == RUMQTTC_INVALID_ARGUMENT);
        /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_ordered_wrapper */
        CHECK(rumqttc_diagnostics_snapshot_ordered_wrapper(snapshot, &value, NULL));
    }
    {
        rumqttc_reconnect_diagnostics_t value = RUMQTTC_RECONNECT_DIAGNOSTICS_INIT;
        /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_reconnect */
        REQUIRE(rumqttc_diagnostics_snapshot_reconnect(NULL, &value, NULL) == RUMQTTC_INVALID_ARGUMENT);
        /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_reconnect */
        CHECK(rumqttc_diagnostics_snapshot_reconnect(snapshot, &value, NULL));
    }
    {
        rumqttc_configuration_status_t value = RUMQTTC_CONFIGURATION_STATUS_INIT;
        /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_configuration */
        REQUIRE(rumqttc_diagnostics_snapshot_configuration(NULL, &value, NULL) == RUMQTTC_INVALID_ARGUMENT);
        /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_configuration */
        CHECK(rumqttc_diagnostics_snapshot_configuration(snapshot, &value, NULL));
    }
    /* ERROR_OUT_FAILURE: rumqttc_diagnostics_snapshot_last_failure */
    REQUIRE(rumqttc_diagnostics_snapshot_last_failure(NULL, &last, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_diagnostics_snapshot_last_failure */
    CHECK(rumqttc_diagnostics_snapshot_last_failure(snapshot, &last, NULL));
    rumqttc_error_destroy(last); rumqttc_diagnostics_snapshot_destroy(snapshot);
}

static unsigned run_case(rumqttc_protocol_t protocol, unsigned stall, unsigned attempt) {
    logging_owner owner = {0};
    rumqttc_logging_options_t options = RUMQTTC_LOGGING_OPTIONS_INIT;
    rumqttc_logging_vtable_t vtable = RUMQTTC_LOGGING_VTABLE_INIT;
    rumqttc_logging_filter_t filter = RUMQTTC_LOGGING_FILTER_INIT;
    rumqttc_logging_statistics_t stats = RUMQTTC_LOGGING_STATISTICS_INIT;
    rumqttc_logging_context_t *context = NULL, *retained_context = NULL;
    rumqttc_logging_registration_t *registration = NULL, *retained_registration = NULL;
    rumqttc_logging_flush_t *flush = NULL;
    rumqttc_config_t *config = NULL;
    rumqttc_client_t *client = NULL;
    rumqttc_log_record_t *copy = NULL;
    rumqttc_log_record_info_t info = RUMQTTC_LOG_RECORD_INFO_INIT;
    uint32_t state = 99;
    uint8_t joined = 99;
    unsigned complete;
    char id[80];
    owner.stall = stall;
    filter.level = RUMQTTC_LOG_TRACE;
    options.record_capacity = stall ? 1 : 1024;
    vtable.record = log_record; vtable.destroy = destroy;
    /* ERROR_OUT_FAILURE: rumqttc_logging_context_new */
    REQUIRE(rumqttc_logging_context_new(NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_context_new */
    CHECK(rumqttc_logging_context_new(&options, &context, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_logging_context_retain */
    REQUIRE(rumqttc_logging_context_retain(NULL, &retained_context, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_context_retain */
    CHECK(rumqttc_logging_context_retain(context, &retained_context, NULL));
    rumqttc_logging_context_release(retained_context);
    vtable.reserved = 1;
    /* ERROR_OUT_FAILURE: rumqttc_logging_registration_new */
    REQUIRE(rumqttc_logging_registration_new(context, &filter, &vtable, &owner, &registration, NULL) == RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(registration == NULL && atomic_load(&owner.destroyed) == 0);
    vtable.reserved = 0;
    /* ERROR_OUT_SUCCESS: rumqttc_logging_registration_new */
    CHECK(rumqttc_logging_registration_new(context, &filter, &vtable, &owner, &registration, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_logging_registration_retain */
    REQUIRE(rumqttc_logging_registration_retain(NULL, &retained_registration, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_registration_retain */
    CHECK(rumqttc_logging_registration_retain(registration, &retained_registration, NULL));
    rumqttc_logging_registration_release(retained_registration);
    /* Flush with no accepted records completes immediately. */
    /* ERROR_OUT_FAILURE: rumqttc_logging_context_flush */
    REQUIRE(rumqttc_logging_context_flush(NULL, &flush, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_context_flush */
    CHECK(rumqttc_logging_context_flush(context, &flush, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_logging_flush_poll */
    REQUIRE(rumqttc_logging_flush_poll(NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_flush_poll */
    CHECK(rumqttc_logging_flush_poll(flush, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_logging_flush_wait_timeout_ms */
    REQUIRE(rumqttc_logging_flush_wait_timeout_ms(NULL, 1, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_flush_wait_timeout_ms */
    CHECK(rumqttc_logging_flush_wait_timeout_ms(flush, 1, NULL));
    rumqttc_logging_flush_release(flush);
    CHECK(rumqttc_config_new(protocol, &config, NULL));
    CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), stall ? 65535 : native_test_port(), NULL));
    REQUIRE(snprintf(id, sizeof(id), "native-logging-%s-%u-%u", stall ? "stall" : "wire", (unsigned)protocol, attempt) > 0);
    CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_logging */
    REQUIRE(rumqttc_config_set_logging(NULL, registration, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_logging */
    CHECK(rumqttc_config_set_logging(config, registration, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_logging */
    REQUIRE(rumqttc_config_clear_logging(NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_logging */
    CHECK(rumqttc_config_clear_logging(config, NULL));
    CHECK(rumqttc_config_set_logging(config, registration, NULL));
    CHECK(rumqttc_client_start(config, &client, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_set_logging */
    REQUIRE(rumqttc_client_set_logging(NULL, registration, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_client_set_logging */
    CHECK(rumqttc_client_set_logging(client, registration, NULL));
    if (stall) {
        for (unsigned i = 0; i < 500 && !atomic_load(&owner.entered); ++i) native_sleep_ms(10);
        REQUIRE(atomic_load(&owner.entered));
        for (unsigned i = 0; i < 32; ++i) {
            uint64_t operation_id = 0;
            (void)rumqttc_client_try_publish(client, native_string("private-topic"), native_bytes(NULL, 0), NULL, &operation_id, NULL);
        }
        CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    } else {
        rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
        rumqttc_event_destroy(event);
        event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED); rumqttc_event_destroy(event);
        event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED); rumqttc_event_destroy(event);
        CHECK(rumqttc_client_close_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    }
    logging_keeps_diagnostics_available(client);
    /* ERROR_OUT_FAILURE: rumqttc_logging_context_statistics */
    REQUIRE(rumqttc_logging_context_statistics(NULL, &stats, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_context_statistics */
    CHECK(rumqttc_logging_context_statistics(context, &stats, NULL));
    if (stall) REQUIRE(stats.overload > 0);
    /* ERROR_OUT_FAILURE: rumqttc_logging_registration_statistics */
    REQUIRE(rumqttc_logging_registration_statistics(NULL, &stats, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_registration_statistics */
    CHECK(rumqttc_logging_registration_statistics(registration, &stats, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_logging_context_state */
    REQUIRE(rumqttc_logging_context_state(NULL, &state, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_context_state */
    CHECK(rumqttc_logging_context_state(context, &state, NULL)); REQUIRE(state == RUMQTTC_LOGGING_OPEN);
    CHECK(rumqttc_logging_context_flush(context, &flush, NULL));
    if (stall) {
        rumqttc_error_t *error = NULL;
        rumqttc_status_t status = rumqttc_logging_flush_poll(flush, &error);
        check_observation_error(status, error, RUMQTTC_WOULD_BLOCK);
        status = rumqttc_logging_flush_wait_timeout_ms(flush, 0, &error);
        check_observation_error(status, error, RUMQTTC_WOULD_BLOCK);
        status = rumqttc_logging_flush_wait_timeout_ms(flush, 1, &error);
        check_observation_error(status, error, RUMQTTC_TIMEOUT);
        REQUIRE(rumqttc_logging_flush_poll(flush, NULL) == RUMQTTC_WOULD_BLOCK);
        REQUIRE(rumqttc_logging_flush_wait_timeout_ms(flush, 1, NULL) == RUMQTTC_TIMEOUT);
    }
    atomic_store(&owner.release, 1);
    {
        rumqttc_error_t *error = (rumqttc_error_t *)(uintptr_t)1;
        CHECK(rumqttc_logging_flush_wait_timeout_ms(flush, NATIVE_DEADLINE_MS, &error));
        REQUIRE(error == NULL);
    }
    rumqttc_logging_flush_release(flush);
    /* Flush covers accepted records, not records rejected on producer contention. */
    CHECK(rumqttc_logging_context_statistics(context, &stats, NULL));
    REQUIRE(stats.failures == 0 && stats.rejected == 0 && stats.discarded == 0);
    REQUIRE(stats.delivered == stats.accepted && stats.delivered == atomic_load(&owner.calls));
    complete = stall || atomic_load(&owner.names) == 15;
    if (!complete) {
        unsigned missing = 0;
        for (unsigned bit = 1; bit <= 8; bit <<= 1)
            if (!(atomic_load(&owner.names) & bit)) ++missing;
        REQUIRE(stats.overload >= missing);
    }
    /* ERROR_OUT_FAILURE: rumqttc_client_clear_logging */
    REQUIRE(rumqttc_client_clear_logging(NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_client_clear_logging */
    CHECK(rumqttc_client_clear_logging(client, NULL));
    CHECK(rumqttc_logging_registration_retain(registration, &retained_registration, NULL));
    CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    rumqttc_config_destroy(config); rumqttc_logging_registration_release(registration);
    /* ERROR_OUT_FAILURE: rumqttc_logging_context_request_shutdown */
    REQUIRE(rumqttc_logging_context_request_shutdown(NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_context_request_shutdown */
    CHECK(rumqttc_logging_context_request_shutdown(context, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_logging_context_try_join */
    REQUIRE(rumqttc_logging_context_try_join(NULL, &joined, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_context_try_join */
    {
        rumqttc_error_t *error = (rumqttc_error_t *)(uintptr_t)1;
        CHECK(rumqttc_logging_context_try_join(context, &joined, &error));
        REQUIRE(joined == 0 && error == NULL);
        rumqttc_status_t status = rumqttc_logging_context_join_timeout_ms(context, 0, &error);
        check_observation_error(status, error, RUMQTTC_WOULD_BLOCK);
        status = rumqttc_logging_context_join_timeout_ms(context, 1, &error);
        check_observation_error(status, error, RUMQTTC_TIMEOUT);
        REQUIRE(rumqttc_logging_context_join_timeout_ms(context, 0, NULL) == RUMQTTC_WOULD_BLOCK);
        REQUIRE(rumqttc_logging_context_join_timeout_ms(context, 1, NULL) == RUMQTTC_TIMEOUT);
    }
    rumqttc_logging_registration_release(retained_registration);
    /* ERROR_OUT_FAILURE: rumqttc_logging_context_join_timeout_ms */
    REQUIRE(rumqttc_logging_context_join_timeout_ms(NULL, 1, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_logging_context_join_timeout_ms */
    {
        rumqttc_error_t *error = (rumqttc_error_t *)(uintptr_t)1;
        CHECK(rumqttc_logging_context_join_timeout_ms(context, NATIVE_DEADLINE_MS, &error));
        REQUIRE(error == NULL);
    }
    CHECK(rumqttc_logging_context_state(context, &state, NULL)); REQUIRE(state == RUMQTTC_LOGGING_QUIESCENT);
    REQUIRE(atomic_load(&owner.destroyed) == 1);
    rumqttc_logging_context_release(context);
    copy = (rumqttc_log_record_t *)atomic_load(&owner.copy);
    if (copy == NULL) {
        REQUIRE(!complete && stats.accepted == 0 && stats.overload > 0);
        return 0;
    }
    /* ERROR_OUT_FAILURE: rumqttc_log_record_info */
    REQUIRE(rumqttc_log_record_info(NULL, &info, NULL) == RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(info.name.len == 0);
    info.struct_size = sizeof(info);
    CHECK(rumqttc_log_record_info(copy, &info, NULL)); REQUIRE(info.name.len > 0);
    /* ERROR_OUT_FAILURE: rumqttc_log_record_copy */
    REQUIRE(rumqttc_log_record_copy(NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    rumqttc_log_record_release(copy);
    return complete;
}
int main(void) {
    if (!(rumqttc_library_capabilities() & RUMQTTC_CAP_LOGGING_SINK)) {
        rumqttc_logging_context_t *context = NULL;
        REQUIRE(getenv("RUMQTTC_TEST_REQUIRE_LOGGING") == NULL);
        REQUIRE(rumqttc_logging_context_new(NULL, &context, NULL) == RUMQTTC_CONFIG_ERROR);
        REQUIRE(context == NULL); return 0;
    }
    for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
        unsigned complete = 0;
        for (unsigned attempt = 0; attempt < 16 && !complete; ++attempt)
            complete = run_case(protocol, 0, attempt);
        REQUIRE(complete);
        REQUIRE(run_case(protocol, 1, 0));
    }
    return 0;
}
