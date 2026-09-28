#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>

typedef struct auth_context {
  atomic_uint starts;
  atomic_uint failures;
} auth_context;

static atomic_uint destroyed;

static void respond(void *user_data, const rumqttc_auth_request_t *request, rumqttc_callback_completion_t *completion) {
  auth_context *context = (auth_context *)user_data;
  rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
  REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_START || request->stage == RUMQTTC_AUTH_REQUEST_SUCCESS);
  if (request->stage == RUMQTTC_AUTH_REQUEST_START) {
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("custom");
    atomic_fetch_add(&context->starts, 1);
  }
  CHECK(rumqttc_callback_auth_complete(completion, &response));
}

static void failed(void *user_data, const rumqttc_auth_request_t *request, uint32_t failure) {
  auth_context *context = (auth_context *)user_data;
  REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_FAILED);
  REQUIRE(failure == RUMQTTC_AUTH_FAILURE_CONNECTION_CLOSED);
  atomic_fetch_add(&context->failures, 1);
}

static void destroy_auth(void *user_data) {
  free(user_data);
  atomic_fetch_add(&destroyed, 1);
}

int main(void) {
  rumqttc_auth_vtable_t vtable = RUMQTTC_AUTH_VTABLE_INIT;
  auth_context *context = calloc(1, sizeof(*context));
  rumqttc_auth_registration_t *registration = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *first = NULL, *second = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_event_t *event;
  uint64_t first_id = 0, second_id = 0, error_id = 0;
  uint8_t failure_present = 0;
  uint32_t failure = 0;
  REQUIRE(context != NULL);
  vtable.respond = respond;
  vtable.failed = failed;
  vtable.destroy = destroy_auth;
  CHECK(rumqttc_auth_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-v5-auth-overlap"), NULL));
  CHECK(rumqttc_config_set_v5_authenticator(config, registration, native_string("custom"), 5000, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(event);
  CHECK(rumqttc_client_reauthenticate_tracked(client, &first, NULL));
  for (;;) {
    uint32_t exchange = 0, stage = 0;
    event = native_wait_event(client, RUMQTTC_EVENT_AUTHENTICATION);
    CHECK(rumqttc_event_authentication(event, &exchange, &stage, NULL, NULL, NULL));
    rumqttc_event_destroy(event);
    if (exchange == RUMQTTC_AUTH_EXCHANGE_REAUTHENTICATION && stage == RUMQTTC_AUTH_STAGE_STARTED)
      break;
  }
  CHECK(rumqttc_client_reauthenticate_tracked(client, &second, NULL));
  CHECK(rumqttc_completion_operation_id(first, &first_id));
  CHECK(rumqttc_completion_operation_id(second, &second_id));
  REQUIRE(first_id != 0 && second_id > first_id);
  {
    rumqttc_status_t status = rumqttc_completion_wait_timeout_ms(second, NATIVE_DEADLINE_MS, &error);
    if (status != RUMQTTC_AMBIGUOUS)
      native_fail(__FILE__, __LINE__, "overlapping completion status", status);
  }
  REQUIRE(error != NULL);
  CHECK(rumqttc_error_auth_failure(error, &failure_present, &failure));
  REQUIRE(failure_present == 1 && failure == RUMQTTC_AUTH_FAILURE_OVERLAPPING);
  CHECK(rumqttc_error_operation_id(error, &failure_present, &error_id));
  REQUIRE(failure_present == 1 && error_id == second_id);
  rumqttc_error_destroy(error);
  error = NULL;
  rumqttc_completion_destroy(second);
  REQUIRE(rumqttc_completion_wait_timeout_ms(first, NATIVE_DEADLINE_MS, &error) != RUMQTTC_OK);
  REQUIRE(error != NULL);
  CHECK(rumqttc_error_auth_failure(error, &failure_present, &failure));
  REQUIRE(failure_present == 1 && failure == RUMQTTC_AUTH_FAILURE_CONNECTION_CLOSED);
  CHECK(rumqttc_error_operation_id(error, &failure_present, &error_id));
  REQUIRE(failure_present == 1 && error_id == first_id);
  rumqttc_error_destroy(error);
  rumqttc_completion_destroy(first);
  unsigned interrupted = 0;
  for (unsigned i = 0; i < 8 && !interrupted; ++i) {
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    native_check_event_accessors(event);
    if (kind == RUMQTTC_EVENT_AUTHENTICATION) {
      uint32_t exchange = 0, stage = 0;
      CHECK(rumqttc_event_authentication(event, &exchange, &stage, NULL, NULL, NULL));
      REQUIRE(exchange != RUMQTTC_AUTH_EXCHANGE_REAUTHENTICATION || stage != RUMQTTC_AUTH_STAGE_SUCCEEDED);
    }
    interrupted = kind == RUMQTTC_EVENT_DISCONNECTED;
    rumqttc_event_destroy(event);
  }
  REQUIRE(interrupted);
  native_close_destroy(client);
  /* Reconnect may begin before the host observes the failed notice. */
  REQUIRE(atomic_load(&context->starts) >= 2);
  REQUIRE(atomic_load(&context->failures) >= 1);
  REQUIRE(atomic_load(&context->failures) <= atomic_load(&context->starts));
  rumqttc_config_destroy(config);
  rumqttc_auth_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == 1);
  return 0;
}
