#include "native_common.h"

struct observer_wait {
    rumqttc_first_connection_t *observer;
    rumqttc_protocol_t protocol;
};

static int await_connection(void *argument) {
    struct observer_wait *wait = argument;
    uint32_t state = UINT32_MAX;
    rumqttc_protocol_t protocol = 0;
    uint8_t session = UINT8_MAX;
    /* ERROR_OUT_SUCCESS: rumqttc_first_connection_wait_timeout_ms */
    CHECK(rumqttc_first_connection_wait_timeout_ms(wait->observer, NATIVE_DEADLINE_MS, &state, &protocol, &session,
                                                   NULL));
    REQUIRE(state == RUMQTTC_FIRST_CONNECTION_SUCCEEDED && protocol == wait->protocol && session == 0);
    return 0;
}

static void exercise(rumqttc_protocol_t protocol, int shared) {
    rumqttc_config_t *config = NULL;
    rumqttc_client_t *client = NULL;
    rumqttc_execution_context_t *execution = NULL;
    rumqttc_first_connection_t *observer = NULL, *retained = NULL, *late = NULL;
    uint32_t state = UINT32_MAX;
    uint8_t session = UINT8_MAX;
    rumqttc_protocol_t observed_protocol = UINT32_MAX;
    /* ERROR_OUT_FAILURE: rumqttc_client_first_connection */
    REQUIRE(rumqttc_client_first_connection(NULL, &observer, NULL) == RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(observer == NULL);
    /* ERROR_OUT_FAILURE: rumqttc_first_connection_retain */
    REQUIRE(rumqttc_first_connection_retain(NULL, &retained, NULL) == RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(retained == NULL);
    /* ERROR_OUT_FAILURE: rumqttc_first_connection_poll */
    REQUIRE(rumqttc_first_connection_poll(NULL, &state, &observed_protocol, &session, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(state == RUMQTTC_FIRST_CONNECTION_UNOBSERVED && observed_protocol == 0 && session == 0);
    /* ERROR_OUT_FAILURE: rumqttc_first_connection_wait_timeout_ms */
    REQUIRE(rumqttc_first_connection_wait_timeout_ms(NULL, 0, &state, NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    CHECK(rumqttc_config_new(protocol, &config, NULL));
    CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
    CHECK(rumqttc_config_set_client_id(config, native_string("native-first-connection"), NULL));
    if (shared) {
        CHECK(rumqttc_execution_context_new(NULL, &execution, NULL));
        CHECK(rumqttc_config_set_execution_context(config, execution, NULL));
    }
    CHECK(rumqttc_client_start(config, &client, NULL));
    rumqttc_config_destroy(config);
    /* ERROR_OUT_SUCCESS: rumqttc_client_first_connection */
    CHECK(rumqttc_client_first_connection(client, &observer, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_first_connection_retain */
    CHECK(rumqttc_first_connection_retain(observer, &retained, NULL));
    struct observer_wait waits[2] = {{observer, protocol}, {retained, protocol}};
    native_thread_t *threads[2] = {native_thread_start(await_connection, &waits[0]),
                                   native_thread_start(await_connection, &waits[1])};
    REQUIRE(threads[0] != NULL && threads[1] != NULL);
    REQUIRE(native_thread_join(threads[0]) == 0 && native_thread_join(threads[1]) == 0);
    /* Observation left the Connected event available to the application. */
    rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    rumqttc_event_destroy(event);
    /* ERROR_OUT_SUCCESS: rumqttc_first_connection_poll */
    CHECK(rumqttc_first_connection_poll(observer, &state, NULL, NULL, NULL));
    REQUIRE(state == RUMQTTC_FIRST_CONNECTION_SUCCEEDED);
    CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    CHECK(rumqttc_client_first_connection(client, &late, NULL));
    CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    if (execution != NULL) {
        CHECK(rumqttc_execution_context_request_shutdown(execution, NULL));
        CHECK(rumqttc_execution_context_join_timeout_ms(execution, NATIVE_DEADLINE_MS, NULL));
        rumqttc_execution_context_release(execution);
    }
    CHECK(rumqttc_first_connection_poll(retained, &state, &observed_protocol, &session, NULL));
    REQUIRE(state == RUMQTTC_FIRST_CONNECTION_SUCCEEDED && observed_protocol == protocol && session == 0);
    CHECK(rumqttc_first_connection_wait_timeout_ms(late, 0, &state, NULL, NULL, NULL));
    rumqttc_first_connection_release(observer);
    rumqttc_first_connection_release(retained);
    rumqttc_first_connection_release(late);
    rumqttc_first_connection_release(NULL);
}

int main(void) {
    REQUIRE((rumqttc_library_capabilities() & RUMQTTC_CAP_FIRST_CONNECTION) != 0);
    for (unsigned protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol)
        for (int shared = 0; shared != 2; ++shared)
            exercise(protocol, shared);
    return 0;
}
