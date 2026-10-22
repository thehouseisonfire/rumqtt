#include "native_common.h"
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

struct consumer {
    rumqttc_client_t *client;
    atomic_int stopped;
};
static int consume(void *argument) {
    struct consumer *consumer = argument;
    while (!atomic_load(&consumer->stopped)) {
        rumqttc_event_t *event = NULL;
        rumqttc_status_t status = rumqttc_client_event_recv_timeout_ms(consumer->client, 10, &event, NULL);
        REQUIRE(status == RUMQTTC_OK || status == RUMQTTC_TIMEOUT);
        rumqttc_event_destroy(event);
    }
    return 0;
}

static void reclaim(void *userdata, const uint8_t *data, size_t len) {
    atomic_uint *calls = userdata;
    REQUIRE(len == 256 && data[0] == 42 && data[len - 1] == 42);
    REQUIRE((rumqttc_library_capabilities() & RUMQTTC_CAP_OWNED_PUBLISH_BUFFER) != 0);
    free((void *)data);
    atomic_fetch_add(calls, 1);
}

static void exercise(rumqttc_protocol_t protocol, int shared) {
    rumqttc_config_t *config = NULL;
    rumqttc_client_t *client = NULL;
    rumqttc_execution_context_t *execution = NULL;
    rumqttc_publish_buffer_t *buffer = NULL, *retained = NULL;
    rumqttc_completion_t *completion = NULL;
    rumqttc_pending_admission_t *pending = NULL;
    uint64_t operation = UINT64_MAX;
    uint32_t state = UINT32_MAX;
    atomic_uint calls;
    atomic_init(&calls, 0);
    uint8_t *data = malloc(256);
    REQUIRE(data != NULL);
    memset(data, 42, 256);
    /* ERROR_OUT_FAILURE: rumqttc_publish_buffer_create */
    REQUIRE(rumqttc_publish_buffer_create(native_bytes(data, 256), NULL, &calls, &buffer, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(buffer == NULL && atomic_load(&calls) == 0);
    /* ERROR_OUT_SUCCESS: rumqttc_publish_buffer_create */
    CHECK(rumqttc_publish_buffer_create(native_bytes(data, 256), reclaim, &calls, &buffer, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_publish_buffer_retain */
    REQUIRE(rumqttc_publish_buffer_retain(NULL, &retained, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_publish_buffer_retain */
    CHECK(rumqttc_publish_buffer_retain(buffer, &retained, NULL));
    CHECK(rumqttc_config_new(protocol, &config, NULL));
    CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
    CHECK(rumqttc_config_set_client_id(config, native_string("native-owned-payload"), NULL));
    CHECK(rumqttc_config_set_request_capacity(config, 8, NULL));
    if (shared) {
        CHECK(rumqttc_execution_context_new(NULL, &execution, NULL));
        CHECK(rumqttc_config_set_execution_context(config, execution, NULL));
    }
    CHECK(rumqttc_client_start(config, &client, NULL));
    rumqttc_config_destroy(config);
    rumqttc_event_t *connected = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    rumqttc_event_destroy(connected);
    struct consumer consumer;
    consumer.client = client;
    atomic_init(&consumer.stopped, 0);
    native_thread_t *thread = native_thread_start(consume, &consumer);
    REQUIRE(thread != NULL);
    /* ERROR_OUT_FAILURE: rumqttc_client_try_publish_buffer */
    REQUIRE(rumqttc_client_try_publish_buffer(NULL, native_string("a"), buffer, NULL, &operation, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(operation == 0);
    /* ERROR_OUT_SUCCESS: rumqttc_client_try_publish_buffer */
    CHECK(rumqttc_client_try_publish_buffer(client, native_string("a"), buffer, NULL, &operation, NULL));
    REQUIRE(operation != 0);
    for (unsigned qos = 0; qos <= 2; ++qos) {
        rumqttc_publish_options_t options = native_publish_options(qos);
        /* ERROR_OUT_FAILURE: rumqttc_client_publish_buffer_tracked */
        REQUIRE(rumqttc_client_publish_buffer_tracked(NULL, native_string("a"), buffer, &options, &completion, NULL) ==
                RUMQTTC_INVALID_ARGUMENT);
        REQUIRE(completion == NULL);
        /* ERROR_OUT_SUCCESS: rumqttc_client_publish_buffer_tracked */
        CHECK(rumqttc_client_publish_buffer_tracked(client, native_string("a"), buffer, &options, &completion, NULL));
        native_wait_completion(completion, qos == 0   ? RUMQTTC_COMPLETION_QOS0_FLUSHED
                                           : qos == 1 ? RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED
                                                      : RUMQTTC_COMPLETION_QOS2_COMPLETED);
        rumqttc_completion_destroy(completion);
    }
    /* ERROR_OUT_FAILURE: rumqttc_client_publish_buffer_deferred */
    REQUIRE(rumqttc_client_publish_buffer_deferred(NULL, native_string("a"), buffer, NULL, &pending, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(pending == NULL);
    /* ERROR_OUT_SUCCESS: rumqttc_client_publish_buffer_deferred */
    CHECK(rumqttc_client_publish_buffer_deferred(client, native_string("a"), buffer, NULL, &pending, NULL));
    CHECK(
        rumqttc_pending_admission_wait_timeout_ms(pending, NATIVE_DEADLINE_MS, &state, &operation, &completion, NULL));
    REQUIRE(state == RUMQTTC_ADMISSION_ADMITTED && operation != 0);
    native_wait_completion(completion, RUMQTTC_COMPLETION_QOS0_FLUSHED);
    rumqttc_completion_destroy(completion);
    rumqttc_pending_admission_release(pending);
    rumqttc_publish_buffer_release(buffer);
    REQUIRE(atomic_load(&calls) == 0);
    atomic_store(&consumer.stopped, 1);
    REQUIRE(native_thread_join(thread) == 0);
    native_close_destroy(client);
    if (execution != NULL) {
        CHECK(rumqttc_execution_context_request_shutdown(execution, NULL));
        CHECK(rumqttc_execution_context_join_timeout_ms(execution, NATIVE_DEADLINE_MS, NULL));
        rumqttc_execution_context_release(execution);
    }
    REQUIRE(atomic_load(&calls) == 0);
    rumqttc_publish_buffer_release(retained);
    REQUIRE(atomic_load(&calls) == 1);
    rumqttc_publish_buffer_release(NULL);
}

int main(void) {
    for (unsigned protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol)
        for (int shared = 0; shared != 2; ++shared)
            exercise(protocol, shared);
    return 0;
}
