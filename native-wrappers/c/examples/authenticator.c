#include "example_common.h"

#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct auth_context {
  atomic_uint starts;
  atomic_uint successes;
} auth_context;

static atomic_uint destroyed;

static void respond(void *user_data, const rumqttc_auth_request_t *request,
                    rumqttc_callback_completion_t *completion) {
  auth_context *context = (auth_context *)user_data;
  rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
  if (request->stage == RUMQTTC_AUTH_REQUEST_START) {
    atomic_fetch_add(&context->starts, 1);
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = example_string("demo");
    response.data_present = 1;
    response.data = example_bytes("hello", 5);
  } else if (request->stage == RUMQTTC_AUTH_REQUEST_SUCCESS) {
    atomic_fetch_add(&context->successes, 1);
  } else {
    response.action = RUMQTTC_AUTH_ACTION_REJECT;
  }
  if (rumqttc_callback_auth_complete(completion, &response) != RUMQTTC_OK)
    atomic_store(&context->starts, 0);
}

static void destroy(void *user_data) {
  free(user_data);
  atomic_fetch_add(&destroyed, 1);
}

int main(int argc, char **argv) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_auth_registration_t *registration = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_auth_vtable_t vtable = RUMQTTC_AUTH_VTABLE_INIT;
  auth_context *context;
  unsigned long port;
  int failed = 0;
  if (argc != 3 || (rumqttc_library_capabilities() & RUMQTTC_CAP_AUTH_CALLBACKS) == 0)
    return 2;
  port = strtoul(argv[2], NULL, 10);
  if (port == 0 || port > 65535)
    return 2;
  context = calloc(1, sizeof(*context));
  if (context == NULL)
    return 1;
  vtable.respond = respond;
  vtable.destroy = destroy;
  if (example_report(rumqttc_auth_registration_new(&vtable, context,
                                                    &registration, &error),
                     &error, "register authenticator")) {
    free(context); /* Failed registration leaves user_data with the caller. */
    return 1;
  }
  failed |= example_report(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config,
                                              &error), &error, "new config");
  if (!failed)
    failed |= example_report(rumqttc_config_set_broker(
                                 config, example_string(argv[1]), (uint16_t)port,
                                 &error), &error, "broker");
  if (!failed)
    failed |= example_report(rumqttc_config_set_client_id(
                                 config, example_string("c-auth-example"),
                                 &error), &error, "client ID");
  if (!failed)
    failed |= example_report(rumqttc_config_set_v5_authenticator(
                                 config, registration, example_string("demo"),
                                 5000, &error), &error, "authenticator");
  if (!failed)
    failed |= example_report(rumqttc_client_start(config, &client, &error),
                             &error, "start client");
  if (client != NULL) {
    rumqttc_event_t *event = example_next_event(client, RUMQTTC_EVENT_CONNECTED);
    if (event == NULL || atomic_load(&context->starts) != 1 ||
        atomic_load(&context->successes) != 1)
      failed = 1;
    rumqttc_event_destroy(event);
    failed |= example_report(rumqttc_client_close_now_timeout_ms(client, 5000,
                                                                  &error),
                             &error, "close client");
    if (example_report(rumqttc_client_destroy_timeout_ms(client, 5000, &error),
                       &error, "destroy client")) {
      rumqttc_client_abandon(client);
      failed = 1;
    }
  }
  rumqttc_config_destroy(config);
  rumqttc_auth_registration_destroy(registration);
  rumqttc_error_destroy(error);
  if (atomic_load(&destroyed) != 1)
    failed = 1;
  return failed;
}
