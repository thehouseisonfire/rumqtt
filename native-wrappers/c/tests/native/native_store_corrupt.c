#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>

typedef struct store_context {
  const uint8_t *bytes;
  size_t length;
  atomic_uint loads;
} store_context;

static atomic_uint destroyed;

static void load_checkpoint(void *user_data,
                            const rumqttc_store_request_t *request,
                            rumqttc_callback_completion_t *completion) {
  store_context *context = (store_context *)user_data;
  REQUIRE(request->checkpoint.len == 0);
  atomic_fetch_add(&context->loads, 1);
  CHECK(rumqttc_callback_store_load_complete(completion,
                                             context->bytes == NULL
                                                 ? RUMQTTC_STORE_FAILED
                                                 : RUMQTTC_STORE_FOUND,
                                             native_bytes(context->bytes,
                                                          context->length)));
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

static void exercise_bad_checkpoint(rumqttc_protocol_t protocol,
                                    const uint8_t *bytes, size_t length,
                                    uint32_t expected_failure) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
  store_context *context = calloc(1, sizeof(*context));
  rumqttc_event_t *event;
  rumqttc_error_t *error = NULL;
  uint8_t failure_present = 0;
  uint32_t failure = 0;
  REQUIRE(context != NULL);
  context->bytes = bytes;
  context->length = length;
  vtable.load = load_checkpoint;
  vtable.save = unexpected_write;
  vtable.clear = unexpected_write;
  vtable.destroy = destroy_store;
  CHECK(rumqttc_store_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  1883, NULL));
  CHECK(rumqttc_config_set_client_id(config,
                                     native_string("native-store-corrupt"), NULL));
  if (protocol == RUMQTTC_PROTOCOL_V4) {
    CHECK(rumqttc_config_set_v4_clean_session(config, 0, NULL));
  } else {
    CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  }
  CHECK(rumqttc_config_set_session_store(config, registration,
                                         native_string("corrupt-scope"), 1000,
                                         64, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
  CHECK(rumqttc_event_disconnected(event, NULL, &error));
  REQUIRE(error != NULL);
  CHECK(rumqttc_error_store_failure(error, &failure_present, &failure));
  REQUIRE(failure_present == 1 && failure == expected_failure);
  rumqttc_error_destroy(error);
  rumqttc_event_destroy(event);
  REQUIRE(atomic_load(&context->loads) == 1);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
  rumqttc_store_registration_destroy(registration);
}

int main(void) {
  const uint8_t corrupt[] = {0, 0, 0, 0, 0, 0, 0, 0};
  const uint8_t version[] = {'R', 'M', 'W', 'C', 0, 2, 4, 0};
  const uint8_t protocol[] = {'R', 'M', 'W', 'C', 0, 1, 9, 0};
  const uint8_t oversized[65] = {0};
  for (rumqttc_protocol_t mqtt_version = RUMQTTC_PROTOCOL_V4;
       mqtt_version <= RUMQTTC_PROTOCOL_V5; ++mqtt_version) {
    exercise_bad_checkpoint(mqtt_version, NULL, 0,
                            RUMQTTC_STORE_FAILURE_LOAD);
    exercise_bad_checkpoint(mqtt_version, corrupt, sizeof(corrupt),
                            RUMQTTC_STORE_FAILURE_CORRUPT);
    exercise_bad_checkpoint(mqtt_version, version, sizeof(version),
                            RUMQTTC_STORE_FAILURE_VERSION);
    exercise_bad_checkpoint(mqtt_version, protocol, sizeof(protocol),
                            RUMQTTC_STORE_FAILURE_PROTOCOL);
    exercise_bad_checkpoint(mqtt_version, oversized, sizeof(oversized),
                            RUMQTTC_STORE_FAILURE_OVERSIZED);
  }
  REQUIRE(atomic_load(&destroyed) == 10);
  return 0;
}
