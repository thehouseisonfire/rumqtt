#include "native_common.h"
#include <stdio.h>

static rumqttc_config_t *configuration(const char *id) {
  rumqttc_config_t *config = NULL;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
  return config;
}

static void failure(rumqttc_error_t *error, uint32_t wanted, uint32_t delivery, uint8_t retryable) {
  uint8_t present = 0, retry = 0, ambiguous = 0, broker = 1;
  uint32_t reason = 0, status = 0;
  CHECK(rumqttc_error_publish_failure(error, &present, &reason));
  REQUIRE(present && reason == wanted);
  CHECK(rumqttc_error_context(error, NULL, NULL, NULL, NULL, &status));
  REQUIRE(status == delivery);
  CHECK(rumqttc_error_flags(error, &retry, &ambiguous));
  REQUIRE(retry == retryable && ambiguous == (delivery == 3));
  CHECK(rumqttc_error_broker_reason(error, &broker, NULL));
  REQUIRE(!broker);
  rumqttc_error_destroy(error);
}

static rumqttc_completion_t *publish(rumqttc_client_t *client, rumqttc_qos_t qos, uint8_t retain) {
  rumqttc_publish_options_t options = native_publish_options(qos);
  rumqttc_completion_t *completion = NULL;
  options.retain = retain;
  CHECK(rumqttc_client_publish_tracked(client, native_string("a"), native_bytes((const uint8_t *)"data", 4),
                                     &options, &completion, NULL));
  return completion;
}

static void rejected(rumqttc_client_t *client, rumqttc_qos_t qos, uint8_t retain,
                     uint32_t status, uint32_t reason, uint8_t retryable) {
  rumqttc_publish_options_t options = native_publish_options(qos);
  rumqttc_error_t *error = NULL;
  uint64_t operation = 99;
  options.retain = retain;
  REQUIRE(rumqttc_client_try_publish(client, native_string("a"), native_bytes((const uint8_t *)"data", 4),
                                   &options, &operation, &error) == status);
  REQUIRE(operation == 0);
  failure(error, reason, 1, retryable);
}

static void snapshot(rumqttc_client_t *client, size_t count, size_t bytes, size_t limit, size_t byte_limit) {
  size_t actual = 99, data = 99, max_count = 0, max_bytes = 0;
  CHECK(rumqttc_client_v5_publish_budget_snapshot(client, &actual, &data, &max_count, &max_bytes, NULL));
  REQUIRE(actual == count && data == bytes && max_count == limit && max_bytes == byte_limit);
}

static void policies(void) {
  rumqttc_config_t *config = configuration("native-admission-strict");
  rumqttc_config_t *v4 = NULL;
  rumqttc_client_t *strict = NULL, *deferred = NULL;
  rumqttc_completion_t *qos0, *qos2, *retained;
  rumqttc_error_t *error = NULL;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &v4, NULL));
  REQUIRE(rumqttc_config_set_v5_publish_admission_policy(v4, 1, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_set_v5_publish_budget(v4, 1, 5, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_reset_v5_publish_budget(v4, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_set_v5_publish_admission_policy(config, UINT32_MAX, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_set_v5_publish_budget(config, 0, 10, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_set_v5_publish_budget(config, 3, 0, NULL) == RUMQTTC_INVALID_ARGUMENT);
  CHECK(rumqttc_config_set_request_capacity(config, 1, NULL));
  CHECK(rumqttc_config_set_v5_publish_budget(config, 3, 10, NULL));
  CHECK(rumqttc_client_start(config, &strict, NULL));
  native_fixture_read("native-admission-strict-ready");
  rejected(strict, RUMQTTC_QOS_1, 0, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_CAPABILITIES_PENDING, 1);
  rejected(strict, RUMQTTC_QOS_2, 0, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_CAPABILITIES_PENDING, 1);
  rejected(strict, RUMQTTC_QOS_0, 1, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_CAPABILITIES_PENDING, 1);
  qos0 = publish(strict, RUMQTTC_QOS_0, 0);
  rejected(strict, RUMQTTC_QOS_0, 0, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_REQUEST_CHANNEL_FULL, 1);
  snapshot(strict, 1, 5, 3, 10);
  // Reusing this config cannot change an existing client's policy or limits.
  CHECK(rumqttc_config_set_v5_publish_admission_policy(config, RUMQTTC_PUBLISH_ADMISSION_EVENT_LOOP_VALIDATED, NULL));
  CHECK(rumqttc_config_set_v5_publish_budget(config, 2, 10, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-admission-deferred"), NULL));
  CHECK(rumqttc_config_set_request_capacity(config, 2, NULL));
  CHECK(rumqttc_client_start(config, &deferred, NULL));
  native_fixture_read("native-admission-deferred-ready");
  qos2 = publish(deferred, RUMQTTC_QOS_2, 0);
  retained = publish(deferred, RUMQTTC_QOS_0, 1);
  rejected(deferred, RUMQTTC_QOS_1, 0, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_COUNT_EXHAUSTED, 1);
  snapshot(deferred, 2, 10, 2, 10);
  rejected(strict, RUMQTTC_QOS_1, 0, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_CAPABILITIES_PENDING, 1);
  native_fixture_write("native-admission-strict-release", 1);
  native_fixture_write("native-admission-deferred-release", 1);
  rumqttc_event_destroy(native_wait_event(strict, RUMQTTC_EVENT_CONNECTED));
  rumqttc_event_destroy(native_wait_event(deferred, RUMQTTC_EVENT_CONNECTED));
  native_wait_completion(qos0, RUMQTTC_COMPLETION_QOS0_FLUSHED);
  rumqttc_completion_destroy(qos0);
  rejected(strict, RUMQTTC_QOS_1, 0, RUMQTTC_LOCAL_REJECTED, RUMQTTC_PUBLISH_FAILURE_MAXIMUM_QOS, 0);
  REQUIRE(rumqttc_completion_wait_timeout_ms(qos2, NATIVE_DEADLINE_MS, &error) == RUMQTTC_LOCAL_REJECTED);
  failure(error, RUMQTTC_PUBLISH_FAILURE_MAXIMUM_QOS, 2, 0);
  error = NULL;
  REQUIRE(rumqttc_completion_wait_timeout_ms(retained, NATIVE_DEADLINE_MS, &error) == RUMQTTC_LOCAL_REJECTED);
  failure(error, RUMQTTC_PUBLISH_FAILURE_RETAIN_UNAVAILABLE, 2, 0);
  rumqttc_completion_destroy(qos2);
  rumqttc_completion_destroy(retained);
  snapshot(deferred, 0, 0, 2, 10);
  // Deferral continues while connected; local rejection leaves valid work running.
  qos2 = publish(deferred, RUMQTTC_QOS_1, 0);
  error = NULL;
  REQUIRE(rumqttc_completion_wait_timeout_ms(qos2, NATIVE_DEADLINE_MS, &error) == RUMQTTC_LOCAL_REJECTED);
  failure(error, RUMQTTC_PUBLISH_FAILURE_MAXIMUM_QOS, 2, 0);
  rumqttc_completion_destroy(qos2);
  qos0 = publish(deferred, RUMQTTC_QOS_0, 0);
  native_wait_completion(qos0, RUMQTTC_COMPLETION_QOS0_FLUSHED);
  rumqttc_completion_destroy(qos0);
  CHECK(rumqttc_config_reset_v5_publish_budget(config, NULL));
  snapshot(deferred, 0, 0, 2, 10);
  native_close_destroy(deferred);
  native_close_destroy(strict);
  rumqttc_config_destroy(config);
  rumqttc_config_destroy(v4);
}

static void replay(void) {
  rumqttc_config_t *config = configuration("native-admission-replay");
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *pending;
  rumqttc_error_t *error = NULL;
  rumqttc_v5_connect_properties_t properties = RUMQTTC_V5_CONNECT_PROPERTIES_INIT;
  properties.session_expiry_present = 1;
  properties.session_expiry_interval = 60;
  CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_config_set_v5_connect_properties(config, &properties, NULL));
  CHECK(rumqttc_config_set_v5_publish_admission_policy(config, 1, NULL));
  CHECK(rumqttc_config_set_v5_publish_budget(config, 1, 5, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  pending = publish(client, RUMQTTC_QOS_1, 0);
  for (unsigned attempt = 1; attempt <= 3; ++attempt) {
    char name[64];
    REQUIRE(snprintf(name, sizeof(name), "native-admission-replay-seen-%u", attempt) > 0);
    native_fixture_read(name);
    snapshot(client, 1, 5, 1, 5);
    rejected(client, RUMQTTC_QOS_1, 0, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_COUNT_EXHAUSTED, 1);
    REQUIRE(snprintf(name, sizeof(name), "native-admission-replay-drop-%u", attempt) > 0);
    native_fixture_write(name, 1);
    rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED));
    REQUIRE(snprintf(name, sizeof(name), "native-admission-replay-ready-%u", attempt + 1) > 0);
    native_fixture_read(name);
    snapshot(client, 1, 5, 1, 5);
    REQUIRE(snprintf(name, sizeof(name), "native-admission-replay-release-%u", attempt + 1) > 0);
    native_fixture_write(name, 1);
    rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  }
  REQUIRE(rumqttc_completion_wait_timeout_ms(pending, NATIVE_DEADLINE_MS, &error) == RUMQTTC_AMBIGUOUS);
  failure(error, RUMQTTC_PUBLISH_FAILURE_MAXIMUM_QOS, 3, 0);
  snapshot(client, 0, 0, 1, 5);
  rumqttc_completion_destroy(pending);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

int main(void) {
  policies();
  replay();
  return 0;
}
