#include "native_common.h"
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct context_t {
  atomic_uintptr_t pending;
  atomic_uint calls;
  int encrypted;
} context_t;
static atomic_uint destroyed;

static int equal(rumqttc_string_view_t view, const char *value) {
  return view.len == strlen(value) && memcmp(view.data, value, view.len) == 0;
}
static void prepare(void *data, const rumqttc_websocket_request_t *request, rumqttc_callback_completion_t *completion) {
  context_t *context = data;
  REQUIRE(equal(request->method, "GET") && equal(request->version, "HTTP/1.1"));
  REQUIRE(equal(request->path_and_query, "/initial"));
  REQUIRE(equal(request->broker_host, "localhost"));
  REQUIRE(request->tls_authority_present == context->encrypted);
  if (context->encrypted)
    REQUIRE(equal(request->tls_authority, "localhost"));
  REQUIRE(request->remaining_ns > 0);
  unsigned count = atomic_fetch_add(&context->calls, 1) + 1;
  REQUIRE(request->attempt == count);
  size_t duplicates = 0;
  for (size_t i = 0; i < request->header_count; ++i)
    if (equal(request->headers[i].name, "x-static"))
      ++duplicates;
  REQUIRE(duplicates == 2);
  rumqttc_callback_completion_t *token = NULL;
  CHECK(rumqttc_callback_completion_retain(completion, &token));
  REQUIRE(atomic_exchange(&context->pending, (uintptr_t)token) == 0);
}
static void destroy(void *data) {
  free(data);
  atomic_fetch_add(&destroyed, 1);
}
static rumqttc_callback_completion_t *pending(context_t *context) {
  uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
  while (!atomic_load(&context->pending)) {
    REQUIRE(native_monotonic_ms() < deadline);
    native_sleep_ms(1);
  }
  return (rumqttc_callback_completion_t *)atomic_exchange(&context->pending, 0);
}
static void finish(rumqttc_callback_completion_t *token, unsigned generation) {
  static const char *signatures[] = {"ed48f07d2ff61c6a5b3c9cca56dd9c524baf45c99a418f5d03f037648c91996a",
                                     "76d26fd5f3ee03aab4aa38b8f5f9652e0bedd3d0479c3bcbd1bdebd980b0e068"};
  char path[96], auth[96], authority[96];
  REQUIRE(snprintf(authority, sizeof(authority), "customer-%u.example:8443", generation) > 0);
  REQUIRE(snprintf(path, sizeof(path), "/dynamic?token=%u&encoded=%%2F", generation) > 0);
  REQUIRE(snprintf(auth, sizeof(auth), "Bearer dynamic-token-%u", generation) > 0);
  rumqttc_websocket_response_t *response = NULL;
  CHECK(rumqttc_websocket_response_new(&response, NULL));
  REQUIRE(rumqttc_websocket_response_header_edit(response, RUMQTTC_WEBSOCKET_HEADER_REPLACE, native_string("Host"),
                                                 native_bytes((const uint8_t *)auth, strlen(auth)),
                                                 NULL) == RUMQTTC_INVALID_ARGUMENT);
  CHECK(rumqttc_websocket_response_set_authority(response, native_string(authority), NULL));
  REQUIRE(rumqttc_websocket_response_set_authority(response, native_string("user@invalid.example"), NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  CHECK(rumqttc_websocket_response_set_path_and_query(response, native_string(path), NULL));
  CHECK(rumqttc_websocket_response_header_edit(response, RUMQTTC_WEBSOCKET_HEADER_REPLACE,
                                               native_string("authorization"),
                                               native_bytes((const uint8_t *)auth, strlen(auth)), NULL));
  CHECK(rumqttc_websocket_response_header_edit(response, RUMQTTC_WEBSOCKET_HEADER_REPLACE, native_string("x-signature"),
                                               native_bytes((const uint8_t *)signatures[generation - 1], 64), NULL));
  CHECK(rumqttc_websocket_response_header_edit(response, RUMQTTC_WEBSOCKET_HEADER_ADD, native_string("x-static"),
                                               native_bytes(NULL, 0), NULL));
  CHECK(rumqttc_websocket_response_header_edit(response, RUMQTTC_WEBSOCKET_HEADER_REMOVE, native_string("x-remove"),
                                               native_bytes(NULL, 0), NULL));
  memset(authority, 'x', strlen(authority));
  memset(path, 'x', strlen(path));
  memset(auth, 'x', strlen(auth));
  CHECK(rumqttc_callback_websocket_complete(token, response));
  rumqttc_websocket_response_destroy(response);
  REQUIRE(rumqttc_callback_websocket_complete(token, (const rumqttc_websocket_response_t *)(uintptr_t)1) ==
          RUMQTTC_INVALID_STATE);
  rumqttc_callback_completion_destroy(token);
}
static void run(rumqttc_protocol_t protocol, int encrypted) {
  context_t *context = calloc(1, sizeof(*context));
  REQUIRE(context != NULL);
  context->encrypted = encrypted;
  rumqttc_websocket_vtable_t vtable = RUMQTTC_WEBSOCKET_VTABLE_INIT;
  vtable.prepare = prepare;
  vtable.destroy = destroy;
  rumqttc_websocket_registration_t *registration = NULL;
  CHECK(rumqttc_websocket_registration_new(&vtable, context, &registration, NULL));
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  const char *port = getenv(encrypted ? "RUMQTTC_TEST_WSS_PORT" : "RUMQTTC_TEST_WS_PORT");
  REQUIRE(port != NULL);
  char url[128], id[128];
  REQUIRE(snprintf(url, sizeof(url), "%s://localhost:%s/initial", encrypted ? "wss" : "ws", port) > 0);
  REQUIRE(snprintf(id, sizeof(id), "native-dynamic-websocket-%u-%d", protocol, encrypted) > 0);
  CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
  if (encrypted) {
    const char *ca = getenv("RUMQTTC_TEST_CA_PEM");
    REQUIRE(ca != NULL);
    rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
    tls.backend =
        rumqttc_library_capabilities() & RUMQTTC_CAP_RUSTLS ? RUMQTTC_TLS_BACKEND_RUSTLS : RUMQTTC_TLS_BACKEND_NATIVE;
    tls.root_policy = RUMQTTC_TLS_ROOTS_PEM;
    tls.ca_pem = native_bytes((const uint8_t *)ca, strlen(ca));
    CHECK(rumqttc_config_set_transport_wss_with_options(config, native_string(url), &tls, NULL));
  } else
    CHECK(rumqttc_config_set_transport_websocket(config, native_string(url), NULL));
  rumqttc_websocket_header_edit_t edits[] = {RUMQTTC_WEBSOCKET_HEADER_EDIT_INIT, RUMQTTC_WEBSOCKET_HEADER_EDIT_INIT,
                                             RUMQTTC_WEBSOCKET_HEADER_EDIT_INIT};
  edits[0].name = native_string("x-static");
  edits[0].value = native_string("one");
  edits[1].name = native_string("x-static");
  edits[1].value = native_string("two");
  edits[2].name = native_string("x-remove");
  edits[2].value = native_string("remove");
  CHECK(rumqttc_config_set_websocket_header_edits(config, edits, 3, NULL));
  CHECK(rumqttc_config_set_websocket_handshake(config, registration, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_config_destroy(config);
  rumqttc_websocket_registration_destroy(registration);
  for (unsigned generation = 1; generation <= 2; ++generation) {
    finish(pending(context), generation);
    rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  }
  REQUIRE(atomic_load(&context->calls) == 2);
  native_close_destroy(client);
}
int main(void) {
  if (!(rumqttc_library_capabilities() & RUMQTTC_CAP_WEBSOCKET_CALLBACKS)) {
    rumqttc_websocket_registration_t *registration = (rumqttc_websocket_registration_t *)(uintptr_t)1;
    REQUIRE(rumqttc_websocket_registration_new(NULL, NULL, &registration, NULL) == RUMQTTC_CONFIG_ERROR);
    REQUIRE(registration == NULL);
    return 0;
  }
  for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
    run(protocol, 0);
    if (rumqttc_library_capabilities() & (RUMQTTC_CAP_RUSTLS | RUMQTTC_CAP_NATIVE_TLS))
      run(protocol, 1);
  }
  unsigned expected = rumqttc_library_capabilities() & (RUMQTTC_CAP_RUSTLS | RUMQTTC_CAP_NATIVE_TLS) ? 4 : 2;
  REQUIRE(atomic_load(&destroyed) == expected);
  return 0;
}
