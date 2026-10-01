#define _POSIX_C_SOURCE 200809L
#include "example_common.h"
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#if defined(_WIN32)
#include <windows.h>
#else
#include <time.h>
#endif

/* Replace token preparation in the worker with your credential provider.
 * Request views must be copied before prepare() returns if a signer needs them. */
typedef struct token_context {
  atomic_uintptr_t pending;
  atomic_uint attempts;
  atomic_int stop;
  atomic_int failed;
} token_context;

static void prepare(void *data, const rumqttc_websocket_request_t *request, rumqttc_callback_completion_t *completion) {
  token_context *context = data;
  rumqttc_callback_completion_t *retained = NULL;
  (void)request;
  if (rumqttc_callback_completion_retain(completion, &retained) != RUMQTTC_OK) {
    atomic_store(&context->failed, 1);
    return;
  }
  rumqttc_callback_completion_destroy(
      (rumqttc_callback_completion_t *)atomic_exchange(&context->pending, (uintptr_t)retained));
}
static void destroy(void *data) { free(data); }
static int credentials(void *data) {
  token_context *context = data;
  while (!atomic_load(&context->stop)) {
    rumqttc_callback_completion_t *completion = (rumqttc_callback_completion_t *)atomic_exchange(&context->pending, 0);
    if (completion == NULL) {
#if defined(_WIN32)
      Sleep(1);
#else
      struct timespec delay = {0, 1000000L};
      (void)nanosleep(&delay, NULL);
#endif
      continue;
    }
    unsigned attempt = atomic_fetch_add(&context->attempts, 1) + 1;
    char authorization[96];
    int length = snprintf(authorization, sizeof(authorization), "Bearer demo-token-%u", attempt);
    rumqttc_websocket_response_t *response = NULL;
    rumqttc_status_t status = rumqttc_websocket_response_new(&response, NULL);
    if (status == RUMQTTC_OK && length > 0 && (size_t)length < sizeof(authorization))
      status = rumqttc_websocket_response_header_edit(response, RUMQTTC_WEBSOCKET_HEADER_REPLACE,
                                                      example_string("authorization"),
                                                      example_bytes(authorization, (size_t)length), NULL);
    else if (status == RUMQTTC_OK)
      status = RUMQTTC_INVALID_ARGUMENT;
    if (status == RUMQTTC_OK)
      status = rumqttc_callback_websocket_complete(completion, response);
    else
      (void)rumqttc_callback_websocket_reject(completion);
    /* A cancelled completion is expected when shutdown wins the race. */
    if (status != RUMQTTC_OK && status != RUMQTTC_INVALID_STATE)
      atomic_store(&context->failed, 1);
    rumqttc_websocket_response_destroy(response);
    rumqttc_callback_completion_destroy(completion);
  }
  rumqttc_callback_completion_destroy((rumqttc_callback_completion_t *)atomic_exchange(&context->pending, 0));
  return 0;
}
int main(int argc, char **argv) {
  if (argc != 3)
    return 2;
  const char *test_port = getenv("RUMQTTC_TEST_WS_PORT");
  unsigned long port = strtoul(test_port != NULL ? test_port : argv[2], NULL, 10);
  if (!port || port > 65535)
    return 2;
  char url[512];
  int length = snprintf(url, sizeof(url), "ws://%s:%lu/mqtt", argv[1], port);
  if (length <= 0 || (size_t)length >= sizeof(url))
    return 2;
  if (!(rumqttc_library_capabilities() & RUMQTTC_CAP_WEBSOCKET_CALLBACKS)) {
    fputs("WebSocket callbacks are unavailable; skipping the token example.\n", stderr);
    return 77;
  }
  token_context *context = calloc(1, sizeof(*context));
  if (context == NULL)
    return 1;
  rumqttc_websocket_vtable_t vtable = RUMQTTC_WEBSOCKET_VTABLE_INIT;
  vtable.prepare = prepare;
  vtable.destroy = destroy;
  rumqttc_websocket_registration_t *registration = NULL;
  if (rumqttc_websocket_registration_new(&vtable, context, &registration, NULL) != RUMQTTC_OK) {
    free(context);
    return 1;
  }
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  example_thread_t *worker = example_thread_start(credentials, context);
  int failed = worker == NULL;
  if (!failed)
    failed = rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL) != RUMQTTC_OK;
  if (!failed)
    failed = rumqttc_config_set_client_id(config, example_string("c-websocket-token-example"), NULL) != RUMQTTC_OK;
  if (!failed)
    failed = rumqttc_config_set_transport_websocket(config, example_string(url), NULL) != RUMQTTC_OK;
  if (!failed)
    failed = rumqttc_config_set_websocket_handshake(config, registration, NULL) != RUMQTTC_OK;
  if (!failed)
    failed = rumqttc_client_start(config, &client, NULL) != RUMQTTC_OK;
  if (!failed) {
    for (unsigned i = 0; i < (test_port != NULL ? 2u : 1u); ++i) {
      rumqttc_event_t *event = example_next_event(client, RUMQTTC_EVENT_CONNECTED);
      if (event == NULL) {
        failed = 1;
        break;
      }
      rumqttc_event_destroy(event);
    }
  }
  /* Keep the registration alive while joining the credential worker. */
  example_destroy_client(&client);
  atomic_store(&context->stop, 1);
  if (worker != NULL) {
    int result = 0;
    if (example_thread_join(worker, 5000, &result) != 0)
      return 1;
  }
  failed |= atomic_load(&context->failed);
  rumqttc_config_destroy(config);
  rumqttc_websocket_registration_destroy(registration);
  return failed;
}
