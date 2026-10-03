#include "native_common.h"

#include <string.h>

static void rejected(rumqttc_client_t *client, rumqttc_event_t *event,
                     const rumqttc_acknowledgement_options_t *options) {
  rumqttc_completion_t *completion = (rumqttc_completion_t *)(uintptr_t)1;
  uint64_t operation = UINT64_MAX;
  uint8_t available = 0;
  REQUIRE(rumqttc_client_try_acknowledge_with_options(
              client, event, options, &operation, NULL) != RUMQTTC_OK);
  REQUIRE(operation == 0);
  REQUIRE(rumqttc_client_acknowledge_with_options_tracked(
              client, event, options, &completion, NULL) != RUMQTTC_OK);
  REQUIRE(completion == NULL);
  CHECK(rumqttc_event_publish(event, NULL, NULL, NULL, NULL, NULL, &available));
  REQUIRE(available == 1);
}

static void exercise(rumqttc_protocol_t protocol) {
  const uint32_t reasons[] = {0, 0x80, 0x83, 0x87, 0x90, 0x91, 0x97, 0x99};
  rumqttc_client_t *client = native_start_client(
      protocol,
      protocol == RUMQTTC_PROTOCOL_V5 ? "native-ack-options-v5"
                                      : "native-ack-options-v4",
      RUMQTTC_ACK_MANUAL, 4, 8, 5000);
  rumqttc_event_t *retained = NULL;
  for (unsigned qos = 1; qos <= 2; ++qos) {
    unsigned count = protocol == RUMQTTC_PROTOCOL_V5 ? 8 : 1;
    for (unsigned index = 0; index < count; ++index) {
      rumqttc_event_t *event =
          native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
      rumqttc_v5_acknowledgement_options_t content =
          RUMQTTC_V5_ACKNOWLEDGEMENT_OPTIONS_INIT;
      rumqttc_acknowledgement_options_t options =
          RUMQTTC_ACKNOWLEDGEMENT_OPTIONS_INIT;
      rumqttc_user_property_t properties[2] = {RUMQTTC_USER_PROPERTY_INIT,
                                               RUMQTTC_USER_PROPERTY_INIT};
      rumqttc_completion_t *completion = NULL;
      uint64_t operation = 0;
      options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
      options.v5_options = &content;
      if (protocol == RUMQTTC_PROTOCOL_V4) {
        /* Explicit V5 defaults must fail, then the same event must accept
         * neutral defaults. */
        rejected(client, event, &options);
        options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL;
        options.v5_options = NULL;
      } else {
        content.reason_code = 0x10;
        rejected(client, event, &options);
        content.reason_code = 0xff;
        rejected(client, event, &options);
        content.reason_code = reasons[index];
        content.reserved[6] = 1;
        rejected(client, event, &options);
        content.reserved[6] = 0;
        content.reason_string_present = 2;
        rejected(client, event, &options);
        content.reason_string_present = 1;
        content.reason_string =
            native_string("this diagnostic makes the ACK larger than 32 bytes");
        rejected(client, event, &options);
        content.reason_string_present = index % 2;
        content.reason_string = native_string("");
        if (index % 2) {
          properties[0].name = native_string("k");
          properties[0].value = native_string("v");
          properties[1].name = native_string("k");
          properties[1].value = native_string("");
          content.user_properties = properties;
          content.user_property_count = 2;
        }
      }
      if (index % 2) {
        CHECK(rumqttc_client_try_acknowledge_with_options(
            client, event, &options, &operation, NULL));
        REQUIRE(operation != 0);
      } else {
        CHECK(rumqttc_client_acknowledge_with_options_tracked(
            client, event, &options, &completion, NULL));
        native_wait_completion(completion, RUMQTTC_COMPLETION_ACKNOWLEDGED);
        native_wait_completion(completion, RUMQTTC_COMPLETION_ACKNOWLEDGED);
        rumqttc_completion_destroy(completion);
      }
      /* Input option records are borrowed only for the call. */
      content.reason_code = 0x10;
      properties[0].value = native_string("overwritten");
      {
        rumqttc_bytes_view_t payload = {NULL, 0};
        uint8_t available = 1;
        CHECK(rumqttc_event_publish(event, NULL, &payload, NULL, NULL, NULL,
                                    &available));
        REQUIRE(payload.len == 1 && payload.data[0] == 'x' && available == 0);
      }
      operation = UINT64_MAX;
      REQUIRE(rumqttc_client_try_acknowledge_with_options(
                  client, event, &options, &operation, NULL) != RUMQTTC_OK);
      REQUIRE(operation == 0);
      rumqttc_event_destroy(retained);
      retained = event;
    }
  }
  /* The broker emits this only after every ACK and derived PUBCOMP reaches the
   * wire. */
  rumqttc_event_t *done =
      native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
  rumqttc_event_destroy(done);
  native_close_destroy(client);
  rumqttc_bytes_view_t payload = {NULL, 0};
  CHECK(
      rumqttc_event_publish(retained, NULL, &payload, NULL, NULL, NULL, NULL));
  REQUIRE(payload.len == 1 && payload.data[0] == 'x');
  rumqttc_event_destroy(retained);
}

int main(void) {
  exercise(RUMQTTC_PROTOCOL_V4);
  exercise(RUMQTTC_PROTOCOL_V5);
  return 0;
}
