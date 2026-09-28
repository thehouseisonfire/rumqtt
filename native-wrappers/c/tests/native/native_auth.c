#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

typedef struct auth_context {
  atomic_uintptr_t worker;
  atomic_uint starts;
  atomic_uint continues;
  atomic_uint successes;
  atomic_uint failures;
  atomic_uint callback_errors;
} auth_context;

static atomic_uint destroyed;

static int complete_challenge(void *argument) {
  rumqttc_callback_completion_t *completion =
      (rumqttc_callback_completion_t *)argument;
  rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
  const uint8_t bytes[] = {'r', 'e', 'p', 'l', 'y'};
  rumqttc_user_property_t properties[] = {
      {sizeof(rumqttc_user_property_t), {"p", 1}, {"1", 1}},
      {sizeof(rumqttc_user_property_t), {"p", 1}, {"2", 1}},
  };
  native_sleep_ms(20);
  response.action = RUMQTTC_AUTH_ACTION_SEND;
  response.method_present = 1;
  response.method = native_string("custom");
  response.data_present = 1;
  response.data = native_bytes(bytes, sizeof(bytes));
  response.user_properties = properties;
  response.user_property_count = 2;
  if (rumqttc_callback_auth_complete(completion, &response) != RUMQTTC_OK ||
      rumqttc_callback_auth_complete(completion, &response) !=
          RUMQTTC_INVALID_STATE) {
    rumqttc_callback_completion_destroy(completion);
    return 1;
  }
  rumqttc_callback_completion_destroy(completion);
  return 0;
}

static void auth_respond(void *user_data, const rumqttc_auth_request_t *request,
                         rumqttc_callback_completion_t *completion) {
  auth_context *context = (auth_context *)user_data;
  rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
  REQUIRE(request->exchange == RUMQTTC_AUTH_EXCHANGE_INITIAL ||
          request->exchange == RUMQTTC_AUTH_EXCHANGE_REAUTHENTICATION);
  REQUIRE(request->method.len == 6 &&
          memcmp(request->method.data, "custom", 6) == 0);
  if (request->stage == RUMQTTC_AUTH_REQUEST_START) {
    const uint8_t bytes[] = {'i', 'n', 'i', 't', 'i', 'a', 'l'};
    REQUIRE(request->reason_code_present == 0);
    atomic_fetch_add(&context->starts, 1);
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("custom");
    response.data_present = 1;
    response.data = native_bytes(bytes, sizeof(bytes));
    if (rumqttc_callback_auth_complete(completion, &response) != RUMQTTC_OK)
      atomic_store(&context->callback_errors, 1);
  } else if (request->stage == RUMQTTC_AUTH_REQUEST_CONTINUE) {
    rumqttc_callback_completion_t *retained = NULL;
    native_thread_t *worker;
    REQUIRE(request->reason_code_present == 1 && request->reason_code == 0x18);
    REQUIRE(request->data_present == 1 && request->data.len == 6 &&
            memcmp(request->data.data, "server", 6) == 0);
    REQUIRE(request->reason_string_present == 1 &&
            request->reason_string.len == 0);
    REQUIRE(request->user_property_count == 2);
    REQUIRE(request->user_properties[0].value.len == 1 &&
            request->user_properties[0].value.data[0] == '1');
    REQUIRE(request->user_properties[1].value.len == 1 &&
            request->user_properties[1].value.data[0] == '2');
    atomic_fetch_add(&context->continues, 1);
    CHECK(rumqttc_callback_completion_retain(completion, &retained));
    worker = native_thread_start(complete_challenge, retained);
    REQUIRE(worker != NULL);
    atomic_store(&context->worker, (uintptr_t)worker);
  } else if (request->stage == RUMQTTC_AUTH_REQUEST_SUCCESS) {
    REQUIRE(request->reason_code_present == 1 && request->reason_code == 0);
    REQUIRE(request->data_present == 1 && request->data.len == 12 &&
            memcmp(request->data.data, "server-proof", 12) == 0);
    atomic_fetch_add(&context->successes, 1);
    CHECK(rumqttc_callback_auth_complete(completion, &response));
  } else {
    atomic_store(&context->callback_errors, 1);
  }
}

static void auth_failed(void *user_data, const rumqttc_auth_request_t *request,
                        uint32_t failure) {
  auth_context *context = (auth_context *)user_data;
  (void)failure;
  REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_FAILED);
  atomic_fetch_add(&context->failures, 1);
}

static void auth_destroy(void *user_data) {
  auth_context *context = (auth_context *)user_data;
  free(context);
  atomic_fetch_add(&destroyed, 1);
}

int main(void) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_auth_registration_t *registration = NULL;
  rumqttc_auth_vtable_t vtable = RUMQTTC_AUTH_VTABLE_INIT;
  auth_context *context = calloc(1, sizeof(*context));
  native_thread_t *worker;
  rumqttc_event_t *event;
  unsigned saw_started = 0;
  unsigned saw_continue = 0;
  unsigned saw_success = 0;
  unsigned saw_connected = 0;
  REQUIRE(context != NULL);
  vtable.respond = auth_respond;
  vtable.failed = auth_failed;
  vtable.destroy = auth_destroy;
  CHECK(rumqttc_auth_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config,
                                     native_string("native-v5-auth-async"), NULL));
  CHECK(rumqttc_config_set_v5_authenticator(config, registration,
                                            native_string("custom"), 5000, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  for (;;) {
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS,
                                               &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    native_check_event_accessors(event);
    if (kind == RUMQTTC_EVENT_AUTHENTICATION) {
      uint32_t stage = 0;
      uint8_t reason_present = 0;
      uint8_t reason = 0;
      uint8_t properties_present = 0;
      uint8_t data_present = 0;
      rumqttc_bytes_view_t data = {NULL, 0};
      CHECK(rumqttc_event_authentication(event, NULL, &stage, NULL, NULL,
                                         NULL));
      CHECK(rumqttc_event_authentication_details(
          event, &reason_present, &reason, &properties_present, NULL, NULL,
          &data_present, &data, NULL, NULL));
      if (stage == RUMQTTC_AUTH_STAGE_STARTED) {
        REQUIRE(saw_started == 0 && saw_continue == 0 && saw_success == 0);
        ++saw_started;
      } else if (stage == RUMQTTC_AUTH_STAGE_CONTINUE) {
        REQUIRE(saw_started == 1 && saw_continue == 0 && saw_success == 0);
        size_t count = 0;
        REQUIRE(reason_present == 1 && reason == 0x18 &&
                properties_present == 1 && data_present == 1 && data.len == 6 &&
                memcmp(data.data, "server", 6) == 0);
        CHECK(rumqttc_event_user_property_count(
            event, RUMQTTC_EVENT_PROPERTIES_AUTHENTICATION, &count));
        REQUIRE(count == 2);
        saw_continue++;
      } else if (stage == RUMQTTC_AUTH_STAGE_SUCCEEDED) {
        REQUIRE(saw_started == 1 && saw_continue == 1 && saw_success == 0);
        REQUIRE(reason_present == 1 && reason == 0 &&
                properties_present == 1 && data_present == 1 && data.len == 12 &&
                memcmp(data.data, "server-proof", 12) == 0);
        saw_success++;
      } else {
        REQUIRE(0);
      }
    }
    if (kind == RUMQTTC_EVENT_CONNECTED) {
      REQUIRE(saw_started == 1 && saw_continue == 1 && !saw_connected);
      saw_connected = 1;
    }
    rumqttc_event_destroy(event);
    if (saw_connected && saw_success)
      break;
  }
  worker = (native_thread_t *)atomic_load(&context->worker);
  REQUIRE(worker != NULL && native_thread_join(worker) == 0);
  REQUIRE(atomic_load(&context->starts) == 1);
  REQUIRE(atomic_load(&context->continues) == 1);
  REQUIRE(atomic_load(&context->successes) == 1);
  REQUIRE(atomic_load(&context->failures) == 0);
  REQUIRE(atomic_load(&context->callback_errors) == 0);
  REQUIRE(saw_continue == 1 && saw_success == 1);
  for (unsigned index = 0; index < 2; ++index) {
    rumqttc_completion_t *completion = NULL;
    CHECK(rumqttc_client_reauthenticate_tracked(client, &completion, NULL));
    const uint32_t stages[] = {RUMQTTC_AUTH_STAGE_STARTED, RUMQTTC_AUTH_STAGE_CONTINUE, RUMQTTC_AUTH_STAGE_SUCCEEDED};
    for (unsigned position = 0; position < 3; ++position) {
      uint32_t exchange = 0, stage = 0;
      event = native_wait_event(client, RUMQTTC_EVENT_AUTHENTICATION);
      CHECK(rumqttc_event_authentication(event, &exchange, &stage, NULL, NULL, NULL));
      REQUIRE(exchange == RUMQTTC_AUTH_EXCHANGE_REAUTHENTICATION && stage == stages[position]);
      rumqttc_event_destroy(event);
    }
    native_wait_completion(completion, RUMQTTC_COMPLETION_AUTHENTICATED);
    rumqttc_completion_destroy(completion);
    worker = (native_thread_t *)atomic_exchange(&context->worker, 0);
    REQUIRE(worker != NULL && native_thread_join(worker) == 0);
  }
  REQUIRE(atomic_load(&context->starts) == 3);
  REQUIRE(atomic_load(&context->continues) == 3);
  REQUIRE(atomic_load(&context->successes) == 3);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
  rumqttc_auth_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == 1);
  return 0;
}
