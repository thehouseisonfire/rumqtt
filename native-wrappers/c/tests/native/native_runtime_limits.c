#include "native_common.h"

#include <stdio.h>
#include <string.h>

static rumqttc_config_t *configuration(rumqttc_protocol_t protocol, const char *id) {
  rumqttc_config_t *config = NULL;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
  return config;
}

static rumqttc_client_t *start(rumqttc_config_t *config) {
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  return client;
}

static rumqttc_completion_t *publish(rumqttc_client_t *client, const char *topic, const uint8_t *payload, size_t size,
                                     const rumqttc_publish_options_t *options) {
  rumqttc_completion_t *completion = NULL;
  CHECK(rumqttc_client_publish_tracked(client, native_string(topic), native_bytes(payload, size), options, &completion,
                                       NULL));
  return completion;
}

static void finish(rumqttc_completion_t *completion, rumqttc_completion_kind_t kind) {
  native_wait_completion(completion, kind);
  rumqttc_completion_destroy(completion);
}

static void protocol_failure(rumqttc_client_t *client, rumqttc_completion_t *pending) {
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
  rumqttc_error_t *error = NULL;
  rumqttc_error_kind_t kind = 0;
  CHECK(rumqttc_event_disconnected(event, NULL, &error));
  CHECK(rumqttc_error_kind(error, &kind));
  REQUIRE(kind == RUMQTTC_ERROR_PROTOCOL);
  rumqttc_error_destroy(error);
  rumqttc_event_destroy(event);
  CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  REQUIRE(rumqttc_completion_wait_timeout_ms(pending, NATIVE_DEADLINE_MS, &error) == RUMQTTC_AMBIGUOUS);
  {
    uint64_t id = 0, error_id = 0;
    uint8_t present = 0;
    CHECK(rumqttc_completion_operation_id(pending, &id));
    CHECK(rumqttc_error_operation_id(error, &present, &error_id));
    REQUIRE(present == 1 && id != 0 && id == error_id);
  }
  rumqttc_error_destroy(error);
  rumqttc_completion_destroy(pending);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
}

static void inflight(rumqttc_protocol_t protocol, uint16_t local, const char *suffix, uint32_t batch) {
  char id[96];
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *pending[3];
  rumqttc_config_t *config;
  rumqttc_client_t *client, *control;
  REQUIRE(snprintf(id, sizeof(id), "native-runtime-inflight-%u-%s", protocol, suffix) > 0);
  config = configuration(protocol, id);
  if (protocol == RUMQTTC_PROTOCOL_V4) {
    REQUIRE(rumqttc_config_set_v4_inflight_limit(config, 0, NULL) == RUMQTTC_INVALID_ARGUMENT);
    CHECK(rumqttc_config_set_v4_inflight_limit(config, local, NULL));
  } else {
    REQUIRE(rumqttc_config_set_v5_outgoing_inflight_upper_limit(config, 0, NULL) == RUMQTTC_INVALID_ARGUMENT);
    CHECK(rumqttc_config_clear_v5_outgoing_inflight_upper_limit(config, NULL));
    CHECK(rumqttc_config_set_v5_outgoing_inflight_upper_limit(config, local, NULL));
  }
  CHECK(rumqttc_config_set_max_request_batch(config, batch, NULL));
  CHECK(rumqttc_config_set_read_batch_size(config, batch, NULL));
  client = start(config);
  control = native_start_client(
      protocol, protocol == RUMQTTC_PROTOCOL_V4 ? "native-runtime-control-v4" : "native-runtime-control-v5",
      RUMQTTC_ACK_AUTOMATIC, 8, 64, NATIVE_DEADLINE_MS);
  for (unsigned i = 0; i < 3; ++i)
    pending[i] = publish(client, "a", (const uint8_t *)"payload", 7, &options);
  options.qos = RUMQTTC_QOS_0;
  finish(publish(control, "native/runtime/admitted", (const uint8_t *)id, strlen(id), &options),
         RUMQTTC_COMPLETION_QOS0_FLUSHED);
  for (unsigned i = 0; i < 3; ++i)
    finish(pending[i], RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  native_close_destroy(control);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

static void outgoing(rumqttc_protocol_t protocol) {
  uint8_t payload[26];
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_config_t *config =
      configuration(protocol, protocol == RUMQTTC_PROTOCOL_V4 ? "native-outgoing-v4" : "native-outgoing-v5");
  rumqttc_client_t *client;
  size_t size = protocol == RUMQTTC_PROTOCOL_V4 ? 25 : 24;
  memset(payload, 'x', sizeof(payload));
  if (protocol == RUMQTTC_PROTOCOL_V4) {
    REQUIRE(rumqttc_config_set_v4_outgoing_packet_limit_bytes(config, 0, NULL) == RUMQTTC_INVALID_ARGUMENT);
    CHECK(rumqttc_config_reset_v4_outgoing_packet_limit(config, NULL));
    CHECK(rumqttc_config_set_v4_outgoing_packet_limit_bytes(config, 32, NULL));
  }
  client = start(config);
  finish(publish(client, "a", payload, size, &options), RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  protocol_failure(client, publish(client, "a", payload, size + 1, &options));
  rumqttc_config_destroy(config);
}

static void incoming(rumqttc_protocol_t protocol, unsigned mode) {
  char id[96];
  const char *name = mode == 0 ? "bytes" : mode == 1 ? "default" : "unlimited";
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_config_t *config;
  rumqttc_client_t *client;
  rumqttc_completion_t *pending;
  REQUIRE(snprintf(id, sizeof(id), "native-runtime-incoming-%u-%s", protocol, name) > 0);
  config = configuration(protocol, id);
  REQUIRE(rumqttc_config_set_local_incoming_packet_limit_bytes(config, 0, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_set_local_incoming_packet_limit_mode(config, UINT32_MAX, NULL) == RUMQTTC_INVALID_ARGUMENT);
  CHECK(rumqttc_config_set_local_incoming_packet_limit_bytes(config, 32, NULL));
  if (mode != 0)
    CHECK(rumqttc_config_set_local_incoming_packet_limit_mode(
        config, mode == 1 ? RUMQTTC_INCOMING_PACKET_LIMIT_DEFAULT : RUMQTTC_INCOMING_PACKET_LIMIT_UNLIMITED, NULL));
  if (protocol == RUMQTTC_PROTOCOL_V5) {
    REQUIRE(rumqttc_config_set_v5_advertised_max_packet_size_bytes(config, 0, NULL) == RUMQTTC_INVALID_ARGUMENT);
    CHECK(rumqttc_config_clear_v5_advertised_max_packet_size(config, NULL));
    CHECK(rumqttc_config_set_v5_advertised_max_packet_size_bytes(config, 1024, NULL));
  }
  client = start(config);
  pending = publish(client, "a", (const uint8_t *)"small", 5, &options);
  if (mode == 0) {
    protocol_failure(client, pending);
  } else {
    for (unsigned i = 0; i < 2; ++i) {
      rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
      rumqttc_bytes_view_t payload = {NULL, 0};
      CHECK(rumqttc_event_publish(event, NULL, &payload, NULL, NULL, NULL, NULL));
      REQUIRE(payload.len == (i == 0 ? 5 : 64));
      REQUIRE(memcmp(payload.data, i == 0 ? "small" : "xxxxxxxx", i == 0 ? 5 : 8) == 0);
      rumqttc_event_destroy(event);
    }
    finish(pending, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    native_close_destroy(client);
  }
  rumqttc_config_destroy(config);
}

static void removed_limits(rumqttc_protocol_t protocol) {
  char id[64];
  uint8_t payload[256];
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *pending[5];
  REQUIRE(snprintf(id, sizeof(id), "native-cleared-limits-%u", protocol) > 0);
  rumqttc_config_t *config = configuration(protocol, id);
  if (protocol == RUMQTTC_PROTOCOL_V4) {
    CHECK(rumqttc_config_set_v4_outgoing_packet_limit_bytes(config, 32, NULL));
    CHECK(rumqttc_config_reset_v4_outgoing_packet_limit(config, NULL));
  } else {
    CHECK(rumqttc_config_set_v5_outgoing_inflight_upper_limit(config, 1, NULL));
    CHECK(rumqttc_config_clear_v5_outgoing_inflight_upper_limit(config, NULL));
    CHECK(rumqttc_config_set_v5_advertised_max_packet_size_bytes(config, 32, NULL));
    CHECK(rumqttc_config_clear_v5_advertised_max_packet_size(config, NULL));
  }
  rumqttc_client_t *client = start(config);
  memset(payload, 0xff, sizeof(payload));
  unsigned count = protocol == RUMQTTC_PROTOCOL_V4 ? 1 : 5;
  for (unsigned i = 0; i < count; ++i)
    pending[i] = publish(client, "native/cleared", payload, sizeof(payload), &options);
  for (unsigned i = 0; i < count; ++i)
    finish(pending[i], RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  if (protocol == RUMQTTC_PROTOCOL_V5) {
    rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
    rumqttc_bytes_view_t bytes = {NULL, 0};
    CHECK(rumqttc_event_publish(event, NULL, &bytes, NULL, NULL, NULL, NULL));
    REQUIRE(bytes.len == 512);
    for (size_t i = 0; i < bytes.len; ++i)
      REQUIRE(bytes.data[i] == 0x80);
    rumqttc_event_destroy(event);
  }
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

static void alias(unsigned policy) {
  char id[64];
  rumqttc_config_t *config;
  rumqttc_client_t *client;
  rumqttc_v5_publish_properties_t properties = RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *pending, *rejected = (rumqttc_completion_t *)(uintptr_t)1;
  REQUIRE(snprintf(id, sizeof(id), "native-alias-%u", policy) > 0);
  config = configuration(RUMQTTC_PROTOCOL_V5, id);
  REQUIRE(rumqttc_config_set_v5_topic_alias_policy(config, UINT32_MAX, NULL) == RUMQTTC_INVALID_ARGUMENT);
  CHECK(rumqttc_config_set_v5_topic_alias_policy(config, policy, NULL));
  CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  if (policy == RUMQTTC_TOPIC_ALIAS_DISABLED) {
    properties.topic_alias = 1;
    options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
    options.v5_properties = &properties;
  }
  client = start(config);
  finish(publish(client, "native/alias", (const uint8_t *)"alias", 5, &options), RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  pending = publish(client, policy == RUMQTTC_TOPIC_ALIAS_DISABLED ? "" : "native/alias", (const uint8_t *)"alias", 5,
                    &options);
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED));
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  finish(pending, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  options.v5_properties = &properties;
  properties.topic_alias = 1;
  REQUIRE(rumqttc_client_publish_tracked(client, native_string(""), native_bytes((const uint8_t *)"alias", 5), &options,
                                         &rejected, NULL) != RUMQTTC_OK);
  REQUIRE(rejected == NULL);
  options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL;
  options.v5_properties = NULL;
  finish(publish(client, "native/alias", (const uint8_t *)"alias", 5, &options), RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

int main(void) {
  inflight(RUMQTTC_PROTOCOL_V4, 2, "local", 0);
  inflight(RUMQTTC_PROTOCOL_V5, 2, "local", 1);
  inflight(RUMQTTC_PROTOCOL_V5, 5, "remote", 8);
  for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
    removed_limits(protocol);
    outgoing(protocol);
    for (unsigned mode = 0; mode < 3; ++mode)
      incoming(protocol, mode);
  }
  for (unsigned policy = RUMQTTC_TOPIC_ALIAS_DISABLED; policy <= RUMQTTC_TOPIC_ALIAS_LRU; ++policy)
    alias(policy);
  return 0;
}
