#include "native_common.h"
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct authority_context {
  const char *mode;
  atomic_uint calls;
  atomic_uint destroyed;
  atomic_uint store_destroyed;
  atomic_uint target_store_calls;
  atomic_uintptr_t pending;
  atomic_uint auth_starts;
  atomic_uint auth_successes;
  atomic_uint auth_destroyed;
  rumqttc_redirect_request_t *request;
  const rumqttc_redirect_request_t *stale;
} authority_context;

static int same_mode(const authority_context *context, const char *mode) { return strcmp(context->mode, mode) == 0; }
static int network_mode(const authority_context *context) { return strncmp(context->mode, "network-", 8) == 0; }
static int auth_mode(const authority_context *context) { return strncmp(context->mode, "auth-", 5) == 0; }
static int isolated_mode(const authority_context *context) {
  return same_mode(context, "isolated") || network_mode(context) || auth_mode(context);
}
static void auth_respond(void *data, const rumqttc_auth_request_t *request, rumqttc_callback_completion_t *completion) {
  authority_context *context = data;
  rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
  if (request->stage == RUMQTTC_AUTH_REQUEST_START) {
    atomic_fetch_add(&context->auth_starts, 1);
    if (atomic_load(&context->calls) == 1) {
      REQUIRE(same_mode(context, "auth-reuse"));
      const char *id = "native-authority-target-auth-reuse";
      REQUIRE(request->client_id.len == strlen(id) && memcmp(request->client_id.data, id, strlen(id)) == 0);
    }
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("test");
  } else {
    REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_SUCCESS);
    atomic_fetch_add(&context->auth_successes, 1);
  }
  CHECK(rumqttc_callback_auth_complete(completion, &response));
}
static void auth_failed(void *data, const rumqttc_auth_request_t *request, uint32_t failure) {
  (void)data;
  (void)failure;
  REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_FAILED);
}
static void auth_destroy(void *data) {
  authority_context *context = data;
  REQUIRE(atomic_fetch_add(&context->auth_destroyed, 1) == 0);
}

static uint32_t decide(void *data, const rumqttc_redirect_request_t *request, rumqttc_redirect_response_t *response) {
  authority_context *context = data;
  rumqttc_redirect_request_info_t info = RUMQTTC_REDIRECT_REQUEST_INFO_INIT;
  rumqttc_redirect_reference_t reference = RUMQTTC_REDIRECT_REFERENCE_INIT;
  REQUIRE(atomic_fetch_add(&context->calls, 1) == 0);
  CHECK(rumqttc_redirect_request_info(request, &info, NULL));
  REQUIRE(info.source == RUMQTTC_REDIRECT_SOURCE_CONNACK && info.attempt == 1 && info.reference_count == 2);
  CHECK(rumqttc_redirect_request_reference(request, 1, &reference, NULL));
  REQUIRE(reference.kind ==
              (network_mode(context) ? RUMQTTC_REDIRECT_REFERENCE_URI : RUMQTTC_REDIRECT_REFERENCE_AUTHORITY) &&
          reference.port > 0);
  CHECK(rumqttc_redirect_request_retain(request, &context->request, NULL));
  if (same_mode(context, "reject"))
    return rumqttc_redirect_response_reject(response, NULL);
  if (same_mode(context, "callback"))
    return RUMQTTC_INTERNAL_ERROR;
  if (same_mode(context, "timeout")) {
    native_sleep_ms(20);
    return RUMQTTC_OK;
  }
  if (same_mode(context, "invalid")) {
    REQUIRE(rumqttc_redirect_response_follow(response, request, 999, RUMQTTC_REDIRECT_TRANSPORT_TCP, NULL, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    return RUMQTTC_OK; /* An ignored builder error still invalidates the decision. */
  }
  if (same_mode(context, "stale")) {
    REQUIRE(rumqttc_redirect_response_follow(response, context->stale, 1, RUMQTTC_REDIRECT_TRANSPORT_TCP, NULL, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    return RUMQTTC_OK;
  }
  CHECK(rumqttc_redirect_response_follow(
      response, request, 1, network_mode(context) ? RUMQTTC_REDIRECT_TRANSPORT_WS : RUMQTTC_REDIRECT_TRANSPORT_TCP,
      NULL, NULL));
  char client_id[100];
  REQUIRE(snprintf(client_id, sizeof(client_id), "native-authority-target-%s", context->mode) > 0);
  CHECK(rumqttc_redirect_response_set_client_id(
      response, same_mode(context, "same") ? RUMQTTC_REDIRECT_CLIENT_ID_REUSE : RUMQTTC_REDIRECT_CLIENT_ID_REPLACE,
      same_mode(context, "same") ? native_string("") : native_string(client_id), NULL));
  char username[] = "target-private-user";
  uint8_t password[] = "target-private-password";
  CHECK(rumqttc_redirect_response_set_credentials(response, 1, native_string(username), 1,
                                                  native_bytes(password, sizeof(password) - 1), NULL));
  memset(username, 'x', sizeof(username) - 1);
  memset(password, 'x', sizeof(password) - 1);
  CHECK(rumqttc_redirect_response_set_reuse(response, same_mode(context, "auth-reuse"),
                                            same_mode(context, "network-reuse"), NULL));
  if (!isolated_mode(context))
    CHECK(rumqttc_redirect_response_set_session(response, RUMQTTC_REDIRECT_SESSION_REUSE,
                                                native_string(same_mode(context, "same") ? "origin" : "target"), NULL));
  return RUMQTTC_OK;
}
static void destroy(void *data) {
  authority_context *context = data;
  REQUIRE(atomic_fetch_add(&context->destroyed, 1) == 0);
}
static void store_destroy(void *data) {
  authority_context *context = data;
  REQUIRE(atomic_fetch_add(&context->store_destroyed, 1) == 0);
}
static void store_request(void *data, const rumqttc_store_request_t *request,
                          rumqttc_callback_completion_t *completion) {
  authority_context *context = data;
  int target = request->scope.len == 6 && memcmp(request->scope.data, "target", 6) == 0;
  REQUIRE(target || (request->scope.len == 6 && memcmp(request->scope.data, "origin", 6) == 0));
  if (target) {
    REQUIRE(same_mode(context, "changed") || same_mode(context, "store-failure") || same_mode(context, "store-cancel"));
    atomic_fetch_add(&context->target_store_calls, 1);
    if (request->operation == RUMQTTC_STORE_LOAD && same_mode(context, "store-failure")) {
      CHECK(rumqttc_callback_store_load_complete(completion, RUMQTTC_STORE_FAILED, native_bytes(NULL, 0)));
      return;
    }
    if (request->operation == RUMQTTC_STORE_LOAD && same_mode(context, "store-cancel")) {
      rumqttc_callback_completion_t *retained = NULL;
      CHECK(rumqttc_callback_completion_retain(completion, &retained));
      atomic_store(&context->pending, (uintptr_t)retained);
      return;
    }
  }
  if (request->operation == RUMQTTC_STORE_LOAD)
    CHECK(rumqttc_callback_store_load_complete(completion, RUMQTTC_STORE_NOT_FOUND, native_bytes(NULL, 0)));
  else
    CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
}
static rumqttc_redirect_request_t *run_case(const char *mode, const rumqttc_redirect_request_t *stale) {
  authority_context context = {0};
  context.mode = mode;
  context.stale = stale;
  rumqttc_redirect_registration_t *registration = NULL;
  rumqttc_store_registration_t *store = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_redirect_vtable_t table = RUMQTTC_REDIRECT_VTABLE_INIT;
  rumqttc_store_vtable_t store_table = RUMQTTC_STORE_VTABLE_INIT;
  table.decide = decide;
  table.destroy = destroy;
  store_table.load = store_request;
  store_table.save = store_request;
  store_table.clear = store_request;
  store_table.destroy = store_destroy;
  REQUIRE(rumqttc_redirect_registration_new(&table, &context, 0, 1000, &registration, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(registration == NULL && atomic_load(&context.destroyed) == 0);
  CHECK(rumqttc_redirect_registration_new(&table, &context, 2, same_mode(&context, "timeout") ? 1 : 5000, &registration,
                                          NULL));
  CHECK(rumqttc_store_registration_new(&store_table, &context, &store, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  char client_id[100];
  REQUIRE(snprintf(client_id, sizeof(client_id), "native-redirect-authority-%s", mode) > 0);
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  if (network_mode(&context)) {
    char url[128];
    const char *port = getenv("RUMQTTC_TEST_WS_PORT");
    REQUIRE(port != NULL);
    REQUIRE(snprintf(url, sizeof(url), "ws://127.0.0.1:%s/authority-origin", port) > 0);
    CHECK(rumqttc_config_set_transport_websocket(config, native_string(url), NULL));
    rumqttc_websocket_header_edit_t header = RUMQTTC_WEBSOCKET_HEADER_EDIT_INIT;
    header.operation = RUMQTTC_WEBSOCKET_HEADER_REPLACE;
    header.name = native_string("authorization");
    header.value = native_string("Bearer origin-private-header");
    CHECK(rumqttc_config_set_websocket_header_edits(config, &header, 1, NULL));
  }
  if (auth_mode(&context)) {
    rumqttc_auth_registration_t *auth = NULL;
    rumqttc_auth_vtable_t auth_table = RUMQTTC_AUTH_VTABLE_INIT;
    auth_table.respond = auth_respond;
    auth_table.failed = auth_failed;
    auth_table.destroy = auth_destroy;
    CHECK(rumqttc_auth_registration_new(&auth_table, &context, &auth, NULL));
    CHECK(rumqttc_config_set_v5_authenticator(config, auth, native_string("test"), 5000, NULL));
    rumqttc_auth_registration_destroy(auth);
  }
  CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_config_set_session_store(config, store, native_string("origin"), 5000, 1048576, NULL));
  /* One owner may be shared by configurations; successful fixed setters replace it. */
  rumqttc_config_t *other = NULL;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &other, NULL));
  CHECK(rumqttc_config_set_v5_redirect_authority(other, registration, NULL));
  CHECK(rumqttc_config_set_v5_redirect_policy(other, RUMQTTC_REDIRECT_REJECT, 0, RUMQTTC_REDIRECT_TRANSPORT_TCP, NULL,
                                              NULL));
  rumqttc_config_destroy(other);
  REQUIRE(atomic_load(&context.destroyed) == 0);
  CHECK(rumqttc_config_set_v5_redirect_authority(config, registration, NULL));
  REQUIRE(rumqttc_config_set_v5_redirect_authority(config, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_redirect_registration_destroy(registration);
  rumqttc_store_registration_destroy(store);
  rumqttc_config_destroy(config);
  REQUIRE(atomic_load(&context.destroyed) == 0);
  uint32_t expected = same_mode(&context, "reject")     ? RUMQTTC_REDIRECT_FAILURE_REJECTED
                      : same_mode(&context, "callback") ? RUMQTTC_REDIRECT_FAILURE_POLICY_CALLBACK
                      : same_mode(&context, "timeout")  ? RUMQTTC_REDIRECT_FAILURE_POLICY_TIMEOUT
                      : (same_mode(&context, "invalid") || same_mode(&context, "stale"))
                          ? RUMQTTC_REDIRECT_FAILURE_INVALID_RESPONSE
                          : 0;
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_REDIRECT);
  uint8_t present = 0;
  uint32_t failure = 0;
  CHECK(rumqttc_event_redirect(event, NULL, NULL, &present, &failure, NULL, NULL));
  REQUIRE(present == (expected != 0) && failure == expected);
  rumqttc_string_view_t selected = {NULL, 0};
  CHECK(rumqttc_event_redirect_selected_reference(event, &present, &selected, NULL));
  REQUIRE(present == (expected == 0));
  rumqttc_event_destroy(event);
  if (same_mode(&context, "store-failure")) {
    event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
    rumqttc_error_t *error = NULL;
    CHECK(rumqttc_event_disconnected(event, NULL, &error));
    CHECK(rumqttc_error_store_failure(error, &present, &failure));
    REQUIRE(present == 1 && failure == RUMQTTC_STORE_FAILURE_LOAD);
    CHECK(rumqttc_error_redirect_failure(error, &present, &failure));
    REQUIRE(present == 1 && failure == RUMQTTC_REDIRECT_FAILURE_TRANSPORT);
    rumqttc_error_destroy(error);
    rumqttc_event_destroy(event);
    CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  } else if (same_mode(&context, "store-cancel")) {
    for (unsigned waited = 0; atomic_load(&context.pending) == 0 && waited < NATIVE_DEADLINE_MS; ++waited)
      native_sleep_ms(1);
    rumqttc_callback_completion_t *pending = (rumqttc_callback_completion_t *)atomic_load(&context.pending);
    REQUIRE(pending != NULL);
    CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    REQUIRE(atomic_load(&context.store_destroyed) == 0);
    REQUIRE(rumqttc_callback_store_load_complete(pending, RUMQTTC_STORE_NOT_FOUND, native_bytes(NULL, 0)) ==
            RUMQTTC_INVALID_STATE);
    rumqttc_callback_completion_destroy(pending);
  } else if (!expected) {
    event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    rumqttc_event_destroy(event);
    native_close_destroy(client);
  } else {
    event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
    rumqttc_event_destroy(event);
    CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  }
  REQUIRE(atomic_load(&context.destroyed) == 1 && atomic_load(&context.store_destroyed) == 1);
  REQUIRE(atomic_load(&context.calls) == 1);
  if (auth_mode(&context)) {
    REQUIRE(atomic_load(&context.auth_starts) == (same_mode(&context, "auth-reuse") ? 2u : 1u));
    REQUIRE(atomic_load(&context.auth_successes) == (same_mode(&context, "auth-reuse") ? 1u : 0u));
    REQUIRE(atomic_load(&context.auth_destroyed) == 1);
  }
  REQUIRE(
      (atomic_load(&context.target_store_calls) > 0) ==
      (same_mode(&context, "changed") || same_mode(&context, "store-failure") || same_mode(&context, "store-cancel")));
  rumqttc_redirect_request_info_t info = RUMQTTC_REDIRECT_REQUEST_INFO_INIT;
  CHECK(rumqttc_redirect_request_info(context.request, &info, NULL));
  REQUIRE(info.reference_count == 2);
  return context.request;
}
int main(void) {
  rumqttc_redirect_request_t *stale = run_case("isolated", NULL);
  const char *modes[] = {"same",    "changed",  "reject",        "invalid",     "stale",
                         "timeout", "callback", "store-failure", "store-cancel"};
  for (size_t i = 0; i < sizeof(modes) / sizeof(modes[0]); ++i)
    rumqttc_redirect_request_destroy(run_case(modes[i], stale));
  rumqttc_redirect_request_destroy(run_case("auth-reuse", stale));
  rumqttc_redirect_request_destroy(run_case("auth-isolate", stale));
  if (rumqttc_library_capabilities() & RUMQTTC_CAP_WEBSOCKET) {
    rumqttc_redirect_request_destroy(run_case("network-reuse", stale));
    rumqttc_redirect_request_destroy(run_case("network-isolate", stale));
  }
  rumqttc_redirect_request_destroy(stale);
  return 0;
}
