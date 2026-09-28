#include "native_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void source(rumqttc_protocol_t protocol, int abrupt) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_last_will_t will = RUMQTTC_LAST_WILL_INIT;
  rumqttc_v5_will_properties_t properties = RUMQTTC_V5_WILL_PROPERTIES_INIT;
  rumqttc_user_property_t user_properties[2] = {RUMQTTC_USER_PROPERTY_INIT, RUMQTTC_USER_PROPERTY_INIT};
  const uint8_t payload[] = {0, 0xff, 0};
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-will-source"), NULL));
  will.topic = native_string("native/will/process");
  will.payload = native_bytes(payload, sizeof(payload));
  will.qos = RUMQTTC_QOS_1;
  if (protocol == RUMQTTC_PROTOCOL_V5) {
    will.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
    will.v5_properties = &properties;
    properties.will_delay_present = 1;
    properties.content_type_present = 1;
    properties.content_type = native_string("");
    properties.correlation_data_present = 1;
    properties.correlation_data = native_bytes(NULL, 0);
    user_properties[0].name = user_properties[1].name = native_string("k");
    user_properties[0].value = native_string("one");
    user_properties[1].value = native_string("");
    properties.user_properties = user_properties;
    properties.user_property_count = 2;
  }
  CHECK(rumqttc_config_set_last_will(config, &will, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(event);
  if (abrupt)
    _Exit(0);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

static void observer(rumqttc_protocol_t protocol, int abrupt) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-will-observer"), NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_t *connected = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(connected);
  rumqttc_config_destroy(config);
  rumqttc_subscription_t subscription = native_subscription("native/will/process", RUMQTTC_QOS_1);
  rumqttc_subscribe_options_t subscribe = RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
  rumqttc_completion_t *completion = NULL;
  CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, &subscribe, &completion, NULL));
  native_wait_completion(completion, RUMQTTC_COMPLETION_SUBSCRIBE);
  rumqttc_completion_destroy(completion);
  FILE *ready = fopen(getenv("RUMQTTC_TEST_READY_PATH"), "wb");
  REQUIRE(ready != NULL && fclose(ready) == 0);
  unsigned released = 0;
  for (unsigned attempt = 0; attempt < 1000; ++attempt) {
    FILE *release = fopen(getenv("RUMQTTC_TEST_RELEASE_PATH"), "rb");
    if (release != NULL) {
      REQUIRE(fclose(release) == 0);
      released = 1;
      break;
    }
    native_sleep_ms(5);
  }
  REQUIRE(released);
  rumqttc_event_t *event = NULL;
  rumqttc_status_t status =
      rumqttc_client_event_recv_timeout_ms(client, abrupt ? NATIVE_DEADLINE_MS : 250, &event, NULL);
  if (abrupt) {
    CHECK(status);
    rumqttc_string_view_t topic = {NULL, 0};
    rumqttc_bytes_view_t payload = {NULL, 0};
    rumqttc_qos_t qos = 0;
    CHECK(rumqttc_event_publish(event, &topic, &payload, &qos, NULL, NULL, NULL));
    REQUIRE(topic.len == strlen("native/will/process") && memcmp(topic.data, "native/will/process", topic.len) == 0);
    REQUIRE(payload.len == 3 && payload.data[0] == 0 && payload.data[1] == 0xff && payload.data[2] == 0);
    REQUIRE(qos == RUMQTTC_QOS_1);
    if (protocol == RUMQTTC_PROTOCOL_V5) {
      uint8_t present = 0;
      rumqttc_string_view_t text = {NULL, 0};
      rumqttc_bytes_view_t correlation = {NULL, 0};
      CHECK(rumqttc_event_v5_content_type(event, &present, &text));
      REQUIRE(present == 1 && text.len == 0);
      CHECK(rumqttc_event_v5_correlation_data(event, &present, &correlation));
      REQUIRE(present == 1 && correlation.len == 0);
      size_t count = 0;
      CHECK(rumqttc_event_v5_user_property_count(event, &count));
      REQUIRE(count == 2);
      for (size_t index = 0; index < count; ++index) {
        rumqttc_string_view_t name, value;
        CHECK(rumqttc_event_v5_user_property_at(event, index, &name, &value));
        REQUIRE(name.len == 1 && name.data[0] == 'k');
        REQUIRE(value.len == (index == 0 ? 3 : 0));
        if (index == 0)
          REQUIRE(memcmp(value.data, "one", 3) == 0);
      }
    }
    rumqttc_event_destroy(event);
  } else {
    REQUIRE(status == RUMQTTC_TIMEOUT && event == NULL);
  }
  native_close_destroy(client);
}

int main(int argc, char **argv) {
  REQUIRE(argc == 4);
  rumqttc_protocol_t protocol = strcmp(argv[2], "v4") == 0 ? RUMQTTC_PROTOCOL_V4 : RUMQTTC_PROTOCOL_V5;
  int abrupt = strcmp(argv[3], "abrupt") == 0;
  if (strcmp(argv[1], "source") == 0)
    source(protocol, abrupt);
  else {
    REQUIRE(strcmp(argv[1], "observer") == 0);
    observer(protocol, abrupt);
  }
  return 0;
}
