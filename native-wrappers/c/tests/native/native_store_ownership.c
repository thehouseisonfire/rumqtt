#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

typedef struct ownership_context {
  atomic_uint active;
  atomic_uint peak;
  atomic_uint entered;
  atomic_uint released;
  atomic_uint reentrant;
  atomic_uintptr_t clients[3];
  atomic_uintptr_t completions[3];
} ownership_context;

static atomic_uint destroyed;

static void load_checkpoint(void *user_data, const rumqttc_store_request_t *request,
                            rumqttc_callback_completion_t *completion) {
  ownership_context *context = (ownership_context *)user_data;
  rumqttc_callback_completion_t *retained = NULL;
  rumqttc_publish_options_t publish = RUMQTTC_PUBLISH_OPTIONS_INIT;
  rumqttc_client_t *client;
  uint64_t operation = 0;
  unsigned index;
  unsigned active;
  unsigned peak;
  REQUIRE(request->operation == RUMQTTC_STORE_LOAD);
  REQUIRE(request->protocol == RUMQTTC_PROTOCOL_V4);
  CHECK(rumqttc_callback_completion_retain(completion, &retained));
  index = atomic_fetch_add(&context->entered, 1);
  REQUIRE(index < 3);
  const char *scope = index == 2 ? "different-scope" : "ownership-scope";
  REQUIRE(request->scope.len == strlen(scope));
  REQUIRE(memcmp(request->scope.data, scope, request->scope.len) == 0);
  atomic_store(&context->completions[index], (uintptr_t)retained);
  active = atomic_fetch_add(&context->active, 1) + 1;
  peak = atomic_load(&context->peak);
  while (active > peak && !atomic_compare_exchange_weak(&context->peak, &peak, active)) {
  }
  while ((client = (rumqttc_client_t *)atomic_load(&context->clients[index])) == NULL)
    native_sleep_ms(1);
  publish.qos = RUMQTTC_QOS_0;
  CHECK(rumqttc_client_try_publish(client, native_string("ownership/reentrant"), native_bytes(NULL, 0), &publish,
                                   &operation, NULL));
  REQUIRE(operation != 0);
  atomic_fetch_add(&context->reentrant, 1);
  while (!atomic_load(&context->released))
    native_sleep_ms(1);
  atomic_fetch_sub(&context->active, 1);
}

static void unexpected_write(void *user_data, const rumqttc_store_request_t *request,
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

static rumqttc_config_t *make_config(rumqttc_store_registration_t *registration, const char *client_id) {
  rumqttc_config_t *config = NULL;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  CHECK(rumqttc_config_set_v4_clean_session(config, 0, NULL));
  CHECK(rumqttc_config_set_session_store(config, registration, native_string("ownership-scope"), NATIVE_DEADLINE_MS,
                                         1024, NULL));
  return config;
}

int main(void) {
  rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
  ownership_context *context = calloc(1, sizeof(*context));
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_config_t *first_config, *second_config, *same_key_config, *different_scope_config;
  rumqttc_client_t *first = NULL, *second = NULL, *third = NULL, *rejected = (void *)1;
  rumqttc_error_t *error = NULL;
  uint8_t present = 0;
  uint32_t failure = 0;
  REQUIRE(context != NULL);
  vtable.load = load_checkpoint;
  vtable.save = unexpected_write;
  vtable.clear = unexpected_write;
  vtable.destroy = destroy_store;
  CHECK(rumqttc_store_registration_new(&vtable, context, &registration, NULL));
  first_config = make_config(registration, "ownership-first");
  second_config = make_config(registration, "ownership-second");
  same_key_config = make_config(registration, "ownership-first");
  CHECK(rumqttc_client_start(first_config, &first, NULL));
  atomic_store(&context->clients[0], (uintptr_t)first);
  for (unsigned attempt = 0; attempt < 500 && atomic_load(&context->entered) < 1; ++attempt)
    native_sleep_ms(10);
  REQUIRE(atomic_load(&context->entered) == 1);
  REQUIRE(rumqttc_client_start(same_key_config, &rejected, &error) != RUMQTTC_OK);
  REQUIRE(rejected == NULL && error != NULL);
  CHECK(rumqttc_error_store_failure(error, &present, &failure));
  REQUIRE(present == 1 && failure == RUMQTTC_STORE_FAILURE_IN_USE);
  rumqttc_error_destroy(error);
  CHECK(rumqttc_client_start(second_config, &second, NULL));
  atomic_store(&context->clients[1], (uintptr_t)second);
  for (unsigned attempt = 0; attempt < 500 && atomic_load(&context->reentrant) < 2; ++attempt)
    native_sleep_ms(10);
  REQUIRE(atomic_load(&context->reentrant) == 2);
  REQUIRE(atomic_load(&context->peak) == 2);
  different_scope_config = make_config(registration, "ownership-first");
  CHECK(rumqttc_config_set_session_store(different_scope_config, registration, native_string("different-scope"),
                                         NATIVE_DEADLINE_MS, 1024, NULL));
  CHECK(rumqttc_client_start(different_scope_config, &third, NULL));
  atomic_store(&context->clients[2], (uintptr_t)third);
  for (unsigned attempt = 0; attempt < 500 && atomic_load(&context->reentrant) < 3; ++attempt)
    native_sleep_ms(10);
  REQUIRE(atomic_load(&context->reentrant) == 3 && atomic_load(&context->peak) == 3);
  REQUIRE(rumqttc_client_destroy_timeout_ms(first, 20, &error) == RUMQTTC_TIMEOUT);
  rumqttc_error_destroy(error);
  REQUIRE(atomic_load(&destroyed) == 0);
  atomic_store(&context->released, 1);
  for (unsigned attempt = 0; attempt < 500 && atomic_load(&context->active) != 0; ++attempt)
    native_sleep_ms(10);
  REQUIRE(atomic_load(&context->active) == 0);
  native_close_destroy(first);
  native_close_destroy(second);
  native_close_destroy(third);
  rumqttc_config_destroy(first_config);
  rumqttc_config_destroy(second_config);
  rumqttc_config_destroy(same_key_config);
  rumqttc_config_destroy(different_scope_config);
  rumqttc_store_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == 0);
  for (unsigned index = 0; index < 3; ++index) {
    rumqttc_callback_completion_t *completion =
        (rumqttc_callback_completion_t *)atomic_load(&context->completions[index]);
    REQUIRE(completion != NULL);
    REQUIRE(rumqttc_callback_store_load_complete(completion, RUMQTTC_STORE_NOT_FOUND, native_bytes(NULL, 0)) ==
            RUMQTTC_INVALID_STATE);
    rumqttc_callback_completion_destroy(completion);
  }
  REQUIRE(atomic_load(&destroyed) == 1);
  return 0;
}
