#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct redirect_context {
  uint16_t port;
  size_t host_length;
  char host[];
} redirect_context;

/* An exact application allowlist. The Server Reference itself is not authority. */
static uint32_t decide(void *data, const rumqttc_redirect_request_t *request, rumqttc_redirect_response_t *response) {
  const redirect_context *context = data;
  rumqttc_redirect_request_info_t info = RUMQTTC_REDIRECT_REQUEST_INFO_INIT;
  uint32_t status = rumqttc_redirect_request_info(request, &info, NULL);
  if (status != RUMQTTC_OK)
    return status;
  for (size_t i = 0; i < info.reference_count; ++i) {
    rumqttc_redirect_reference_t reference = RUMQTTC_REDIRECT_REFERENCE_INIT;
    status = rumqttc_redirect_request_reference(request, i, &reference, NULL);
    if (status != RUMQTTC_OK)
      return status;
    if (reference.kind != RUMQTTC_REDIRECT_REFERENCE_AUTHORITY || reference.port != context->port ||
        reference.host.len != context->host_length ||
        memcmp(reference.host.data, context->host, context->host_length) != 0)
      continue;
    status = rumqttc_redirect_response_follow(response, request, i, RUMQTTC_REDIRECT_TRANSPORT_TCP, NULL, NULL);
    if (status != RUMQTTC_OK)
      return status;
    /* A replacement identity still starts an isolated clean session by default. */
    return rumqttc_redirect_response_set_client_id(response, RUMQTTC_REDIRECT_CLIENT_ID_REPLACE,
                                                   example_string("c-redirect-target"), NULL);
  }
  return rumqttc_redirect_response_reject(response, NULL);
}

static void destroy(void *data) { free(data); }

/* Demo: HOST PORT; approves only localhost:PORT, using plaintext TCP. */
int main(int argc, char **argv) {
  if (argc != 3)
    return 2;
  char *end = NULL;
  unsigned long port = strtoul(argv[2], &end, 10);
  if (end == argv[2] || *end != '\0' || port == 0 || port > UINT16_MAX)
    return 2;
  redirect_context *context = malloc(sizeof(*context) + sizeof("localhost"));
  if (context == NULL)
    return 1;
  context->port = (uint16_t)port;
  context->host_length = strlen("localhost");
  memcpy(context->host, "localhost", sizeof("localhost"));
  rumqttc_redirect_vtable_t vtable = RUMQTTC_REDIRECT_VTABLE_INIT;
  vtable.decide = decide;
  vtable.destroy = destroy;
  rumqttc_redirect_registration_t *registration = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_event_t *event = NULL;
  rumqttc_error_t *error = NULL;
  int result = 1;
  uint32_t status = rumqttc_redirect_registration_new(&vtable, context, 2, 1000, &registration, &error);
  if (example_report(status, &error, "redirect_registration_new")) {
    free(context); /* Failed registration does not transfer ownership. */
    goto cleanup;
  }
  if (example_report(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, &error), &error, "config_new") ||
      example_report(rumqttc_config_set_broker(config, example_string(argv[1]), (uint16_t)port, &error), &error,
                     "set_broker") ||
      example_report(rumqttc_config_set_client_id(config, example_string("c-redirect-example"), &error), &error,
                     "set_client_id") ||
      example_report(rumqttc_config_set_v5_redirect_authority(config, registration, &error), &error,
                     "set_redirect_authority") ||
      example_report(rumqttc_client_start(config, &client, &error), &error, "client_start"))
    goto cleanup;
  rumqttc_redirect_registration_destroy(registration);
  registration = NULL;
  rumqttc_config_destroy(config);
  config = NULL;
  event = example_next_event(client, RUMQTTC_EVENT_CONNECTED);
  if (event == NULL)
    goto cleanup;
  if (example_report(rumqttc_client_close_timeout_ms(client, 5000, &error), &error, "close"))
    goto cleanup;
  result = 0;
cleanup:
  rumqttc_event_destroy(event);
  rumqttc_redirect_registration_destroy(registration);
  rumqttc_config_destroy(config);
  rumqttc_error_destroy(error);
  example_destroy_client(&client);
  return result;
}
