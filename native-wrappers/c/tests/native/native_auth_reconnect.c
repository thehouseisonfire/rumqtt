#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>

typedef struct auth_context {
  atomic_uint starts;
  atomic_uint challenges;
  atomic_uint successes;
} auth_context;

static atomic_uint destroyed;

static void auth_respond(void *user_data, const rumqttc_auth_request_t *request,
                         rumqttc_callback_completion_t *completion) {
  auth_context *context = (auth_context *)user_data;
  rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
  if (request->stage == RUMQTTC_AUTH_REQUEST_START) {
    atomic_fetch_add(&context->starts, 1);
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("custom");
  } else if (request->stage == RUMQTTC_AUTH_REQUEST_CONTINUE) {
    atomic_fetch_add(&context->challenges, 1);
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("custom");
  } else if (request->stage == RUMQTTC_AUTH_REQUEST_SUCCESS) {
    atomic_fetch_add(&context->successes, 1);
    response.action = RUMQTTC_AUTH_ACTION_COMPLETE;
  } else {
    REQUIRE(0);
  }
  CHECK(rumqttc_callback_auth_complete(completion, &response));
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
  unsigned connected = 0;
  REQUIRE(context != NULL);
  vtable.respond = auth_respond;
  vtable.destroy = auth_destroy;
  CHECK(rumqttc_auth_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config,
                                     native_string("native-v5-auth-reconnect"), NULL));
  CHECK(rumqttc_config_set_v5_authenticator(config, registration,
                                            native_string("custom"), 5000, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  while (connected < 2) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS,
                                               &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    REQUIRE(kind != RUMQTTC_EVENT_DRIVER_TERMINATED);
    if (kind == RUMQTTC_EVENT_CONNECTED)
      ++connected;
    rumqttc_event_destroy(event);
  }
  REQUIRE(atomic_load(&context->starts) == 2);
  REQUIRE(atomic_load(&context->challenges) == 2);
  REQUIRE(atomic_load(&context->successes) == 2);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
  rumqttc_auth_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == 1);
  return 0;
}
