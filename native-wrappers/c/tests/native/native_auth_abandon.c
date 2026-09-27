#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>

typedef struct auth_context {
  atomic_uintptr_t pending;
} auth_context;

static atomic_uint destroyed;

static void auth_respond(void *user_data, const rumqttc_auth_request_t *request,
                         rumqttc_callback_completion_t *completion) {
  auth_context *context = (auth_context *)user_data;
  if (request->stage == RUMQTTC_AUTH_REQUEST_START) {
    rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("custom");
    CHECK(rumqttc_callback_auth_complete(completion, &response));
  } else if (request->stage == RUMQTTC_AUTH_REQUEST_CONTINUE) {
    rumqttc_callback_completion_t *retained = NULL;
    CHECK(rumqttc_callback_completion_retain(completion, &retained));
    REQUIRE(atomic_exchange(&context->pending, (uintptr_t)retained) == 0);
  } else {
    REQUIRE(0);
  }
}

static void auth_destroy(void *user_data) {
  free(user_data);
  atomic_fetch_add(&destroyed, 1);
}

int main(void) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_auth_registration_t *registration = NULL;
  rumqttc_auth_vtable_t vtable = RUMQTTC_AUTH_VTABLE_INIT;
  auth_context *context = calloc(1, sizeof(*context));
  rumqttc_callback_completion_t *pending;
  rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
  REQUIRE(context != NULL);
  vtable.respond = auth_respond;
  vtable.destroy = auth_destroy;
  CHECK(rumqttc_auth_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config,
                                     native_string("native-v5-auth-abandon"), NULL));
  CHECK(rumqttc_config_set_v5_authenticator(config, registration,
                                            native_string("custom"), 5000, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  for (unsigned attempt = 0; attempt < 500 &&
                             atomic_load(&context->pending) == 0; ++attempt)
    native_sleep_ms(10);
  pending = (rumqttc_callback_completion_t *)atomic_exchange(&context->pending, 0);
  REQUIRE(pending != NULL);
  rumqttc_client_abandon(client);
  native_sleep_ms(500);
  response.action = RUMQTTC_AUTH_ACTION_SEND;
  response.method_present = 1;
  response.method = native_string("custom");
  REQUIRE(rumqttc_callback_auth_complete(pending, &response) ==
          RUMQTTC_INVALID_STATE);
  rumqttc_config_destroy(config);
  rumqttc_auth_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == 0);
  rumqttc_callback_completion_destroy(pending);
  for (unsigned attempt = 0; attempt < 500 &&
                             atomic_load(&destroyed) == 0; ++attempt)
    native_sleep_ms(10);
  REQUIRE(atomic_load(&destroyed) == 1);
  return 0;
}
