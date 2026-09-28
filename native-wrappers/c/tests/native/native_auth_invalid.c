#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>

enum invalid_mode { SUCCESS_RESPONSE, CALLBACK_METHOD, BROKER_METHOD, DUPLICATE_METHOD, INVALID_PROPERTY };

typedef struct auth_context {
  enum invalid_mode mode;
  atomic_uint failures;
} auth_context;

static atomic_uint destroyed;

static void check_response_inputs(rumqttc_callback_completion_t *completion, rumqttc_auth_response_t valid) {
  rumqttc_auth_response_t invalid = valid;
  REQUIRE(rumqttc_callback_auth_complete(completion, NULL) == RUMQTTC_INVALID_ARGUMENT);
  invalid.action = UINT32_MAX;
  REQUIRE(rumqttc_callback_auth_complete(completion, &invalid) == RUMQTTC_INVALID_ARGUMENT);
  invalid = valid;
  invalid.reserved[0] = 1;
  REQUIRE(rumqttc_callback_auth_complete(completion, &invalid) == RUMQTTC_INVALID_ARGUMENT);
  invalid = valid;
  invalid.method_present = 2;
  REQUIRE(rumqttc_callback_auth_complete(completion, &invalid) == RUMQTTC_INVALID_ARGUMENT);
  invalid = valid;
  invalid.data_present = 1;
  invalid.data = native_bytes(NULL, 1);
  REQUIRE(rumqttc_callback_auth_complete(completion, &invalid) == RUMQTTC_INVALID_ARGUMENT);
  invalid = valid;
  invalid.user_property_count = 1;
  REQUIRE(rumqttc_callback_auth_complete(completion, &invalid) == RUMQTTC_INVALID_ARGUMENT);
  invalid = valid;
  invalid.method.len = SIZE_MAX;
  REQUIRE(rumqttc_callback_auth_complete(completion, &invalid) == RUMQTTC_INVALID_ARGUMENT);
}

static void respond(void *user_data, const rumqttc_auth_request_t *request, rumqttc_callback_completion_t *completion) {
  auth_context *context = user_data;
  rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
  if (request->stage == RUMQTTC_AUTH_REQUEST_START) {
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("custom");
    check_response_inputs(completion, response);
  } else if (request->stage == RUMQTTC_AUTH_REQUEST_CONTINUE) {
    REQUIRE(context->mode == CALLBACK_METHOD);
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("changed");
  } else {
    REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_SUCCESS);
    if (request->exchange == RUMQTTC_AUTH_EXCHANGE_REAUTHENTICATION) {
      REQUIRE(context->mode == SUCCESS_RESPONSE);
      response.action = RUMQTTC_AUTH_ACTION_SEND;
      response.method_present = 1;
      response.method = native_string("custom");
    }
  }
  CHECK(rumqttc_callback_auth_complete(completion, &response));
  REQUIRE(rumqttc_callback_auth_complete(completion, &response) == RUMQTTC_INVALID_STATE);
}

static void failed(void *user_data, const rumqttc_auth_request_t *request, uint32_t failure) {
  auth_context *context = user_data;
  REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_FAILED);
  REQUIRE(failure != 0);
  atomic_fetch_add(&context->failures, 1);
}

static void destroy_auth(void *user_data) {
  free(user_data);
  atomic_fetch_add(&destroyed, 1);
}

static void run_case(enum invalid_mode mode, const char *client_id) {
  rumqttc_auth_vtable_t vtable = RUMQTTC_AUTH_VTABLE_INIT;
  auth_context *context = calloc(1, sizeof(*context));
  rumqttc_auth_registration_t *registration = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_event_t *event;
  unsigned saw_failure = 0;
  uint32_t expected_failure =
      mode == CALLBACK_METHOD ? RUMQTTC_AUTH_FAILURE_METHOD : RUMQTTC_AUTH_FAILURE_INVALID_RESPONSE;
  REQUIRE(context != NULL);
  context->mode = mode;
  vtable.respond = respond;
  vtable.failed = failed;
  vtable.destroy = destroy_auth;
  CHECK(rumqttc_auth_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  CHECK(rumqttc_config_set_v5_authenticator(config, registration, native_string("custom"), 5000, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(event);
  CHECK(rumqttc_client_reauthenticate_tracked(client, &completion, NULL));
  for (unsigned attempt = 0; attempt < 12; ++attempt) {
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    native_check_event_accessors(event);
    if (kind == RUMQTTC_EVENT_DISCONNECTED || kind == RUMQTTC_EVENT_DRIVER_TERMINATED) {
      rumqttc_error_kind_t error_kind = 0;
      CHECK(rumqttc_event_disconnected(event, NULL, &error));
      CHECK(rumqttc_error_kind(error, &error_kind));
      REQUIRE(error_kind == (mode <= CALLBACK_METHOD ? RUMQTTC_ERROR_AUTHENTICATION : RUMQTTC_ERROR_PROTOCOL));
      if (mode <= CALLBACK_METHOD) {
        uint8_t present = 0;
        uint32_t failure = 0;
        CHECK(rumqttc_error_auth_failure(error, &present, &failure));
        REQUIRE(present == 1 && failure == expected_failure);
      }
      rumqttc_error_destroy(error);
      rumqttc_event_destroy(event);
      saw_failure = 1;
      break;
    }
    REQUIRE(kind != RUMQTTC_EVENT_CONNECTED);
    rumqttc_event_destroy(event);
  }
  REQUIRE(saw_failure);
  REQUIRE(rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, &error) != RUMQTTC_OK);
  uint64_t operation_id = 0, error_id = 0;
  uint8_t present = 0;
  CHECK(rumqttc_completion_operation_id(completion, &operation_id));
  CHECK(rumqttc_error_operation_id(error, &present, &error_id));
  REQUIRE(present == 1 && operation_id != 0 && error_id == operation_id);
  if (mode <= CALLBACK_METHOD) {
    uint32_t failure = 0;
    CHECK(rumqttc_error_auth_failure(error, &present, &failure));
    REQUIRE(present == 1 && failure == expected_failure);
  }
  rumqttc_error_destroy(error);
  rumqttc_completion_destroy(completion);
  native_close_destroy(client);
  REQUIRE(atomic_load(&context->failures) == 1);
  rumqttc_config_destroy(config);
  rumqttc_auth_registration_destroy(registration);
}

int main(void) {
  run_case(SUCCESS_RESPONSE, "native-v5-auth-invalid-success");
  run_case(CALLBACK_METHOD, "native-v5-auth-invalid-callback-method");
  run_case(BROKER_METHOD, "native-v5-auth-invalid-broker-method");
  run_case(DUPLICATE_METHOD, "native-v5-auth-invalid-duplicate");
  run_case(INVALID_PROPERTY, "native-v5-auth-invalid-property");
  REQUIRE(atomic_load(&destroyed) == 5);
  return 0;
}
