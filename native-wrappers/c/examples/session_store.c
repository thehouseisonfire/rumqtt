#include "example_common.h"

#include <stdio.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

typedef struct store_context {
  uint8_t *checkpoint;
  size_t length;
} store_context;

static atomic_uint destroy_count;
static atomic_int callback_failed;

static void store_load(void *user_data, const rumqttc_store_request_t *request,
                       rumqttc_callback_completion_t *completion) {
  store_context *context = (store_context *)user_data;
  rumqttc_bytes_view_t bytes = {context->checkpoint, context->length};
  (void)request;
  if (context->checkpoint == NULL) {
    if (rumqttc_callback_store_load_complete(
            completion, RUMQTTC_STORE_NOT_FOUND,
            (rumqttc_bytes_view_t){NULL, 0}) != RUMQTTC_OK)
      atomic_store(&callback_failed, 1);
  } else if (rumqttc_callback_store_load_complete(
                 completion, RUMQTTC_STORE_FOUND, bytes) != RUMQTTC_OK) {
    atomic_store(&callback_failed, 1);
  }
}

static void store_save(void *user_data, const rumqttc_store_request_t *request,
                       rumqttc_callback_completion_t *completion) {
  store_context *context = (store_context *)user_data;
  uint8_t *replacement = malloc(request->checkpoint.len ? request->checkpoint.len : 1);
  if (replacement == NULL) {
    if (rumqttc_callback_store_write_complete(completion,
                                              RUMQTTC_STORE_FAILED) != RUMQTTC_OK)
      atomic_store(&callback_failed, 1);
    return;
  }
  if (request->checkpoint.len != 0)
    memcpy(replacement, request->checkpoint.data, request->checkpoint.len);
  free(context->checkpoint);
  context->checkpoint = replacement;
  context->length = request->checkpoint.len;
  if (rumqttc_callback_store_write_complete(completion,
                                            RUMQTTC_STORE_FOUND) != RUMQTTC_OK)
    atomic_store(&callback_failed, 1);
}

static void store_clear(void *user_data, const rumqttc_store_request_t *request,
                        rumqttc_callback_completion_t *completion) {
  store_context *context = (store_context *)user_data;
  (void)request;
  free(context->checkpoint);
  context->checkpoint = NULL;
  context->length = 0;
  if (rumqttc_callback_store_write_complete(completion,
                                            RUMQTTC_STORE_FOUND) != RUMQTTC_OK)
    atomic_store(&callback_failed, 1);
}

static void store_destroy(void *user_data) {
  store_context *context = (store_context *)user_data;
  free(context->checkpoint);
  free(context);
  atomic_fetch_add(&destroy_count, 1);
}

int main(int argc, char **argv) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
  store_context *context;
  unsigned long parsed_port;
  int failed = 0;

  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }
  parsed_port = strtoul(argv[2], NULL, 10);
  if (parsed_port == 0 || parsed_port > 65535)
    return 2;
  context = calloc(1, sizeof(*context));
  if (context == NULL)
    return 1;
  vtable.load = store_load;
  vtable.save = store_save;
  vtable.clear = store_clear;
  vtable.destroy = store_destroy;
  if (example_report(rumqttc_store_registration_new(&vtable, context,
                                                    &registration, &error),
                     &error, "register store")) {
    free(context); /* Failed registration leaves user_data with the caller. */
    return 1;
  }
  failed |= example_report(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &config, &error),
                           &error, "new config");
  if (!failed)
    failed |= example_report(
        rumqttc_config_set_broker(config, example_string(argv[1]),
                                  (uint16_t)parsed_port, &error),
        &error, "broker");
  if (!failed)
    failed |= example_report(rumqttc_config_set_client_id(
                                 config, example_string("c-store-example"), &error),
                             &error, "client ID");
  if (!failed)
    failed |= example_report(rumqttc_config_set_v4_clean_session(config, 0, &error),
                             &error, "durable session");
  if (!failed)
    failed |= example_report(
        rumqttc_config_set_session_store(config, registration,
                                         example_string("example-scope"), 5000,
                                         16u * 1024u * 1024u, &error),
        &error, "session store");
  if (!failed)
    failed |= example_report(rumqttc_client_start(config, &client, &error),
                             &error, "start client");
  if (client != NULL) {
    failed |= example_report(rumqttc_client_close_now_timeout_ms(client, 5000, &error),
                             &error, "close client");
    if (example_report(rumqttc_client_destroy_timeout_ms(client, 5000, &error),
                       &error, "destroy client")) {
      rumqttc_client_abandon(client);
      failed = 1;
    }
  }
  rumqttc_config_destroy(config);
  rumqttc_store_registration_destroy(registration);
  rumqttc_error_destroy(error);
  if (atomic_load(&destroy_count) != 1 || atomic_load(&callback_failed))
    failed = 1;
  return failed;
}
