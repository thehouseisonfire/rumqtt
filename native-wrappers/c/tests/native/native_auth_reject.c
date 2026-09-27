#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>

typedef struct auth_context {
  unsigned starts;
  unsigned challenges;
  unsigned failures;
} auth_context;

static atomic_uint destroyed;

static void auth_respond(void *user_data, const rumqttc_auth_request_t *request,
                         rumqttc_callback_completion_t *completion) {
  auth_context *context = (auth_context *)user_data;
  rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
  if (request->stage == RUMQTTC_AUTH_REQUEST_START) {
    ++context->starts;
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("custom");
  } else if (request->stage == RUMQTTC_AUTH_REQUEST_CONTINUE) {
    ++context->challenges;
    REQUIRE(request->reason_code_present == 1 && request->reason_code == 0x18);
    response.action = RUMQTTC_AUTH_ACTION_REJECT;
  } else {
    REQUIRE(0);
  }
  CHECK(rumqttc_callback_auth_complete(completion, &response));
}

static void auth_failed(void *user_data, const rumqttc_auth_request_t *request,
                        uint32_t failure) {
  auth_context *context = (auth_context *)user_data;
  REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_FAILED);
  REQUIRE(failure == RUMQTTC_AUTH_FAILURE_REJECTED);
  ++context->failures;
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
  rumqttc_event_t *event;
  rumqttc_error_t *error = NULL;
  uint8_t present = 0;
  uint32_t failure = 0;
  unsigned saw_failed = 0;
  REQUIRE(context != NULL);
  vtable.respond = auth_respond;
  vtable.failed = auth_failed;
  vtable.destroy = auth_destroy;
  CHECK(rumqttc_auth_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config,
                                     native_string("native-v5-auth-reject"), NULL));
  CHECK(rumqttc_config_set_v5_authenticator(config, registration,
                                            native_string("custom"), 5000, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  for (;;) {
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS,
                                               &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    if (kind == RUMQTTC_EVENT_AUTHENTICATION) {
      uint32_t stage = 0;
      CHECK(rumqttc_event_authentication(event, NULL, &stage, NULL, NULL,
                                         NULL));
      if (stage == RUMQTTC_AUTH_STAGE_FAILED)
        saw_failed = 1;
    } else if (kind == RUMQTTC_EVENT_DRIVER_TERMINATED) {
      CHECK(rumqttc_event_disconnected(event, NULL, &error));
      rumqttc_event_destroy(event);
      break;
    }
    rumqttc_event_destroy(event);
  }
  REQUIRE(saw_failed == 1);
  REQUIRE(context->starts == 1 && context->challenges == 1 &&
          context->failures == 1);
  REQUIRE(error != NULL);
  CHECK(rumqttc_error_auth_failure(error, &present, &failure));
  REQUIRE(present == 1 && failure == RUMQTTC_AUTH_FAILURE_REJECTED);
  rumqttc_error_destroy(error);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
  rumqttc_auth_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == 1);
  return 0;
}
