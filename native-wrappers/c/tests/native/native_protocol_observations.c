#include "native_common.h"
#include <string.h>

static rumqttc_event_t *next(rumqttc_client_t *client, uint32_t expected, uint32_t packet, uint32_t protocol) {
  rumqttc_event_t *event = NULL;
  uint32_t kind = 0;
  CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
  CHECK(rumqttc_event_kind(event, &kind));
  REQUIRE(kind == expected);
  rumqttc_protocol_observation_details_t info = RUMQTTC_PROTOCOL_OBSERVATION_DETAILS_INIT;
  /* ERROR_OUT_SUCCESS: rumqttc_event_protocol_observation */
  CHECK(rumqttc_event_protocol_observation(event, &info, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_event_protocol_observation */
  REQUIRE(rumqttc_event_protocol_observation(event, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  rumqttc_protocol_observation_details_t small;
  memset(&small, 0xa5, sizeof(small));
  small.struct_size = 8;
  REQUIRE(rumqttc_event_protocol_observation(event, &small, NULL) == RUMQTTC_INVALID_ARGUMENT);
  const unsigned char *raw = (const unsigned char *)&small;
  for (size_t i = sizeof(uint32_t); i < 8; ++i)
    REQUIRE(raw[i] == 0);
  for (size_t i = 8; i < sizeof(small); ++i)
    REQUIRE(raw[i] == 0xa5);
  rumqttc_ack_token_t *token = NULL;
  REQUIRE(rumqttc_event_take_ack_token(event, &token, NULL) == RUMQTTC_INVALID_STATE && token == NULL);
  struct {
    rumqttc_protocol_observation_details_t details;
    uint64_t canary;
  } extended;
  memset(&extended, 0, sizeof(extended));
  extended.details.struct_size = sizeof(extended);
  extended.canary = UINT64_MAX;
  CHECK(rumqttc_event_protocol_observation(event, &extended.details, NULL));
  REQUIRE(extended.canary == UINT64_MAX && extended.details.present);
  REQUIRE(info.present && info.packet_kind == packet && info.protocol == protocol);
  REQUIRE(info.generation_present && info.connection_generation == 1);
  REQUIRE(info.direction ==
          (expected == RUMQTTC_EVENT_OUTGOING ? RUMQTTC_DIRECTION_OUTGOING : RUMQTTC_DIRECTION_INCOMING));
  if (expected == RUMQTTC_EVENT_OUTGOING) {
    REQUIRE(!info.reason_present && !info.properties_available && !info.properties_present && !info.results_present);
    size_t count = SIZE_MAX;
    REQUIRE(rumqttc_event_protocol_observation_user_property_count(event, &count, NULL) == RUMQTTC_INVALID_STATE);
    REQUIRE(count == 0);
  }
  return event;
}

static void outgoing(rumqttc_client_t *client, uint32_t packet, uint32_t protocol, int coarse, int precise) {
  if (!coarse && !precise)
    return;
  if (precise) {
    rumqttc_event_t *event = next(client, RUMQTTC_EVENT_OUTGOING, packet, protocol);
    rumqttc_event_destroy(event);
  } else {
    rumqttc_event_t *event = NULL;
    uint32_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    REQUIRE(kind == RUMQTTC_EVENT_OUTGOING);
    rumqttc_protocol_observation_details_t info = RUMQTTC_PROTOCOL_OBSERVATION_DETAILS_INIT;
    /* ERROR_OUT_SUCCESS: rumqttc_event_protocol_observation */
    CHECK(rumqttc_event_protocol_observation(event, &info, NULL));
    REQUIRE(!info.present && !info.packet_kind);
    rumqttc_event_destroy(event);
  }
}

static rumqttc_event_t *incoming(rumqttc_client_t *client, uint32_t packet, uint32_t protocol, int selected,
                                 uint8_t reason) {
  if (!selected)
    return NULL;
  rumqttc_event_t *event = next(client, RUMQTTC_EVENT_PROTOCOL_OBSERVATION, packet, protocol);
  rumqttc_protocol_observation_details_t info = RUMQTTC_PROTOCOL_OBSERVATION_DETAILS_INIT;
  /* ERROR_OUT_SUCCESS: rumqttc_event_protocol_observation */
  CHECK(rumqttc_event_protocol_observation(event, &info, NULL));
  REQUIRE(info.packet_id_present && info.packet_id != 0);
  if (packet != RUMQTTC_PACKET_SUBACK && packet != RUMQTTC_PACKET_UNSUBACK) {
    REQUIRE(info.reason_present == (protocol == RUMQTTC_PROTOCOL_V5));
    if (info.reason_present)
      REQUIRE(info.reason == reason);
  }
  REQUIRE(info.properties_available == (protocol == RUMQTTC_PROTOCOL_V5));
  return event;
}

static void check_properties(rumqttc_event_t *event) {
  uint8_t present = 0;
  rumqttc_string_view_t value = {NULL, 0};
  /* ERROR_OUT_SUCCESS: rumqttc_event_protocol_observation_reason_string */
  CHECK(rumqttc_event_protocol_observation_reason_string(event, &present, &value, NULL));
  REQUIRE(present && value.len == strlen("terminal-private-reason"));
  REQUIRE(memcmp(value.data, "terminal-private-reason", value.len) == 0);
  size_t count = 0;
  /* ERROR_OUT_SUCCESS: rumqttc_event_protocol_observation_user_property_count */
  CHECK(rumqttc_event_protocol_observation_user_property_count(event, &count, NULL));
  REQUIRE(count == 2);
  rumqttc_string_view_t key = {NULL, 0};
  /* ERROR_OUT_SUCCESS: rumqttc_event_protocol_observation_user_property_at */
  CHECK(rumqttc_event_protocol_observation_user_property_at(event, 1, &key, &value, NULL));
  REQUIRE(key.len == strlen("terminal-private-key") && value.len == 0);
  value.data = (const char *)(uintptr_t)1;
  value.len = SIZE_MAX;
  /* ERROR_OUT_FAILURE: rumqttc_event_protocol_observation_user_property_at */
  REQUIRE(rumqttc_event_protocol_observation_user_property_at(event, count, NULL, &value, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(value.data == NULL && value.len == 0);
}

static void run(uint32_t protocol, int coarse, int precise, int controls) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-terminal-observe"), NULL));
  CHECK(rumqttc_config_set_emit_outgoing_events(config, (uint8_t)coarse, NULL));
  uint32_t flags = (precise ? RUMQTTC_OBSERVE_PRECISE_OUTGOING : 0) | (controls ? RUMQTTC_OBSERVE_INCOMING_CONTROL : 0);
  /* ERROR_OUT_FAILURE: rumqttc_config_set_protocol_observations */
  REQUIRE(rumqttc_config_set_protocol_observations(config, 8, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_protocol_observations */
  CHECK(rumqttc_config_set_protocol_observations(config, flags, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_t *connected = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_protocol_observation_details_t metadata = RUMQTTC_PROTOCOL_OBSERVATION_DETAILS_INIT;
  REQUIRE(rumqttc_event_protocol_observation(connected, &metadata, NULL) == RUMQTTC_INVALID_STATE);
  REQUIRE(!metadata.present);
  rumqttc_event_destroy(connected);
  rumqttc_event_t *retained = NULL;
  for (unsigned scenario = 0; scenario < 5; ++scenario) {
    uint32_t qos = scenario == 0 ? RUMQTTC_QOS_1 : RUMQTTC_QOS_2;
    const char *topic = scenario == 2 ? "rejected" : scenario == 3 ? "absent" : scenario == 4 ? "empty" : "matched";
    int rejected = scenario == 2 && protocol == RUMQTTC_PROTOCOL_V5;
    rumqttc_publish_options_t options = native_publish_options(qos);
    rumqttc_completion_t *completion = NULL;
    CHECK(rumqttc_client_publish_tracked(client, native_string(topic), native_bytes(NULL, 0), &options, &completion,
                                         NULL));
    REQUIRE(rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, NULL) ==
            (rejected ? RUMQTTC_BROKER_REJECTED : RUMQTTC_OK));
    outgoing(client, RUMQTTC_PACKET_PUBLISH, protocol, coarse, precise);
    rumqttc_event_t *event =
        incoming(client, scenario == 0 ? RUMQTTC_PACKET_PUBACK : RUMQTTC_PACKET_PUBREC, protocol, controls,
                 rejected       ? 0x87
                 : scenario < 2 ? 0x10
                                : 0);
    if (event && protocol == RUMQTTC_PROTOCOL_V5) {
      if (scenario < 3)
        check_properties(event);
      else {
        uint8_t present = UINT8_MAX;
        rumqttc_string_view_t value = {(const char *)(uintptr_t)1, SIZE_MAX};
        /* ERROR_OUT_SUCCESS: rumqttc_event_protocol_observation_reason_string */
        CHECK(rumqttc_event_protocol_observation_reason_string(event, &present, &value, NULL));
        REQUIRE(present == (scenario == 4));
        REQUIRE(value.len == 0);
      }
    }
    if (scenario == 0)
      retained = event;
    else
      rumqttc_event_destroy(event);
    if (scenario != 0 && !rejected) {
      outgoing(client, RUMQTTC_PACKET_PUBREL, protocol, coarse, precise);
      rumqttc_event_destroy(incoming(client, RUMQTTC_PACKET_PUBCOMP, protocol, controls, 0));
    }
    rumqttc_completion_destroy(completion);
  }
  rumqttc_subscription_t subscriptions[3] = {native_subscription("a", RUMQTTC_QOS_1),
                                             native_subscription("b", RUMQTTC_QOS_1),
                                             native_subscription("c", RUMQTTC_QOS_2)};
  rumqttc_string_view_t filters[3] = {native_string("a"), native_string("b"), native_string("c")};
  for (int unsubscribe = 0; unsubscribe < 2; ++unsubscribe) {
    rumqttc_completion_t *completion = NULL;
    if (unsubscribe)
      CHECK(rumqttc_client_unsubscribe_tracked(client, filters, 3, NULL, &completion, NULL));
    else
      CHECK(rumqttc_client_subscribe_tracked(client, subscriptions, 3, NULL, &completion, NULL));
    CHECK(rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, NULL));
    outgoing(client, unsubscribe ? RUMQTTC_PACKET_UNSUBSCRIBE : RUMQTTC_PACKET_SUBSCRIBE, protocol, coarse, precise);
    rumqttc_event_t *event =
        incoming(client, unsubscribe ? RUMQTTC_PACKET_UNSUBACK : RUMQTTC_PACKET_SUBACK, protocol, controls, 0);
    if (event) {
      uint8_t present = 0, code = 0;
      size_t count = 0;
      /* ERROR_OUT_SUCCESS: rumqttc_event_protocol_observation_result_count */
      CHECK(rumqttc_event_protocol_observation_result_count(event, &present, &count, NULL));
      REQUIRE(present == (!unsubscribe || protocol == RUMQTTC_PROTOCOL_V5));
      REQUIRE(count == (present ? 3u : 0u));
      if (present) {
        /* ERROR_OUT_SUCCESS: rumqttc_event_protocol_observation_result_at */
        CHECK(rumqttc_event_protocol_observation_result_at(event, 1, &code, NULL));
        REQUIRE(code == (unsubscribe ? 0x11 : protocol == RUMQTTC_PROTOCOL_V5 ? 0x87 : 0x80));
      }
      rumqttc_event_destroy(event);
    }
    rumqttc_completion_destroy(completion);
  }
  /* A broker PUBLISH fences all preceding native events, including when every
   * observation selector is disabled. Any extra event breaks this sequence. */
  rumqttc_publish_options_t fence_options = native_publish_options(RUMQTTC_QOS_0);
  rumqttc_completion_t *fence = NULL;
  CHECK(rumqttc_client_publish_tracked(client, native_string("observation-fence"), native_bytes(NULL, 0),
                                       &fence_options, &fence, NULL));
  native_wait_completion(fence, RUMQTTC_COMPLETION_QOS0_FLUSHED);
  outgoing(client, RUMQTTC_PACKET_PUBLISH, protocol, coarse, precise);
  rumqttc_event_t *fence_event = NULL;
  uint32_t fence_kind = 0;
  CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &fence_event, NULL));
  CHECK(rumqttc_event_kind(fence_event, &fence_kind));
  REQUIRE(fence_kind == RUMQTTC_EVENT_INCOMING_PUBLISH);
  rumqttc_event_destroy(fence_event);
  rumqttc_completion_destroy(fence);
  native_close_destroy(client);
  if (retained) {
    if (protocol == RUMQTTC_PROTOCOL_V5) {
      check_properties(retained);
      rumqttc_string_view_t view = {NULL, 0};
      CHECK(rumqttc_event_protocol_observation_reason_string(retained, NULL, &view, NULL));
      char copy[64];
      size_t written = 0;
      CHECK(rumqttc_string_copy(view, copy, sizeof(copy), &written));
      rumqttc_event_destroy(retained);
      REQUIRE(written == strlen("terminal-private-reason") && memcmp(copy, "terminal-private-reason", written) == 0);
    } else
      rumqttc_event_destroy(retained);
  }
  rumqttc_config_destroy(config);
}

static void inbound(uint32_t protocol, int manual) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-terminal-inbound"), NULL));
  CHECK(rumqttc_config_set_ack_mode(config, manual ? RUMQTTC_ACK_MANUAL : RUMQTTC_ACK_AUTOMATIC, NULL));
  CHECK(rumqttc_config_set_keep_alive_seconds(config, 1, NULL));
  CHECK(rumqttc_config_set_protocol_observations(
      config, RUMQTTC_OBSERVE_INCOMING_CONTROL | RUMQTTC_OBSERVE_PRECISE_OUTGOING, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  rumqttc_completion_t *completion = NULL;
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_0);
  CHECK(rumqttc_client_publish_tracked(client, native_string("observation-inbound"), native_bytes(NULL, 0), &options,
                                       &completion, NULL));
  native_wait_completion(completion, RUMQTTC_COMPLETION_QOS0_FLUSHED);
  rumqttc_completion_destroy(completion);
  unsigned publishes = 0, pubrec = 0, pubrel = 0, pubcomp = 0, pingreq = 0, pingresp = 0;
  while (!pingresp) {
    rumqttc_event_t *event = NULL;
    uint32_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    if (kind == RUMQTTC_EVENT_INCOMING_PUBLISH) {
      ++publishes;
      uint8_t available = 0;
      CHECK(rumqttc_event_publish(event, NULL, NULL, NULL, NULL, NULL, &available));
      if (manual && available) {
        rumqttc_completion_t *acknowledged = NULL;
        CHECK(rumqttc_client_acknowledge_tracked(client, event, &acknowledged, NULL));
        native_wait_completion(acknowledged, RUMQTTC_COMPLETION_ACKNOWLEDGED);
        rumqttc_completion_destroy(acknowledged);
      }
    } else {
      rumqttc_protocol_observation_details_t info = RUMQTTC_PROTOCOL_OBSERVATION_DETAILS_INIT;
      /* ERROR_OUT_SUCCESS: rumqttc_event_protocol_observation */
      CHECK(rumqttc_event_protocol_observation(event, &info, NULL));
      REQUIRE(info.present && info.generation_present && info.connection_generation == 1);
      if (kind == RUMQTTC_EVENT_OUTGOING) {
        REQUIRE(!info.reason_present && !info.properties_available);
        if (info.packet_kind == RUMQTTC_PACKET_PUBREC)
          ++pubrec;
        else if (info.packet_kind == RUMQTTC_PACKET_PUBCOMP)
          ++pubcomp;
        else if (info.packet_kind == RUMQTTC_PACKET_PINGREQ)
          ++pingreq;
        else
          REQUIRE(info.packet_kind == RUMQTTC_PACKET_PUBLISH);
      } else {
        REQUIRE(kind == RUMQTTC_EVENT_PROTOCOL_OBSERVATION);
        if (info.packet_kind == RUMQTTC_PACKET_PUBREL)
          ++pubrel;
        else {
          REQUIRE(info.packet_kind == RUMQTTC_PACKET_PINGRESP);
          ++pingresp;
        }
      }
    }
    rumqttc_event_destroy(event);
  }
  REQUIRE(publishes == 2 && pubrec == 2 && pubrel == 2 && pubcomp == 2 && pingreq == 1 && pingresp == 1);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

static void recovery(uint32_t protocol) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(
      config,
      native_string(protocol == RUMQTTC_PROTOCOL_V5 ? "native-terminal-recovery-v5" : "native-terminal-recovery-v4"),
      NULL));
  if (protocol == RUMQTTC_PROTOCOL_V5)
    CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  else
    CHECK(rumqttc_config_set_v4_clean_session(config, 0, NULL));
  CHECK(rumqttc_config_set_protocol_observations(
      config, RUMQTTC_OBSERVE_INCOMING_CONTROL | RUMQTTC_OBSERVE_PRECISE_OUTGOING, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  rumqttc_completion_t *completion = NULL;
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_2);
  CHECK(rumqttc_client_publish_tracked(client, native_string("recovery"), native_bytes(NULL, 0), &options, &completion,
                                       NULL));
  native_wait_completion(completion, RUMQTTC_COMPLETION_QOS2_COMPLETED);
  unsigned releases = 0, connections = 0;
  uint16_t identifier = 0;
  for (;;) {
    rumqttc_event_t *event = NULL;
    uint32_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    if (kind == RUMQTTC_EVENT_CONNECTED)
      ++connections;
    if (kind == RUMQTTC_EVENT_OUTGOING || kind == RUMQTTC_EVENT_PROTOCOL_OBSERVATION) {
      rumqttc_protocol_observation_details_t info = RUMQTTC_PROTOCOL_OBSERVATION_DETAILS_INIT;
      /* ERROR_OUT_SUCCESS: rumqttc_event_protocol_observation */
      CHECK(rumqttc_event_protocol_observation(event, &info, NULL));
      REQUIRE(info.present && info.packet_id_present && info.generation_present);
      if (!identifier)
        identifier = info.packet_id;
      REQUIRE(info.packet_id == identifier);
      if (info.packet_kind == RUMQTTC_PACKET_PUBREL) {
        ++releases;
        REQUIRE(info.connection_generation == releases);
      }
      if (info.packet_kind == RUMQTTC_PACKET_PUBCOMP) {
        REQUIRE(info.connection_generation == 2 && releases == 2 && connections == 1);
        if (protocol == RUMQTTC_PROTOCOL_V5)
          REQUIRE(info.reason_present && info.reason == 0x92);
        rumqttc_event_destroy(event);
        break;
      }
    } else
      REQUIRE(kind == RUMQTTC_EVENT_CONNECTED || kind == RUMQTTC_EVENT_DISCONNECTED);
    rumqttc_event_destroy(event);
  }
  rumqttc_completion_destroy(completion);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

int main(void) {
  rumqttc_protocol_observation_details_t invalid = RUMQTTC_PROTOCOL_OBSERVATION_DETAILS_INIT;
  invalid.present = UINT8_MAX;
  REQUIRE(rumqttc_event_protocol_observation(NULL, &invalid, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(invalid.present == 0);
  size_t count = SIZE_MAX;
  REQUIRE(rumqttc_event_protocol_observation_user_property_count(NULL, &count, NULL) == RUMQTTC_INVALID_ARGUMENT &&
          count == 0);
  REQUIRE(rumqttc_event_protocol_observation_user_property_count(NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  uint8_t present = UINT8_MAX, reason = UINT8_MAX;
  rumqttc_string_view_t text = {(const char *)(uintptr_t)1, SIZE_MAX};
  rumqttc_bytes_view_t data = {(const uint8_t *)(uintptr_t)1, SIZE_MAX};
  /* ERROR_OUT_FAILURE: rumqttc_event_protocol_observation_result_count */
  REQUIRE(rumqttc_event_protocol_observation_result_count(NULL, &present, &count, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(present == 0 && count == 0);
  /* ERROR_OUT_FAILURE: rumqttc_event_protocol_observation_result_at */
  REQUIRE(rumqttc_event_protocol_observation_result_at(NULL, 0, &reason, NULL) == RUMQTTC_INVALID_ARGUMENT &&
          reason == 0);
  /* ERROR_OUT_FAILURE: rumqttc_event_protocol_observation_user_property_count */
  REQUIRE(rumqttc_event_protocol_observation_user_property_count(NULL, &count, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_FAILURE: rumqttc_event_protocol_observation_reason_string */
  REQUIRE(rumqttc_event_protocol_observation_reason_string(NULL, &present, &text, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(present == 0 && text.data == NULL && text.len == 0);
  /* ERROR_OUT_FAILURE: rumqttc_event_protocol_observation_authentication_method */
  REQUIRE(rumqttc_event_protocol_observation_authentication_method(NULL, &present, &text, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(present == 0 && text.data == NULL && text.len == 0);
  /* ERROR_OUT_FAILURE: rumqttc_event_protocol_observation_authentication_data */
  REQUIRE(rumqttc_event_protocol_observation_authentication_data(NULL, &present, &data, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(present == 0 && data.data == NULL && data.len == 0);
  for (uint32_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol)
    for (int coarse = 0; coarse < 2; ++coarse)
      for (int precise = 0; precise < 2; ++precise)
        for (int controls = 0; controls < 2; ++controls)
          run(protocol, coarse, precise, controls);
  inbound(RUMQTTC_PROTOCOL_V4, 0);
  inbound(RUMQTTC_PROTOCOL_V5, 0);
  inbound(RUMQTTC_PROTOCOL_V4, 1);
  inbound(RUMQTTC_PROTOCOL_V5, 1);
  recovery(RUMQTTC_PROTOCOL_V4);
  recovery(RUMQTTC_PROTOCOL_V5);
  return 0;
}
