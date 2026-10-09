#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

typedef struct resolver_context {
  uint32_t result;
  unsigned calls;
} resolver_context;

static atomic_uint destroyed;

static void resolve(void *user_data, const rumqttc_resolver_request_t *request,
                    rumqttc_callback_completion_t *completion) {
  resolver_context *context = (resolver_context *)user_data;
  REQUIRE(request->owner.len == strlen("_mqtt._tcp.service.invalid") ||
          request->owner.len == strlen("_mqtt._tcp.service.invalid."));
  REQUIRE(memcmp(request->owner.data, "_mqtt._tcp.service.invalid",
                 strlen("_mqtt._tcp.service.invalid")) == 0);
  ++context->calls;
  CHECK(rumqttc_callback_srv_complete(completion, context->result, NULL, 0));
  REQUIRE(rumqttc_callback_srv_complete(completion, context->result, NULL, 0) ==
          RUMQTTC_INVALID_STATE);
}

static void resolver_destroy(void *user_data) {
  free(user_data);
  atomic_fetch_add(&destroyed, 1);
}

static void run_case(const char *client_id, uint32_t response,
                     uint32_t expected_failure) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_resolver_registration_t *registration = NULL;
  rumqttc_resolver_vtable_t vtable = RUMQTTC_RESOLVER_VTABLE_INIT;
  resolver_context *context = calloc(1, sizeof(*context));
  unsigned saw_failure = 0;
  unsigned saw_terminal = 0;
  REQUIRE(context != NULL);
  context->result = response;
  vtable.resolve = resolve;
  vtable.destroy = resolver_destroy;
  CHECK(rumqttc_resolver_registration_new(&vtable, context, &registration,
                                           NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  CHECK(rumqttc_config_set_v5_redirect_policy(config, RUMQTTC_REDIRECT_FOLLOW,
                                               3, RUMQTTC_REDIRECT_TRANSPORT_TCP,
                                               NULL, NULL));
  if (response != UINT32_MAX)
    CHECK(rumqttc_config_set_v5_srv_resolver(config, registration, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  while (!saw_terminal) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS,
                                               &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    if (kind == RUMQTTC_EVENT_REDIRECT) {
      uint32_t source = 0;
      uint32_t reason = 0;
      uint8_t failure_present = 0;
      uint32_t failure = 0;
      uint8_t reference_present = 0;
      rumqttc_string_view_t reference = {NULL, 0};
      uint32_t decision = 0;
      uint64_t attempts = 0;
      CHECK(rumqttc_event_redirect(event, &source, &reason, &failure_present,
                                   &failure, &reference_present, &reference));
      CHECK(rumqttc_event_redirect_diagnostics(event, &decision, &attempts,
                                               NULL, NULL, NULL, NULL, NULL,
                                               NULL, NULL));
      REQUIRE(source == RUMQTTC_REDIRECT_SOURCE_CONNACK);
      REQUIRE(reason == RUMQTTC_REDIRECT_REASON_USE_ANOTHER_SERVER);
      REQUIRE(reference_present == 1);
      REQUIRE(reference.len == strlen("_mqtt._tcp.service.invalid"));
      REQUIRE(memcmp(reference.data, "_mqtt._tcp.service.invalid",
                     reference.len) == 0);
      REQUIRE(decision == RUMQTTC_REDIRECT_DECISION_FOLLOW);
      if (failure_present) {
        REQUIRE(failure == expected_failure);
        REQUIRE(attempts == 1);
        saw_failure = 1;
      }
    } else if (kind == RUMQTTC_EVENT_CONNECTED) {
      REQUIRE(0);
    } else if (kind == RUMQTTC_EVENT_DRIVER_TERMINATED) {
      rumqttc_error_t *error = NULL;
      uint8_t revision_present = 1;
      uint64_t revision = UINT64_MAX;
      REQUIRE(saw_failure == 1);
      CHECK(rumqttc_event_disconnected(event, NULL, &error));
      CHECK(rumqttc_error_configuration_revision(error, &revision_present, &revision));
      REQUIRE(revision_present == 0 && revision == 0);
      rumqttc_error_destroy(error);
      saw_terminal = 1;
    }
    rumqttc_event_destroy(event);
  }
  REQUIRE(context->calls == (response == UINT32_MAX ? 0u : 1u));
  native_close_destroy(client);
  rumqttc_config_destroy(config);
  rumqttc_resolver_registration_destroy(registration);
}

int main(void) {
  run_case("native-v5-srv-empty", RUMQTTC_SRV_SUCCESS,
           RUMQTTC_REDIRECT_FAILURE_DNS);
  run_case("native-v5-srv-failed", RUMQTTC_SRV_FAILED,
           RUMQTTC_REDIRECT_FAILURE_CALLBACK);
  unsigned expected_destroyed = 2;
  if (!(rumqttc_library_capabilities() & RUMQTTC_CAP_SYSTEM_SRV)) {
    run_case("native-v5-srv-missing", UINT32_MAX, RUMQTTC_REDIRECT_FAILURE_DNS);
    ++expected_destroyed;
  }
  REQUIRE(atomic_load(&destroyed) == expected_destroyed);
  return 0;
}
