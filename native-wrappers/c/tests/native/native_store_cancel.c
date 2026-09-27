#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>

typedef struct store_context {
  atomic_uintptr_t pending;
} store_context;

static atomic_uint destroyed;

static void load_checkpoint(void *user_data,
                            const rumqttc_store_request_t *request,
                            rumqttc_callback_completion_t *completion) {
  store_context *context = (store_context *)user_data;
  rumqttc_callback_completion_t *retained = NULL;
  REQUIRE(request->checkpoint.len == 0);
  CHECK(rumqttc_callback_completion_retain(completion, &retained));
  REQUIRE(atomic_exchange(&context->pending, (uintptr_t)retained) == 0);
}

static void unexpected_write(void *user_data,
                             const rumqttc_store_request_t *request,
                             rumqttc_callback_completion_t *completion) {
  (void)user_data;
  (void)request;
  (void)completion;
  REQUIRE(0);
}

static void destroy_store(void *user_data) {
  free(user_data);
  atomic_fetch_add(&destroyed, 1);
}

static void run_case(rumqttc_protocol_t protocol, const char *client_id,
                     int wait_for_timeout) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
  store_context *context = calloc(1, sizeof(*context));
  rumqttc_callback_completion_t *pending;
  unsigned expected_destroyed = atomic_load(&destroyed);
  REQUIRE(context != NULL);
  vtable.load = load_checkpoint;
  vtable.save = unexpected_write;
  vtable.clear = unexpected_write;
  vtable.destroy = destroy_store;
  CHECK(rumqttc_store_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), 1883,
                                  NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  if (protocol == RUMQTTC_PROTOCOL_V4)
    CHECK(rumqttc_config_set_v4_clean_session(config, 0, NULL));
  else
    CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_config_set_session_store(config, registration,
                                         native_string("cancel-scope"),
                                         wait_for_timeout ? 100 : 5000,
                                         1024, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  for (unsigned attempt = 0; attempt < 500 &&
                             atomic_load(&context->pending) == 0; ++attempt)
    native_sleep_ms(10);
  pending = (rumqttc_callback_completion_t *)atomic_exchange(&context->pending, 0);
  REQUIRE(pending != NULL);
  if (wait_for_timeout) {
    rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
    rumqttc_error_t *error = NULL;
    uint8_t present = 0;
    uint32_t failure = 0;
    CHECK(rumqttc_event_disconnected(event, NULL, &error));
    REQUIRE(error != NULL);
    CHECK(rumqttc_error_store_failure(error, &present, &failure));
    REQUIRE(present == 1 && failure == RUMQTTC_STORE_FAILURE_TIMEOUT);
    rumqttc_error_destroy(error);
    rumqttc_event_destroy(event);
  }
  native_close_destroy(client);
  REQUIRE(rumqttc_callback_store_load_complete(
              pending, RUMQTTC_STORE_NOT_FOUND, native_bytes(NULL, 0)) ==
          RUMQTTC_INVALID_STATE);
  rumqttc_config_destroy(config);
  rumqttc_store_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == expected_destroyed);
  rumqttc_callback_completion_destroy(pending);
  REQUIRE(atomic_load(&destroyed) == expected_destroyed + 1);
}

int main(void) {
  run_case(RUMQTTC_PROTOCOL_V4, "native-store-cancel-v4", 0);
  run_case(RUMQTTC_PROTOCOL_V5, "native-store-cancel-v5", 0);
  run_case(RUMQTTC_PROTOCOL_V4, "native-store-timeout-v4", 1);
  run_case(RUMQTTC_PROTOCOL_V5, "native-store-timeout-v5", 1);
  REQUIRE(atomic_load(&destroyed) == 4);
  return 0;
}
