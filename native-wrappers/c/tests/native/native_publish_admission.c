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
  if (!present || reason != wanted)
    fprintf(stderr, "publish failure: expected %u, actual %u (present %u)\n", wanted, reason, present);
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
  rejected(strict, RUMQTTC_QOS_0, 1, RUMQTTC_LOCAL_REJECTED, RUMQTTC_PUBLISH_FAILURE_RETAIN_UNAVAILABLE, 0);
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

static void replay(uint8_t retain) {
  const char *id = retain ? "native-admission-replay-retain" : "native-admission-replay";
  rumqttc_config_t *config = configuration(id);
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
  pending = publish(client, RUMQTTC_QOS_1, retain);
  for (unsigned attempt = 1; attempt <= 3; ++attempt) {
    char name[64];
    REQUIRE(snprintf(name, sizeof(name), "%s-seen-%u", id, attempt) > 0);
    native_fixture_read(name);
    snapshot(client, 1, 5, 1, 5);
    rejected(client, RUMQTTC_QOS_1, 0, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_COUNT_EXHAUSTED, 1);
    REQUIRE(snprintf(name, sizeof(name), "%s-drop-%u", id, attempt) > 0);
    native_fixture_write(name, 1);
    rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED));
    REQUIRE(snprintf(name, sizeof(name), "%s-ready-%u", id, attempt + 1) > 0);
    native_fixture_read(name);
    snapshot(client, 1, 5, 1, 5);
    REQUIRE(snprintf(name, sizeof(name), "%s-release-%u", id, attempt + 1) > 0);
    native_fixture_write(name, 1);
    rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  }
  REQUIRE(rumqttc_completion_wait_timeout_ms(pending, NATIVE_DEADLINE_MS, &error) == RUMQTTC_AMBIGUOUS);
  failure(error, retain ? RUMQTTC_PUBLISH_FAILURE_RETAIN_UNAVAILABLE : RUMQTTC_PUBLISH_FAILURE_MAXIMUM_QOS, 3, 0);
  snapshot(client, 0, 0, 1, 5);
  rumqttc_completion_destroy(pending);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

static rumqttc_completion_t *matrix_publish(rumqttc_client_t *client, rumqttc_qos_t qos,
                                           uint8_t retain, const char *topic, uint16_t alias) {
  rumqttc_publish_options_t options = native_publish_options(qos);
  rumqttc_v5_publish_properties_t properties = RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  rumqttc_completion_t *completion = NULL;
  options.retain = retain;
  if (alias) {
    properties.topic_alias = alias;
    options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
    options.v5_properties = &properties;
  }
  CHECK(rumqttc_client_publish_tracked(client, native_string(topic), native_bytes((const uint8_t *)"data", 4),
                                     &options, &completion, NULL));
  return completion;
}

static void alias_rejected(rumqttc_client_t *client, const char *topic, uint16_t alias,
                          uint32_t status, uint32_t reason) {
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_0);
  rumqttc_v5_publish_properties_t properties = RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  rumqttc_error_t *error = NULL;
  uint64_t operation = 99;
  properties.topic_alias = alias;
  options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  options.v5_properties = &properties;
  REQUIRE(rumqttc_client_try_publish(client, native_string(topic), native_bytes((const uint8_t *)"data", 4),
                                   &options, &operation, &error) == status);
  REQUIRE(operation == 0);
  failure(error, reason, 1, status == RUMQTTC_BACKPRESSURE);
}

static void local_alias_rejected(rumqttc_client_t *client, unsigned policy,
                                 const char *topic, uint16_t alias, uint32_t reason) {
  if (!policy) {
    alias_rejected(client, topic, alias, RUMQTTC_LOCAL_REJECTED, reason);
  } else {
    rumqttc_completion_t *completion = matrix_publish(client, RUMQTTC_QOS_0, 0, topic, alias);
    rumqttc_error_t *error = NULL;
    REQUIRE(rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, &error) == RUMQTTC_LOCAL_REJECTED);
    failure(error, reason, 2, 0);
    rumqttc_completion_destroy(completion);
  }
}

static void matrix(unsigned policy) {
  char id[64], barrier[96];
  rumqttc_config_t *config;
  rumqttc_client_t *client = NULL;
  const size_t count_limit = 18, byte_limit = 200000;
  REQUIRE(snprintf(id, sizeof(id), "native-admission-matrix-%u", policy) > 0);
  config = configuration(id);
  CHECK(rumqttc_config_set_v5_publish_admission_policy(config, policy, NULL));
  CHECK(rumqttc_config_set_v5_publish_budget(config, count_limit, byte_limit, NULL));
  CHECK(rumqttc_config_set_request_capacity(config, 32, NULL));
  CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  for (unsigned generation = 1; generation <= 2; ++generation) {
    rumqttc_completion_t *pending[18];
    rumqttc_completion_kind_t kinds[18];
    size_t count = 0, bytes = 0;
    REQUIRE(snprintf(barrier, sizeof(barrier), "%s-ready-%u", id, generation) > 0);
    native_fixture_read(barrier);
    for (unsigned qos = 0; qos <= 2; ++qos) {
      for (unsigned retain = 0; retain <= 1; ++retain) {
        if (!policy && (qos || retain)) {
          rejected(client, qos, (uint8_t)retain, RUMQTTC_BACKPRESSURE,
                   RUMQTTC_PUBLISH_FAILURE_CAPABILITIES_PENDING, 1);
        } else {
          pending[count] = matrix_publish(client, qos, (uint8_t)retain, "a", 0);
          kinds[count++] = qos == 0 ? RUMQTTC_COMPLETION_QOS0_FLUSHED :
                           qos == 1 ? RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED : RUMQTTC_COMPLETION_QOS2_COMPLETED;
          bytes += 5;
        }
      }
    }
    if (!policy) {
      alias_rejected(client, "a", 1, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_CAPABILITIES_PENDING);
      alias_rejected(client, "", 1, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_CAPABILITIES_PENDING);
    } else {
      pending[count] = matrix_publish(client, RUMQTTC_QOS_0, 0, "a", 1);
      kinds[count++] = RUMQTTC_COMPLETION_QOS0_FLUSHED;
      bytes += 5;
      pending[count] = matrix_publish(client, RUMQTTC_QOS_0, 0, "", 1);
      kinds[count++] = RUMQTTC_COMPLETION_QOS0_FLUSHED;
      bytes += UINT16_MAX + 4u;
    }
    while (count < count_limit) {
      pending[count] = publish(client, RUMQTTC_QOS_0, 0);
      kinds[count++] = RUMQTTC_COMPLETION_QOS0_FLUSHED;
      bytes += 5;
    }
    snapshot(client, count_limit, bytes, count_limit, byte_limit);
    rejected(client, RUMQTTC_QOS_0, 0, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_COUNT_EXHAUSTED, 1);
    REQUIRE(snprintf(barrier, sizeof(barrier), "%s-release-%u", id, generation) > 0);
    native_fixture_write(barrier, 1);
    rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
    for (size_t index = 0; index < count; ++index) {
      native_wait_completion(pending[index], kinds[index]);
      rumqttc_completion_destroy(pending[index]);
    }
    snapshot(client, 0, 0, count_limit, byte_limit);
    // The same QoS/retain/alias matrix is valid with negotiated capabilities.
    for (unsigned qos = 0; qos <= 2; ++qos) {
      for (unsigned retain = 0; retain <= 1; ++retain) {
        rumqttc_completion_t *completion = matrix_publish(client, qos, (uint8_t)retain, "a", 0);
        native_wait_completion(completion, qos == 0 ? RUMQTTC_COMPLETION_QOS0_FLUSHED :
                               qos == 1 ? RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED : RUMQTTC_COMPLETION_QOS2_COMPLETED);
        rumqttc_completion_destroy(completion);
      }
    }
    for (unsigned binding = 0; binding < 2; ++binding) {
      rumqttc_completion_t *completion = matrix_publish(client, RUMQTTC_QOS_0, 0, binding ? "" : "a", 1);
      native_wait_completion(completion, RUMQTTC_COMPLETION_QOS0_FLUSHED);
      rumqttc_completion_destroy(completion);
    }
    local_alias_rejected(client, policy, "a", 3, RUMQTTC_PUBLISH_FAILURE_TOPIC_ALIAS_MAXIMUM);
    if (generation == 1) {
      rumqttc_completion_t *drop = matrix_publish(client, RUMQTTC_QOS_0, 0, "matrix/drop", 0);
      native_wait_completion(drop, RUMQTTC_COMPLETION_QOS0_FLUSHED);
      rumqttc_completion_destroy(drop);
      rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED));
    }
  }
  // No ACKs are returned until capacity exhaustion and failed alias admission
  // have been observed. Native queue transfers cannot replenish the count.
  {
    rumqttc_completion_t *held[18];
    for (size_t index = 0; index < count_limit; ++index)
      held[index] = matrix_publish(client, RUMQTTC_QOS_1, 0, "hold", 0);
    REQUIRE(snprintf(barrier, sizeof(barrier), "%s-held", id) > 0);
    native_fixture_read(barrier);
    snapshot(client, count_limit, count_limit * 8, count_limit, byte_limit);
    alias_rejected(client, "b", 2, RUMQTTC_BACKPRESSURE, RUMQTTC_PUBLISH_FAILURE_COUNT_EXHAUSTED);
    rumqttc_completion_destroy(held[0]);
    snapshot(client, count_limit, count_limit * 8, count_limit, byte_limit);
    REQUIRE(snprintf(barrier, sizeof(barrier), "%s-ack", id) > 0);
    native_fixture_write(barrier, 1);
    for (size_t index = 1; index < count_limit; ++index) {
      native_wait_completion(held[index], RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
      native_wait_completion(held[index], RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
      rumqttc_completion_destroy(held[index]);
    }
    snapshot(client, 0, 0, count_limit, byte_limit);
  }
  local_alias_rejected(client, policy, "", 2, RUMQTTC_PUBLISH_FAILURE_TOPIC_ALIAS_UNMAPPED);
  {
    rumqttc_completion_t *qos2 = matrix_publish(client, RUMQTTC_QOS_2, 0, "hold2", 0);
    REQUIRE(snprintf(barrier, sizeof(barrier), "%s-pubrel", id) > 0);
    native_fixture_read(barrier);
    snapshot(client, 1, 9, count_limit, byte_limit);
    REQUIRE(rumqttc_completion_wait_timeout_ms(qos2, 0, NULL) == RUMQTTC_TIMEOUT);
    snapshot(client, 1, 9, count_limit, byte_limit);
    REQUIRE(snprintf(barrier, sizeof(barrier), "%s-pubcomp", id) > 0);
    native_fixture_write(barrier, 1);
    native_wait_completion(qos2, RUMQTTC_COMPLETION_QOS2_COMPLETED);
    rumqttc_completion_destroy(qos2);
    snapshot(client, 0, 0, count_limit, byte_limit);
  }
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

static void capacity_matrix(unsigned policy, unsigned bytes_only) {
  char id[64], barrier[96];
  rumqttc_client_t *client = NULL;
  const size_t byte_limit = bytes_only ? 5 : 32;
  const uint32_t reason = bytes_only ? RUMQTTC_PUBLISH_FAILURE_BYTES_EXHAUSTED :
                                     RUMQTTC_PUBLISH_FAILURE_REQUEST_CHANNEL_FULL;
  REQUIRE(snprintf(id, sizeof(id), "native-admission-capacity-%u-%u", policy, bytes_only) > 0);
  rumqttc_config_t *config = configuration(id);
  CHECK(rumqttc_config_set_v5_publish_admission_policy(config, policy, NULL));
  CHECK(rumqttc_config_set_v5_publish_budget(config, 4, byte_limit, NULL));
  CHECK(rumqttc_config_set_request_capacity(config, bytes_only ? 4 : 1, NULL));
  CHECK(rumqttc_config_set_v5_outgoing_inflight_upper_limit(config, 1, NULL));
  CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  for (unsigned generation = 1; generation <= 2; ++generation) {
    REQUIRE(snprintf(barrier, sizeof(barrier), "%s-ready-%u", id, generation) > 0);
    native_fixture_read(barrier);
    rumqttc_completion_t *first = publish(client, RUMQTTC_QOS_0, 0);
    rejected(client, RUMQTTC_QOS_0, 0, RUMQTTC_BACKPRESSURE, reason, 1);
    snapshot(client, 1, 5, 4, byte_limit);
    REQUIRE(snprintf(barrier, sizeof(barrier), "%s-release-%u", id, generation) > 0);
    native_fixture_write(barrier, 1);
    rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
    native_wait_completion(first, RUMQTTC_COMPLETION_QOS0_FLUSHED);
    rumqttc_completion_destroy(first);
    snapshot(client, 0, 0, 4, byte_limit);
    first = publish(client, RUMQTTC_QOS_1, 0);
    REQUIRE(snprintf(barrier, sizeof(barrier), "%s-held-%u", id, generation) > 0);
    native_fixture_read(barrier);
    // An unacknowledged PUBLISH closes the protocol window, so the next
    // channel admission cannot be consumed until the broker releases it.
    rumqttc_completion_t *queued = bytes_only ? NULL : publish(client, RUMQTTC_QOS_1, 0);
    rejected(client, RUMQTTC_QOS_1, 0, RUMQTTC_BACKPRESSURE, reason, 1);
    snapshot(client, bytes_only ? 1 : 2, bytes_only ? 5 : 10, 4, byte_limit);
    REQUIRE(snprintf(barrier, sizeof(barrier), "%s-ack-%u", id, generation) > 0);
    native_fixture_write(barrier, 1);
    native_wait_completion(first, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    rumqttc_completion_destroy(first);
    if (queued) {
      native_wait_completion(queued, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
      rumqttc_completion_destroy(queued);
    }
    snapshot(client, 0, 0, 4, byte_limit);
    if (generation == 1) {
      first = publish(client, RUMQTTC_QOS_0, 0);
      native_wait_completion(first, RUMQTTC_COMPLETION_QOS0_FLUSHED);
      rumqttc_completion_destroy(first);
      rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED));
    }
  }
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

static void unbound_alias_replay(void) {
  rumqttc_config_t *config = configuration("native-admission-unbound");
  rumqttc_client_t *client = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_completion_t *first, *alias, *valid;
  CHECK(rumqttc_config_set_v5_publish_admission_policy(config, 1, NULL));
  CHECK(rumqttc_config_set_v5_publish_budget(config, 2, UINT16_MAX + 9u, NULL));
  CHECK(rumqttc_config_set_v5_outgoing_inflight_upper_limit(config, 1, NULL));
  CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  first = publish(client, RUMQTTC_QOS_1, 0);
  native_fixture_read("native-admission-unbound-ready");
  // The first unacknowledged PUBLISH fills the protocol window. This alias-only
  // request must stay unsent until cleanup diagnoses the missing replay binding.
  alias = matrix_publish(client, RUMQTTC_QOS_1, 0, "", 1);
  snapshot(client, 2, UINT16_MAX + 9u, 2, UINT16_MAX + 9u);
  native_fixture_write("native-admission-unbound-drop", 1);
  REQUIRE(rumqttc_completion_wait_timeout_ms(alias, NATIVE_DEADLINE_MS, &error) == RUMQTTC_LOCAL_REJECTED);
  failure(error, RUMQTTC_PUBLISH_FAILURE_TOPIC_ALIAS_REPLAY_UNAVAILABLE, 2, 0);
  rumqttc_completion_destroy(alias);
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  native_wait_completion(first, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  rumqttc_completion_destroy(first);
  snapshot(client, 0, 0, 2, UINT16_MAX + 9u);
  valid = publish(client, RUMQTTC_QOS_1, 0);
  native_wait_completion(valid, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  rumqttc_completion_destroy(valid);
  snapshot(client, 0, 0, 2, UINT16_MAX + 9u);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

static void session_loss(void) {
  rumqttc_config_t *config = configuration("native-admission-session-reset");
  rumqttc_client_t *client = NULL;
  rumqttc_error_t *error = NULL;
  CHECK(rumqttc_config_set_v5_publish_admission_policy(config, 1, NULL));
  CHECK(rumqttc_config_set_v5_publish_budget(config, 1, 5, NULL));
  CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  rumqttc_completion_t *pending = publish(client, RUMQTTC_QOS_1, 0);
  native_fixture_read("native-admission-session-reset-seen");
  snapshot(client, 1, 5, 1, 5);
  native_fixture_write("native-admission-session-reset-drop", 1);
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED));
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  REQUIRE(rumqttc_completion_wait_timeout_ms(pending, NATIVE_DEADLINE_MS, &error) == RUMQTTC_AMBIGUOUS);
  failure(error, RUMQTTC_PUBLISH_FAILURE_SESSION_RESET, 3, 0);
  rumqttc_completion_destroy(pending);
  snapshot(client, 0, 0, 1, 5);
  pending = publish(client, RUMQTTC_QOS_1, 0);
  native_wait_completion(pending, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  rumqttc_completion_destroy(pending);
  snapshot(client, 0, 0, 1, 5);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

int main(void) {
  policies();
  replay(0);
  replay(1);
  matrix(0);
  matrix(1);
  for (unsigned policy = 0; policy <= 1; ++policy)
    for (unsigned bytes_only = 0; bytes_only <= 1; ++bytes_only)
      capacity_matrix(policy, bytes_only);
  unbound_alias_replay();
  session_loss();
  return 0;
}
