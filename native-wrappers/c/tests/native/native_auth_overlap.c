#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>

typedef struct auth_context {
  atomic_uint starts;
  atomic_uint continues;
  atomic_uint successes;
  atomic_uint failures;
} auth_context;

static atomic_uint destroyed;

static void respond(void *user_data, const rumqttc_auth_request_t *request, rumqttc_callback_completion_t *completion) {
  auth_context *context = (auth_context *)user_data;
  rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
  if (request->stage == RUMQTTC_AUTH_REQUEST_START) {
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("custom");
    atomic_fetch_add(&context->starts, 1);
  } else if (request->stage == RUMQTTC_AUTH_REQUEST_CONTINUE) {
    REQUIRE(request->exchange == RUMQTTC_AUTH_EXCHANGE_REAUTHENTICATION);
    REQUIRE(request->data_present && request->data.len == 4 && memcmp(request->data.data, "hold", 4) == 0);
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("custom");
    atomic_fetch_add(&context->continues, 1);
  } else {
    REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_SUCCESS);
    REQUIRE(request->exchange == RUMQTTC_AUTH_EXCHANGE_INITIAL);
    atomic_fetch_add(&context->successes, 1);
  }
  CHECK(rumqttc_callback_auth_complete(completion, &response));
}

static void failed(void *user_data, const rumqttc_auth_request_t *request, uint32_t failure) {
  auth_context *context = (auth_context *)user_data;
  REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_FAILED);
  REQUIRE(request->exchange == RUMQTTC_AUTH_EXCHANGE_REAUTHENTICATION);
  REQUIRE(failure == RUMQTTC_AUTH_FAILURE_CONNECTION_CLOSED);
  atomic_fetch_add(&context->failures, 1);
}

static void destroy_auth(void *user_data) {
  free(user_data);
  atomic_fetch_add(&destroyed, 1);
}

static void authentication(rumqttc_client_t *client, uint32_t exchange, uint32_t stage) {
  rumqttc_event_t *event = NULL;
  uint32_t kind = 0, actual_exchange = 0, actual_stage = 0, failure = 0;
  uint8_t failure_present = 0;
  CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
  CHECK(rumqttc_event_kind(event, &kind));
  if (kind != RUMQTTC_EVENT_AUTHENTICATION)
    fprintf(stderr, "auth expected exchange=%u stage=%u; received event kind=%u\n", exchange, stage, kind);
  REQUIRE(kind == RUMQTTC_EVENT_AUTHENTICATION);
  native_check_event_accessors(event);
  CHECK(rumqttc_event_authentication(event, &actual_exchange, &actual_stage, &failure_present, &failure, NULL));
  REQUIRE(actual_exchange == exchange && actual_stage == stage);
  REQUIRE(failure_present == (stage == RUMQTTC_AUTH_STAGE_FAILED));
  if (failure_present)
    REQUIRE(failure == RUMQTTC_AUTH_FAILURE_CONNECTION_CLOSED);
  rumqttc_event_destroy(event);
}

static void connected(rumqttc_client_t *client) {
  rumqttc_event_t *event = NULL;
  uint32_t kind = 0;
  CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
  CHECK(rumqttc_event_kind(event, &kind));
  REQUIRE(kind == RUMQTTC_EVENT_CONNECTED);
  native_check_event_accessors(event);
  rumqttc_event_destroy(event);
}

static void completion_failure(rumqttc_error_t *error, uint64_t id, uint32_t expected) {
  uint8_t present = 0;
  uint32_t failure = 0;
  uint64_t error_id = 0;
  REQUIRE(error != NULL);
  CHECK(rumqttc_error_auth_failure(error, &present, &failure));
  REQUIRE(present && failure == expected);
  CHECK(rumqttc_error_operation_id(error, &present, &error_id));
  REQUIRE(present && error_id == id);
  rumqttc_error_destroy(error);
}

int main(void) {
  rumqttc_auth_vtable_t vtable = RUMQTTC_AUTH_VTABLE_INIT;
  auth_context *context = calloc(1, sizeof(*context));
  rumqttc_auth_registration_t *registration = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *first = NULL, *second = NULL;
  uint64_t first_id = 0, second_id = 0;
  REQUIRE(context != NULL);
  vtable.respond = respond;
  vtable.failed = failed;
  vtable.destroy = destroy_auth;
  CHECK(rumqttc_auth_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-v5-auth-overlap"), NULL));
  CHECK(rumqttc_config_set_v5_authenticator(config, registration, native_string("custom"), 10000, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  authentication(client, RUMQTTC_AUTH_EXCHANGE_INITIAL, RUMQTTC_AUTH_STAGE_STARTED);
  connected(client);
  authentication(client, RUMQTTC_AUTH_EXCHANGE_INITIAL, RUMQTTC_AUTH_STAGE_SUCCEEDED);
  CHECK(rumqttc_client_reauthenticate_tracked(client, &first, NULL));
  authentication(client, RUMQTTC_AUTH_EXCHANGE_REAUTHENTICATION, RUMQTTC_AUTH_STAGE_STARTED);
  authentication(client, RUMQTTC_AUTH_EXCHANGE_REAUTHENTICATION, RUMQTTC_AUTH_STAGE_CONTINUE);
  REQUIRE(native_fixture_read("auth-held") == 1);
  CHECK(rumqttc_client_reauthenticate_tracked(client, &second, NULL));
  CHECK(rumqttc_completion_operation_id(first, &first_id));
  CHECK(rumqttc_completion_operation_id(second, &second_id));
  REQUIRE(first_id != 0 && second_id > first_id);
  uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
  for (;;) {
    rumqttc_error_t *error = NULL;
    rumqttc_status_t status = rumqttc_completion_poll(second, &error);
    /* The broker is parked. This intermediate state proves resolution order,
       independent of sequential waiter order or thread scheduling. */
    REQUIRE(rumqttc_completion_poll(first, NULL) == RUMQTTC_WOULD_BLOCK);
    if (status != RUMQTTC_WOULD_BLOCK) {
      REQUIRE(status == RUMQTTC_AUTHENTICATION_ERROR);
      completion_failure(error, second_id, RUMQTTC_AUTH_FAILURE_OVERLAPPING);
      break;
    }
    rumqttc_error_destroy(error);
    REQUIRE(native_monotonic_ms() < deadline);
    native_sleep_ms(1);
  }
  rumqttc_event_t *event = NULL;
  REQUIRE(rumqttc_client_event_recv_timeout_ms(client, 0, &event, NULL) == RUMQTTC_TIMEOUT);
  REQUIRE(event == NULL);
  native_fixture_write("auth-abort", 1);
  authentication(client, RUMQTTC_AUTH_EXCHANGE_REAUTHENTICATION, RUMQTTC_AUTH_STAGE_FAILED);
  uint32_t kind = 0;
  CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
  CHECK(rumqttc_event_kind(event, &kind));
  REQUIRE(kind == RUMQTTC_EVENT_DISCONNECTED);
  native_check_event_accessors(event);
  rumqttc_event_destroy(event);
  REQUIRE(native_fixture_read("auth-reconnected") == 1);
  rumqttc_error_t *error = NULL;
  REQUIRE(rumqttc_completion_wait_timeout_ms(first, NATIVE_DEADLINE_MS, &error) == RUMQTTC_AMBIGUOUS);
  completion_failure(error, first_id, RUMQTTC_AUTH_FAILURE_CONNECTION_CLOSED);
  error = NULL;
  REQUIRE(rumqttc_completion_poll(second, &error) == RUMQTTC_AUTHENTICATION_ERROR);
  completion_failure(error, second_id, RUMQTTC_AUTH_FAILURE_OVERLAPPING);
  REQUIRE(rumqttc_client_event_recv_timeout_ms(client, 0, &event, NULL) == RUMQTTC_TIMEOUT);
  native_fixture_write("auth-reconnect-release", 1);
  authentication(client, RUMQTTC_AUTH_EXCHANGE_INITIAL, RUMQTTC_AUTH_STAGE_STARTED);
  connected(client);
  authentication(client, RUMQTTC_AUTH_EXCHANGE_INITIAL, RUMQTTC_AUTH_STAGE_SUCCEEDED);
  REQUIRE(rumqttc_client_event_recv_timeout_ms(client, 50, &event, NULL) == RUMQTTC_TIMEOUT);
  native_close_destroy(client);
  REQUIRE(atomic_load(&context->starts) == 3 && atomic_load(&context->continues) == 1);
  REQUIRE(atomic_load(&context->successes) == 2 && atomic_load(&context->failures) == 1);
  rumqttc_completion_destroy(first);
  rumqttc_completion_destroy(second);
  rumqttc_config_destroy(config);
  rumqttc_auth_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == 1);
  return 0;
}
