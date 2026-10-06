#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>

typedef struct callback_owner {
    rumqttc_client_t *peer;
    rumqttc_completion_t *diagnostics;
    atomic_uintptr_t pending;
} callback_owner;

static atomic_uint destroyed;

static void load(void *userdata, const rumqttc_store_request_t *request, rumqttc_callback_completion_t *completion) {
    callback_owner *owner = userdata;
    rumqttc_callback_completion_t *retained = NULL;
    rumqttc_event_t *event = NULL;
    (void)request;
    REQUIRE(rumqttc_client_destroy_timeout_ms(owner->peer, 1, NULL) == RUMQTTC_INVALID_STATE);
    REQUIRE(rumqttc_client_close_now_timeout_ms(owner->peer, 1, NULL) == RUMQTTC_INVALID_STATE);
    REQUIRE(rumqttc_client_event_recv_timeout_ms(owner->peer, 1, &event, NULL) == RUMQTTC_INVALID_STATE);
    REQUIRE(event == NULL);
    REQUIRE(rumqttc_completion_wait_timeout_ms(owner->diagnostics, 1, NULL) == RUMQTTC_INVALID_STATE);
    CHECK(rumqttc_callback_completion_retain(completion, &retained));
    atomic_store(&owner->pending, (uintptr_t)retained);
}

static void unexpected(void *userdata, const rumqttc_store_request_t *request,
                       rumqttc_callback_completion_t *completion) {
    (void)userdata;
    (void)request;
    (void)completion;
    REQUIRE(0);
}

static void destroy(void *userdata) {
    free(userdata);
    atomic_fetch_add(&destroyed, 1);
}

static void run_case(rumqttc_protocol_t protocol) {
    rumqttc_execution_options_t options = RUMQTTC_EXECUTION_OPTIONS_INIT;
    rumqttc_execution_context_t *context = NULL;
    rumqttc_config_t *config = NULL;
    rumqttc_client_t *client = NULL;
    rumqttc_store_registration_t *store = NULL;
    rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
    callback_owner *owner = calloc(1, sizeof(*owner));
    rumqttc_callback_completion_t *token;
    uint32_t state = 99;
    unsigned initial_destroyed = atomic_load(&destroyed);
    REQUIRE(owner != NULL);
    atomic_init(&owner->pending, (uintptr_t)0);
    options.worker_threads = 1;
    options.client_capacity = 2;
    CHECK(rumqttc_execution_context_new(&options, &context, NULL));
    CHECK(rumqttc_config_new(protocol, &config, NULL));
    CHECK(rumqttc_config_set_execution_context(config, context, NULL));
    CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), 65535, NULL));
    CHECK(rumqttc_config_set_client_id(config, native_string("execution-peer"), NULL));
    CHECK(rumqttc_client_start(config, &owner->peer, NULL));
    CHECK(rumqttc_client_diagnostics_tracked(owner->peer, &owner->diagnostics, NULL));
    vtable.load = load;
    vtable.save = unexpected;
    vtable.clear = unexpected;
    vtable.destroy = destroy;
    CHECK(rumqttc_store_registration_new(&vtable, owner, &store, NULL));
    CHECK(rumqttc_config_set_client_id(config, native_string("execution-store"), NULL));
    if (protocol == RUMQTTC_PROTOCOL_V4)
        CHECK(rumqttc_config_set_v4_clean_session(config, 0, NULL));
    else
        CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
    CHECK(rumqttc_config_set_session_store(config, store, native_string("execution"), 5000, 1024, NULL));
    CHECK(rumqttc_client_start(config, &client, NULL));
    for (unsigned i = 0; i < 500 && atomic_load(&owner->pending) == 0; ++i)
        native_sleep_ms(10);
    token = (rumqttc_callback_completion_t *)atomic_load(&owner->pending);
    REQUIRE(token != NULL);
    CHECK(rumqttc_execution_context_request_shutdown(context, NULL));
    CHECK(rumqttc_execution_context_request_shutdown(context, NULL));
    CHECK(rumqttc_execution_context_join_timeout_ms(context, NATIVE_DEADLINE_MS, NULL));
    CHECK(rumqttc_execution_context_state(context, &state, NULL));
    REQUIRE(state == RUMQTTC_EXECUTION_QUIESCENT);
    CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    CHECK(rumqttc_client_destroy_timeout_ms(owner->peer, NATIVE_DEADLINE_MS, NULL));
    rumqttc_completion_destroy(owner->diagnostics);
    rumqttc_config_destroy(config);
    rumqttc_store_registration_destroy(store);
    rumqttc_execution_context_release(context);
    REQUIRE(atomic_load(&destroyed) == initial_destroyed);
    REQUIRE(rumqttc_callback_store_load_complete(token, RUMQTTC_STORE_NOT_FOUND, native_bytes(NULL, 0)) ==
            RUMQTTC_INVALID_STATE);
    rumqttc_callback_completion_destroy(token);
    REQUIRE(atomic_load(&destroyed) == initial_destroyed + 1);
}

int main(void) {
    size_t baseline = native_process_thread_count();
    run_case(RUMQTTC_PROTOCOL_V4);
    run_case(RUMQTTC_PROTOCOL_V5);
    for (unsigned i = 0; i < 32; ++i) {
        rumqttc_execution_context_t *context = NULL;
        CHECK(rumqttc_execution_context_new(NULL, &context, NULL));
        CHECK(rumqttc_execution_context_request_shutdown(context, NULL));
        CHECK(rumqttc_execution_context_join_timeout_ms(context, NATIVE_DEADLINE_MS, NULL));
        rumqttc_execution_context_release(context);
    }
    for (unsigned i = 0; i < 100 && native_process_thread_count() != baseline; ++i)
        native_sleep_ms(10);
    REQUIRE(native_process_thread_count() == baseline);
    return 0;
}
