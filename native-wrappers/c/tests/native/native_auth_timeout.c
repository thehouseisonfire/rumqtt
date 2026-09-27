#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>

typedef struct auth_context {
  atomic_uintptr_t pending;
  atomic_uint failures;
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

static void auth_failed(void *user_data, const rumqttc_auth_request_t *request,
                        uint32_t failure) {
  auth_context *context = (auth_context *)user_data;
  REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_FAILED);
  REQUIRE(failure == RUMQTTC_AUTH_FAILURE_TIMEOUT);
  atomic_fetch_add(&context->failures, 1);
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
  rumqttc_event_t *event;
  rumqttc_error_t *error = NULL;
  rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
  uint32_t stage = 0;
  uint8_t failure_present = 0;
  uint32_t failure = 0;
  REQUIRE(context != NULL);
  vtable.respond = auth_respond;
  vtable.failed = auth_failed;
  vtable.destroy = auth_destroy;
  CHECK(rumqttc_auth_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config,
                                     native_string("native-v5-auth-timeout"), NULL));
  CHECK(rumqttc_config_set_v5_authenticator(config, registration,
                                            native_string("custom"), 100, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  for (unsigned attempt = 0; attempt < 500 &&
                             atomic_load(&context->failures) == 0; ++attempt)
    native_sleep_ms(10);
  REQUIRE(atomic_load(&context->failures) == 1);
  event = native_wait_event(client, RUMQTTC_EVENT_AUTHENTICATION);
  CHECK(rumqttc_event_authentication(event, NULL, &stage, NULL, NULL, NULL));
  REQUIRE(stage == RUMQTTC_AUTH_STAGE_STARTED);
  rumqttc_event_destroy(event);
  event = native_wait_event(client, RUMQTTC_EVENT_AUTHENTICATION);
  CHECK(rumqttc_event_authentication(event, NULL, &stage, NULL, NULL, NULL));
  REQUIRE(stage == RUMQTTC_AUTH_STAGE_CONTINUE);
  rumqttc_event_destroy(event);
  event = native_wait_event(client, RUMQTTC_EVENT_AUTHENTICATION);
  CHECK(rumqttc_event_authentication(event, NULL, &stage, NULL, NULL, NULL));
  REQUIRE(stage == RUMQTTC_AUTH_STAGE_FAILED);
  rumqttc_event_destroy(event);
  event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
  CHECK(rumqttc_event_disconnected(event, NULL, &error));
  REQUIRE(error != NULL);
  CHECK(rumqttc_error_auth_failure(error, &failure_present, &failure));
  REQUIRE(failure_present == 1 && failure == RUMQTTC_AUTH_FAILURE_TIMEOUT);
  rumqttc_error_destroy(error);
  rumqttc_event_destroy(event);
  pending = (rumqttc_callback_completion_t *)atomic_exchange(&context->pending, 0);
  REQUIRE(pending != NULL);
  response.action = RUMQTTC_AUTH_ACTION_SEND;
  response.method_present = 1;
  response.method = native_string("custom");
  REQUIRE(rumqttc_callback_auth_complete(pending, &response) ==
          RUMQTTC_INVALID_STATE);
  rumqttc_callback_completion_destroy(pending);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
  rumqttc_auth_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == 1);
  return 0;
}
