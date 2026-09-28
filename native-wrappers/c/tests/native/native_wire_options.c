#include "native_common.h"

#include <string.h>

static void expect_connected(rumqttc_client_t *client) {
  for (unsigned attempt = 0; attempt < 10; ++attempt) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    rumqttc_event_destroy(event);
    if (kind == RUMQTTC_EVENT_CONNECTED)
      return;
    REQUIRE(kind != RUMQTTC_EVENT_DRIVER_TERMINATED);
  }
  REQUIRE(0);
}

static void run_case(rumqttc_protocol_t protocol, const char *client_id) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_last_will_t will = RUMQTTC_LAST_WILL_INIT;
  rumqttc_v5_will_properties_t will_properties = RUMQTTC_V5_WILL_PROPERTIES_INIT;
  rumqttc_v5_connect_properties_t connect_properties = RUMQTTC_V5_CONNECT_PROPERTIES_INIT;
  char topic[] = "native/will";
  uint8_t payload[] = {0, 0xff, 0};
  char first_key[] = "a";
  char first_value[] = "1";
  char second_value[] = "2";
  rumqttc_user_property_t user_properties[] = {
      RUMQTTC_USER_PROPERTY_INIT,
      RUMQTTC_USER_PROPERTY_INIT,
  };
  user_properties[0].name = native_string(first_key);
  user_properties[0].value = native_string(first_value);
  user_properties[1].name = native_string(first_key);
  user_properties[1].value = native_string(second_value);
  will.topic = native_string(topic);
  will.payload = native_bytes(payload, sizeof(payload));
  will.qos = RUMQTTC_QOS_1;
  will.retain = 1;
  if (protocol == RUMQTTC_PROTOCOL_V5) {
    will.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
    will.v5_properties = &will_properties;
    will_properties.will_delay_present = 1;
    will_properties.content_type_present = 1;
    will_properties.content_type = native_string("");
    will_properties.correlation_data_present = 1;
    will_properties.correlation_data = native_bytes(NULL, 0);
    will_properties.user_properties = user_properties;
    will_properties.user_property_count = 2;
    connect_properties.receive_maximum_present = 1;
    connect_properties.receive_maximum = 7;
    connect_properties.request_response_info_present = 1;
    connect_properties.request_response_information = 1;
    connect_properties.user_properties = user_properties;
    connect_properties.user_property_count = 2;
  }
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  REQUIRE(rumqttc_config_set_last_will(config, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  uint32_t saved_protocol_options = will.protocol_options;
  will.protocol_options = UINT32_MAX;
  REQUIRE(rumqttc_config_set_last_will(config, &will, NULL) == RUMQTTC_INVALID_ARGUMENT);
  will.protocol_options = saved_protocol_options;
  will.qos = UINT32_MAX;
  REQUIRE(rumqttc_config_set_last_will(config, &will, NULL) == RUMQTTC_INVALID_ARGUMENT);
  will.qos = RUMQTTC_QOS_1;
  will.retain = 2;
  REQUIRE(rumqttc_config_set_last_will(config, &will, NULL) == RUMQTTC_INVALID_ARGUMENT);
  will.retain = 1;
  will.payload.data = NULL;
  REQUIRE(rumqttc_config_set_last_will(config, &will, NULL) == RUMQTTC_INVALID_ARGUMENT);
  will.payload = native_bytes(payload, sizeof(payload));
  will.struct_size--;
  REQUIRE(rumqttc_config_set_last_will(config, &will, NULL) == RUMQTTC_INVALID_ARGUMENT);
  will.struct_size++;
  will.reserved[0] = 1;
  REQUIRE(rumqttc_config_set_last_will(config, &will, NULL) == RUMQTTC_INVALID_ARGUMENT);
  will.reserved[0] = 0;
  if (protocol == RUMQTTC_PROTOCOL_V5) {
    will_properties.user_properties = NULL;
    REQUIRE(rumqttc_config_set_last_will(config, &will, NULL) == RUMQTTC_INVALID_ARGUMENT);
    will_properties.user_properties = user_properties;
    will_properties.user_property_count = SIZE_MAX;
    REQUIRE(rumqttc_config_set_last_will(config, &will, NULL) == RUMQTTC_INVALID_ARGUMENT);
    will_properties.user_property_count = 2;
    will_properties.struct_size--;
    REQUIRE(rumqttc_config_set_last_will(config, &will, NULL) == RUMQTTC_INVALID_ARGUMENT);
    will_properties.struct_size++;
    REQUIRE(rumqttc_config_set_v5_connect_properties(config, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    connect_properties.receive_maximum = UINT32_MAX;
    REQUIRE(rumqttc_config_set_v5_connect_properties(config, &connect_properties, NULL) == RUMQTTC_INVALID_ARGUMENT);
    connect_properties.receive_maximum = 7;
    connect_properties.topic_alias_maximum_present = 1;
    connect_properties.topic_alias_maximum = UINT32_MAX;
    REQUIRE(rumqttc_config_set_v5_connect_properties(config, &connect_properties, NULL) == RUMQTTC_INVALID_ARGUMENT);
    connect_properties.topic_alias_maximum_present = 0;
    connect_properties.topic_alias_maximum = 0;
    connect_properties.user_property_count = SIZE_MAX;
    REQUIRE(rumqttc_config_set_v5_connect_properties(config, &connect_properties, NULL) == RUMQTTC_INVALID_ARGUMENT);
    connect_properties.user_property_count = 2;
    connect_properties.struct_size--;
    REQUIRE(rumqttc_config_set_v5_connect_properties(config, &connect_properties, NULL) == RUMQTTC_INVALID_ARGUMENT);
    connect_properties.struct_size++;
    connect_properties.reserved[0] = 1;
    REQUIRE(rumqttc_config_set_v5_connect_properties(config, &connect_properties, NULL) == RUMQTTC_INVALID_ARGUMENT);
    connect_properties.reserved[0] = 0;
    connect_properties.user_properties = NULL;
    REQUIRE(rumqttc_config_set_v5_connect_properties(config, &connect_properties, NULL) == RUMQTTC_INVALID_ARGUMENT);
    connect_properties.user_properties = user_properties;
  }
  CHECK(rumqttc_config_set_last_will(config, &will, NULL));
  if (protocol == RUMQTTC_PROTOCOL_V5)
    CHECK(rumqttc_config_set_v5_connect_properties(config, &connect_properties, NULL));
  memset(topic, 'x', sizeof(topic) - 1);
  memset(payload, 'x', sizeof(payload));
  first_key[0] = 'x';
  first_value[0] = 'x';
  second_value[0] = 'x';
  CHECK(rumqttc_client_start(config, &client, NULL));
  expect_connected(client);
  native_close_destroy(client);
  CHECK(rumqttc_config_clear_last_will(config, NULL));
  if (protocol == RUMQTTC_PROTOCOL_V5)
    CHECK(rumqttc_config_clear_v5_connect_properties(config, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  expect_connected(client);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

int main(void) {
  run_case(RUMQTTC_PROTOCOL_V4, "native-wire-options-v4");
  run_case(RUMQTTC_PROTOCOL_V5, "native-wire-options-v5");
  return 0;
}
