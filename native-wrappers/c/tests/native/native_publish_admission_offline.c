#include "native_common.h"
#include <stdatomic.h>

/* Hold connection establishment at a host callback, without sockets or sleeps
 * governing MQTT ordering. This also verifies the public C API on minimal builds. */
typedef struct held_connector {
  atomic_uintptr_t token;
  atomic_uint cancelled;
  atomic_uint destroyed;
} held_connector;
static void connect_held(void *data, const rumqttc_transport_connect_request_t *request,
                         rumqttc_callback_completion_t *completion) {
  held_connector *host = data;
  rumqttc_callback_completion_t *token = NULL;
  REQUIRE(request->protocol == RUMQTTC_PROTOCOL_V5);
  CHECK(rumqttc_callback_completion_retain(completion, &token));
  REQUIRE(atomic_exchange(&host->token, (uintptr_t)token) == 0);
}
static void cancel_held(void *data, uint64_t id) {
  REQUIRE(id != 0);
  atomic_fetch_add(&((held_connector *)data)->cancelled, 1);
}
static void destroy_held(void *data) {
  atomic_fetch_add(&((held_connector *)data)->destroyed, 1);
}
static void reason(rumqttc_error_t *error, uint32_t wanted, uint8_t retryable) {
  uint8_t present = 0, retry = 0, ambiguous = 1;
  uint32_t failure = 0, delivery = 0;
  CHECK(rumqttc_error_publish_failure(error, &present, &failure));
  REQUIRE(present && failure == wanted);
  CHECK(rumqttc_error_flags(error, &retry, &ambiguous));
  REQUIRE(retry == retryable && !ambiguous);
  CHECK(rumqttc_error_context(error, NULL, NULL, NULL, NULL, &delivery));
  REQUIRE(delivery == 1);
  rumqttc_error_destroy(error);
}
static void attempt(rumqttc_client_t *client, rumqttc_qos_t qos, const char *payload,
                    size_t size, uint32_t status, uint32_t failure, uint8_t retryable) {
  rumqttc_publish_options_t options = native_publish_options(qos);
  rumqttc_error_t *error = NULL;
  uint64_t operation = 123;
  REQUIRE(rumqttc_client_try_publish(client, native_string("a"), native_bytes((const uint8_t *)payload, size),
                                   &options, &operation, &error) == status);
  REQUIRE(operation == 0);
  reason(error, failure, retryable);
}
static void scenario(unsigned mode) {
  held_connector host = {0};
  rumqttc_transport_vtable_t table = RUMQTTC_TRANSPORT_VTABLE_INIT;
  rumqttc_transport_registration_t *registration = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_publish_options_t options = native_publish_options(mode == 0 ? RUMQTTC_QOS_0 : RUMQTTC_QOS_2);
  size_t count = 99, bytes = 99, count_limit = 0, byte_limit = 0;
  table.connect = connect_held; table.cancel = cancel_held; table.destroy = destroy_held;
  CHECK(rumqttc_transport_registration_new(&table, &host, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("memory"), 1883, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("offline-admission"), NULL));
  CHECK(rumqttc_config_set_transport_connector(config, registration, NULL));
  rumqttc_transport_registration_destroy(registration);
  CHECK(rumqttc_config_set_request_capacity(config, mode == 0 ? 1 : 2, NULL));
  CHECK(rumqttc_config_set_v5_publish_budget(config, mode == 1 ? 1 : 2, mode == 0 ? 10 : 5, NULL));
  if (mode != 0)
    CHECK(rumqttc_config_set_v5_publish_admission_policy(config, RUMQTTC_PUBLISH_ADMISSION_EVENT_LOOP_VALIDATED, NULL));
  REQUIRE(rumqttc_config_set_v5_publish_admission_policy(config, UINT32_MAX, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_set_v5_publish_budget(config, 0, 5, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_set_v5_publish_budget(config, 2, 0, NULL) == RUMQTTC_INVALID_ARGUMENT);
  CHECK(rumqttc_client_start(config, &client, NULL));
  {
    uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
    while (!atomic_load(&host.token)) {
      REQUIRE(native_monotonic_ms() < deadline);
      native_sleep_ms(1);
    }
  }
  if (mode == 0) {
    attempt(client, RUMQTTC_QOS_1, "data", 4, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_CAPABILITIES_PENDING, 1);
    attempt(client, RUMQTTC_QOS_2, "data", 4, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_CAPABILITIES_PENDING, 1);
  }
  {
    rumqttc_v5_publish_properties_t properties = RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
    rumqttc_publish_options_t alias = native_publish_options(RUMQTTC_QOS_0);
    rumqttc_error_t *error = NULL;
    uint64_t operation = 0;
    properties.topic_alias = 1;
    alias.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
    alias.v5_properties = &properties;
    if (mode == 0) {
      REQUIRE(rumqttc_client_try_publish(client, native_string("a"), native_bytes((const uint8_t *)"data", 4),
                                       &alias, &operation, &error) == RUMQTTC_BACKPRESSURE);
      reason(error, RUMQTTC_PUBLISH_FAILURE_CAPABILITIES_PENDING, 1);
    } else {
      REQUIRE(rumqttc_client_try_publish(client, native_string(""), native_bytes(NULL, 0),
                                       &alias, &operation, &error) == RUMQTTC_INVALID_ARGUMENT);
      reason(error, RUMQTTC_PUBLISH_FAILURE_TOO_LARGE, 0);
    }
  }
  CHECK(rumqttc_client_publish_tracked(client, native_string("a"), native_bytes((const uint8_t *)"data", 4),
                                     &options, &completion, NULL));
  // Editing this reusable config affects neither a live client nor its limits.
  CHECK(rumqttc_config_set_v5_publish_admission_policy(config, mode == 0 ? 1 : 0, NULL));
  CHECK(rumqttc_config_reset_v5_publish_budget(config, NULL));
  if (mode == 0)
    attempt(client, RUMQTTC_QOS_0, "data", 4, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_REQUEST_CHANNEL_FULL, 1);
  else if (mode == 1)
    attempt(client, RUMQTTC_QOS_2, "data", 4, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_COUNT_EXHAUSTED, 1);
  else
    attempt(client, RUMQTTC_QOS_0, "", 0, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_BYTES_EXHAUSTED, 1);
  if (mode != 0)
    attempt(client, RUMQTTC_QOS_2, "data!", 5, RUMQTTC_INVALID_ARGUMENT, RUMQTTC_PUBLISH_FAILURE_TOO_LARGE, 0);
  rumqttc_completion_destroy(completion);
  CHECK(rumqttc_client_v5_publish_budget_snapshot(client, &count, &bytes, &count_limit, &byte_limit, NULL));
  REQUIRE(count == 1 && bytes == 5 && count_limit == (mode == 1 ? 1u : 2u) && byte_limit == (mode == 0 ? 10u : 5u));
  CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_client_v5_publish_budget_snapshot(client, &count, &bytes, NULL, NULL, NULL));
  REQUIRE(count == 0 && bytes == 0);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  rumqttc_config_destroy(config);
  REQUIRE(atomic_load(&host.cancelled) == 1 && atomic_load(&host.destroyed) == 0);
  rumqttc_callback_completion_destroy((rumqttc_callback_completion_t *)atomic_load(&host.token));
  REQUIRE(atomic_load(&host.destroyed) == 1);
}
int main(void) {
  rumqttc_config_t *v4 = NULL;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &v4, NULL));
  REQUIRE(rumqttc_config_set_v5_publish_admission_policy(v4, 1, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_set_v5_publish_budget(v4, 1, 5, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_reset_v5_publish_budget(v4, NULL) == RUMQTTC_INVALID_ARGUMENT);
  rumqttc_config_destroy(v4);
  for (unsigned mode = 0; mode < 3; ++mode)
    scenario(mode);
  return 0;
}
