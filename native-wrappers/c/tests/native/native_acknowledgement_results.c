#include "native_common.h"

#include <string.h>

static rumqttc_completion_t *publish(rumqttc_client_t *client, const char *topic, uint32_t qos, uint32_t status) {
  rumqttc_publish_options_t options = native_publish_options(qos);
  rumqttc_completion_t *completion = NULL;
  rumqttc_error_t *error = NULL;
  CHECK(
      rumqttc_client_publish_tracked(client, native_string(topic), native_bytes(NULL, 0), &options, &completion, NULL));
  REQUIRE(rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, &error) == status);
  if (status == RUMQTTC_BROKER_REJECTED) {
    uint8_t present = 0, reason = 0;
    CHECK(rumqttc_error_broker_reason(error, &present, &reason));
    REQUIRE(present && (reason == 0x87 || reason == 0x92));
  }
  rumqttc_error_destroy(error);
  if (status == RUMQTTC_BROKER_REJECTED) {
    error = NULL;
    REQUIRE(rumqttc_completion_poll(completion, &error) == status);
    rumqttc_error_destroy(error);
    uint32_t kind = UINT32_MAX;
    REQUIRE(rumqttc_completion_kind(completion, &kind, NULL) == status);
    REQUIRE(kind == 0);
  }
  return completion;
}

static rumqttc_acknowledgement_details_t metadata(rumqttc_completion_t *completion, uint32_t protocol, uint32_t kind) {
  rumqttc_acknowledgement_details_t details = RUMQTTC_ACKNOWLEDGEMENT_DETAILS_INIT;
  CHECK(rumqttc_completion_acknowledgement(completion, &details, NULL));
  REQUIRE(details.present && details.protocol == protocol && details.packet_kind == kind && details.packet_id != 0);
  REQUIRE(details.packet_id ==
          native_fixture_read(protocol == RUMQTTC_PROTOCOL_V5 ? "terminal-ack-id-v5" : "terminal-ack-id-v4"));
  return details;
}

static rumqttc_string_view_t properties(rumqttc_completion_t *completion, uint32_t protocol, const char *reason) {
  uint8_t present = 9;
  size_t count = 9;
  rumqttc_string_view_t view = {NULL, 0}, key = {NULL, 0}, value = {NULL, 0};
  CHECK(rumqttc_completion_acknowledgement_reason_string(completion, &present, &view, NULL));
  REQUIRE(present == (protocol == RUMQTTC_PROTOCOL_V5 && reason != NULL));
  if (present)
    REQUIRE(view.len == strlen(reason) && (view.len == 0 || memcmp(view.data, reason, view.len) == 0));
  else
    REQUIRE(view.data == NULL && view.len == 0);
  CHECK(rumqttc_completion_acknowledgement_user_property_count(completion, &count, NULL));
  REQUIRE(count == (protocol == RUMQTTC_PROTOCOL_V5 && reason != NULL && *reason != '\0' ? 2u : 0u));
  for (size_t index = 0; index < count; ++index) {
    CHECK(rumqttc_completion_acknowledgement_user_property_at(completion, index, &key, &value, NULL));
    REQUIRE(key.len == strlen("terminal-private-key") && memcmp(key.data, "terminal-private-key", key.len) == 0);
    REQUIRE(value.len == (index == 0 ? strlen("terminal-private-value") : 0));
    if (index == 0)
      REQUIRE(memcmp(value.data, "terminal-private-value", value.len) == 0);
  }
  key.data = value.data = (const char *)(uintptr_t)1;
  key.len = value.len = SIZE_MAX;
  REQUIRE(rumqttc_completion_acknowledgement_user_property_at(completion, count, &key, &value, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(key.data == NULL && key.len == 0 && value.data == NULL && value.len == 0);
  return view;
}

static void run_protocol(uint32_t protocol) {
  const int mqtt5 = protocol == RUMQTTC_PROTOCOL_V5;
  rumqttc_client_t *client = native_start_client(protocol, mqtt5 ? "native-terminal-v5" : "native-terminal-v4",
                                                 RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
  rumqttc_completion_t *retained[8] = {NULL};
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  CHECK(rumqttc_client_publish_tracked(client, native_string("pending"), native_bytes(NULL, 0), &options, &retained[0],
                                       NULL));
  REQUIRE(native_fixture_read(mqtt5 ? "terminal-pending-v5" : "terminal-pending-v4"));
  rumqttc_acknowledgement_details_t details = RUMQTTC_ACKNOWLEDGEMENT_DETAILS_INIT;
  memset((unsigned char *)&details + sizeof(details.struct_size), 0xff, sizeof(details) - sizeof(details.struct_size));
  REQUIRE(rumqttc_completion_acknowledgement(retained[0], &details, NULL) == RUMQTTC_WOULD_BLOCK);
  REQUIRE(details.present == 0 && details.packet_id == 0 && details.protocol == 0);
  REQUIRE(rumqttc_completion_wait_timeout_ms(retained[0], 0, NULL) == RUMQTTC_TIMEOUT);
  native_fixture_write(mqtt5 ? "terminal-release-v5" : "terminal-release-v4", 1);
  native_wait_completion(retained[0], RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  (void)metadata(retained[0], protocol, RUMQTTC_ACKNOWLEDGEMENT_PUBACK);
  {
    struct {
      uint32_t size;
      uint32_t guard;
    } small = {sizeof(uint32_t), 0x12345678u};
    REQUIRE(rumqttc_completion_acknowledgement(retained[0], (rumqttc_acknowledgement_details_t *)&small, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(small.size == sizeof(uint32_t) && small.guard == 0x12345678u);
    REQUIRE(rumqttc_completion_acknowledgement(retained[0], NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(rumqttc_completion_acknowledgement_reason_string(retained[0], NULL, NULL, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(rumqttc_completion_acknowledgement_user_property_count(retained[0], NULL, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(rumqttc_completion_acknowledgement_user_property_at(retained[0], 0, NULL, NULL, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(rumqttc_completion_acknowledgement_result_count(retained[0], NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(rumqttc_completion_acknowledgement_result_at(retained[0], 0, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  }
  rumqttc_string_view_t borrowed = properties(retained[0], protocol, "terminal-private-reason");
  for (unsigned index = 0; index < 3; ++index) {
    rumqttc_string_view_t again = properties(retained[0], protocol, "terminal-private-reason");
    REQUIRE(again.data == borrowed.data && again.len == borrowed.len);
  }
  retained[1] = publish(client, "matched", RUMQTTC_QOS_1, RUMQTTC_OK);
  details = metadata(retained[1], protocol, RUMQTTC_ACKNOWLEDGEMENT_PUBACK);
  REQUIRE(details.reason_present == mqtt5 && details.reason == (mqtt5 ? 0x10 : 0));
  uint8_t present = 9, reason = 9;
  size_t count = 9;
  REQUIRE(rumqttc_completion_acknowledgement_result_count(retained[1], &present, &count, NULL) ==
          RUMQTTC_INVALID_STATE);
  REQUIRE(present == 0 && count == 0);
  retained[2] = publish(client, "empty", RUMQTTC_QOS_1, RUMQTTC_OK);
  (void)properties(retained[2], protocol, "");
  retained[3] = publish(client, "absent", RUMQTTC_QOS_1, RUMQTTC_OK);
  (void)properties(retained[3], protocol, NULL);
  retained[4] = publish(client, "rejected", RUMQTTC_QOS_1, mqtt5 ? RUMQTTC_BROKER_REJECTED : RUMQTTC_OK);
  (void)metadata(retained[4], protocol, RUMQTTC_ACKNOWLEDGEMENT_PUBACK);
  (void)properties(retained[4], protocol, "terminal-private-reason");
  retained[5] = publish(client, "rejected", RUMQTTC_QOS_2, mqtt5 ? RUMQTTC_BROKER_REJECTED : RUMQTTC_OK);
  (void)metadata(retained[5], protocol, mqtt5 ? RUMQTTC_ACKNOWLEDGEMENT_PUBREC : RUMQTTC_ACKNOWLEDGEMENT_PUBCOMP);
  (void)properties(retained[5], protocol, "terminal-private-reason");
  retained[6] = publish(client, "ordinary", RUMQTTC_QOS_2, mqtt5 ? RUMQTTC_BROKER_REJECTED : RUMQTTC_OK);
  details = metadata(retained[6], protocol, RUMQTTC_ACKNOWLEDGEMENT_PUBCOMP);
  REQUIRE(!details.recovered && details.reason == (mqtt5 ? 0x92 : 0));
  (void)properties(retained[6], protocol, "terminal-private-reason");
  retained[7] = publish(client, "ordinary", RUMQTTC_QOS_0, RUMQTTC_OK);
  CHECK(rumqttc_completion_acknowledgement(retained[7], &details, NULL));
  REQUIRE(!details.present && !details.packet_id && !details.reason_present);
  REQUIRE(rumqttc_completion_acknowledgement_reason_string(retained[7], &present, &borrowed, NULL) ==
          RUMQTTC_INVALID_STATE);
  REQUIRE(!present && borrowed.data == NULL && borrowed.len == 0);
  for (unsigned operation = 0; operation < 2; ++operation) {
    rumqttc_completion_t *completion = NULL;
    rumqttc_subscription_t filters[3] = {RUMQTTC_SUBSCRIPTION_INIT, RUMQTTC_SUBSCRIPTION_INIT,
                                         RUMQTTC_SUBSCRIPTION_INIT};
    rumqttc_string_view_t names[3] = {native_string("a"), native_string("b"), native_string("c")};
    for (unsigned index = 0; index < 3; ++index) {
      filters[index].filter = names[index];
      filters[index].qos = RUMQTTC_QOS_2;
    }
    if (operation == 0)
      CHECK(rumqttc_client_subscribe_tracked(client, filters, 3, NULL, &completion, NULL));
    else
      CHECK(rumqttc_client_unsubscribe_tracked(client, names, 3, NULL, &completion, NULL));
    native_wait_completion(completion, operation == 0 ? RUMQTTC_COMPLETION_SUBSCRIBE : RUMQTTC_COMPLETION_UNSUBSCRIBE);
    details = metadata(completion, protocol,
                       operation == 0 ? RUMQTTC_ACKNOWLEDGEMENT_SUBACK : RUMQTTC_ACKNOWLEDGEMENT_UNSUBACK);
    REQUIRE(!details.reason_present);
    (void)properties(completion, protocol, "terminal-private-reason");
    CHECK(rumqttc_completion_acknowledgement_result_count(completion, &present, &count, NULL));
    REQUIRE(present == (mqtt5 || operation == 0) && count == (present ? 3u : 0u));
    for (size_t index = 0; index < count; ++index) {
      const uint8_t expected_sub[] = {1, mqtt5 ? 0x87 : 0x80, 2}, expected_unsub[] = {0, 0x11, 0x87};
      CHECK(rumqttc_completion_acknowledgement_result_at(completion, index, &reason, NULL));
      REQUIRE(reason == (operation == 0 ? expected_sub[index] : expected_unsub[index]));
    }
    REQUIRE(rumqttc_completion_acknowledgement_result_at(completion, count, &reason, NULL) ==
            (present ? RUMQTTC_INVALID_ARGUMENT : RUMQTTC_INVALID_STATE));
    REQUIRE(reason == 0);
    rumqttc_completion_destroy(completion);
  }
  native_close_destroy(client);
  (void)properties(retained[4], protocol, "terminal-private-reason");
  borrowed = properties(retained[0], protocol, "terminal-private-reason");
  char copied[64] = {0};
  size_t required = 0;
  CHECK(rumqttc_string_copy(borrowed, copied, sizeof(copied), &required));
  for (unsigned index = 0; index < 8; ++index)
    rumqttc_completion_destroy(retained[index]);
  if (mqtt5)
    REQUIRE(required == strlen("terminal-private-reason") && memcmp(copied, "terminal-private-reason", required) == 0);
}

static void recovery(void) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-terminal-recovery"), NULL));
  CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_config_destroy(config);
  rumqttc_event_t *connected = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(connected);
  rumqttc_completion_t *completion = publish(client, "recovery", RUMQTTC_QOS_2, RUMQTTC_OK);
  rumqttc_acknowledgement_details_t details =
      metadata(completion, RUMQTTC_PROTOCOL_V5, RUMQTTC_ACKNOWLEDGEMENT_PUBCOMP);
  REQUIRE(details.recovered && details.reason_present && details.reason == 0x92);
  (void)properties(completion, RUMQTTC_PROTOCOL_V5, "terminal-private-reason");
  native_close_destroy(client);
  rumqttc_completion_destroy(completion);
}

int main(void) {
  run_protocol(RUMQTTC_PROTOCOL_V4);
  run_protocol(RUMQTTC_PROTOCOL_V5);
  recovery();
  return 0;
}
