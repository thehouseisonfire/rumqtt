#include "native_common.h"

#include <string.h>

static void properties(const rumqttc_event_t *event, uint32_t property_class) {
  size_t count = 0;
  CHECK(rumqttc_event_user_property_count(event, property_class, &count));
  REQUIRE(count == 2);
  for (size_t index = 0; index < count; ++index) {
    rumqttc_string_view_t key = {NULL, 0}, value = {NULL, 0};
    CHECK(rumqttc_event_user_property_at(event, property_class, index, &key, &value));
    REQUIRE(key.len == 1 && key.data[0] == 'k');
    REQUIRE(value.len == (index == 0 ? 1 : 0));
    if (index == 0)
      REQUIRE(value.data[0] == 'v');
  }
}

int main(void) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_event_t *connected, *disconnected = NULL;
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *completion = NULL;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(""), NULL));
  rumqttc_v5_connect_properties_t connect_properties = RUMQTTC_V5_CONNECT_PROPERTIES_INIT;
  connect_properties.request_response_info_present = 1;
  connect_properties.request_response_information = 1;
  CHECK(rumqttc_config_set_v5_connect_properties(config, &connect_properties, NULL));
  CHECK(rumqttc_config_set_emit_outgoing_events(config, 1, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  connected = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  const uint64_t values[] = {0, 7, 1, 0, 1024, 2, 0, 1, 0, 30};
  for (uint32_t selector = 1; selector <= 10; ++selector) {
    uint8_t present = 0;
    uint64_t value = UINT64_MAX;
    CHECK(rumqttc_event_connack_v5_scalar(connected, selector, &present, &value));
    REQUIRE(present == 1 && value == values[selector - 1]);
  }
  const char *texts[] = {"assigned-native", "", "", "localhost:1883"};
  rumqttc_string_view_t borrowed = {NULL, 0};
  char copy[32];
  size_t written = 0;
  for (uint32_t selector = 1; selector <= 5; ++selector) {
    uint8_t present = UINT8_MAX;
    rumqttc_string_view_t text = {(const char *)(uintptr_t)1, SIZE_MAX};
    CHECK(rumqttc_event_connack_v5_string(connected, selector, &present, &text));
    REQUIRE(present == (selector <= 4));
    if (present) {
      REQUIRE(text.len == strlen(texts[selector - 1]));
      REQUIRE(text.len == 0 || memcmp(text.data, texts[selector - 1], text.len) == 0);
    } else {
      REQUIRE(text.data == NULL && text.len == 0);
    }
    if (selector == RUMQTTC_CONNACK_STRING_SERVER_REFERENCE)
      borrowed = text;
  }
  CHECK(rumqttc_string_copy(borrowed, copy, sizeof(copy), &written));
  REQUIRE(written == borrowed.len);
  {
    uint8_t present = UINT8_MAX;
    rumqttc_bytes_view_t data = {(const uint8_t *)(uintptr_t)1, SIZE_MAX};
    CHECK(rumqttc_event_connack_v5_authentication_data(connected, &present, &data));
    REQUIRE(present == 0 && data.data == NULL && data.len == 0);
  }
  properties(connected, RUMQTTC_EVENT_PROPERTIES_CONNACK);
  CHECK(rumqttc_client_publish_tracked(client, native_string("a"), native_bytes(NULL, 0), &options, &completion, NULL));
  unsigned saw_outgoing = 0;
  for (unsigned i = 0; i < 12 && disconnected == NULL; ++i) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    native_check_event_accessors(event);
    if (kind == RUMQTTC_EVENT_OUTGOING) {
      uint32_t activity = 0;
      uint8_t present = 0;
      uint16_t packet_id = 0;
      CHECK(rumqttc_event_outgoing_kind(event, &activity));
      CHECK(rumqttc_event_outgoing_packet_id(event, &present, &packet_id));
      if (activity == RUMQTTC_OUTGOING_PUBLISH) {
        REQUIRE(present && packet_id != 0);
        ++saw_outgoing;
      }
    } else if (kind == RUMQTTC_EVENT_BROKER_DISCONNECT) {
      disconnected = event;
      continue;
    }
    rumqttc_event_destroy(event);
  }
  REQUIRE(disconnected != NULL && saw_outgoing == 1);
  native_wait_completion(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  rumqttc_completion_destroy(completion);
  uint8_t reason = 0, expiry_present = 0, reason_present = 0, reference_present = 0;
  uint32_t expiry = UINT32_MAX;
  rumqttc_string_view_t reason_text = {NULL, 0}, reference = {NULL, 0};
  CHECK(rumqttc_event_broker_disconnect(disconnected, &reason, &expiry_present, &expiry, &reason_present, &reason_text,
                                        &reference_present, &reference));
  REQUIRE(reason == 0x80 && expiry_present && expiry == 0 && reason_present && reason_text.len == 0);
  REQUIRE(reference_present && reference.len == 0);
  properties(disconnected, RUMQTTC_EVENT_PROPERTIES_BROKER_DISCONNECT);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
  REQUIRE(borrowed.len == written && memcmp(borrowed.data, copy, written) == 0);
  properties(connected, RUMQTTC_EVENT_PROPERTIES_CONNACK);
  properties(disconnected, RUMQTTC_EVENT_PROPERTIES_BROKER_DISCONNECT);
  rumqttc_event_destroy(connected);
  rumqttc_event_destroy(disconnected);
  REQUIRE(written == strlen("localhost:1883") && memcmp(copy, "localhost:1883", written) == 0);
  return 0;
}
