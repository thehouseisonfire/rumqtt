#include "native_common.h"

#include <string.h>

#define EVENT_COUNT 512u

static void check_payload(const rumqttc_event_t *event, unsigned index) {
  rumqttc_string_view_t topic = {NULL, 0};
  rumqttc_bytes_view_t payload = {NULL, 0};
  const uint8_t expected[] = {
      (uint8_t)(index >> 24), (uint8_t)(index >> 16), (uint8_t)(index >> 8), (uint8_t)index, 0, 0xff};
  CHECK(rumqttc_event_publish(event, &topic, &payload, NULL, NULL, NULL, NULL));
  REQUIRE(topic.len == strlen("native/events/item") && memcmp(topic.data, "native/events/item", topic.len) == 0);
  REQUIRE(payload.len == sizeof(expected) && memcmp(payload.data, expected, sizeof(expected)) == 0);
}

static void run(rumqttc_protocol_t protocol, int backpressure) {
  rumqttc_client_t *client =
      native_start_client(protocol, backpressure ? "native-events-backpressure" : "native-events-retained",
                          RUMQTTC_ACK_AUTOMATIC, 8, backpressure ? 4 : EVENT_COUNT + 8, NATIVE_DEADLINE_MS);
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_0);
  rumqttc_completion_t *completion = NULL;
  rumqttc_event_t *events[EVENT_COUNT] = {NULL};
  CHECK(rumqttc_client_publish_tracked(client, native_string("native/events/burst"), native_bytes(NULL, 0), &options,
                                       &completion, NULL));
  native_wait_completion(completion, RUMQTTC_COMPLETION_QOS0_FLUSHED);
  rumqttc_completion_destroy(completion);
  unsigned count = backpressure ? 1 : EVENT_COUNT;
  for (unsigned index = 0; index < count; ++index) {
    events[index] = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
    check_payload(events[index], index);
  }
  if (backpressure) {
    /* The driver blocks on the small queue while the remaining burst arrives.
     * Its five-second event deadline is deliberately longer than close's deadline. */
    native_sleep_ms(100);
    CHECK(rumqttc_client_close_now_timeout_ms(client, 1000, NULL));
  } else {
    CHECK(rumqttc_client_close_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  }
  /* Retained packet owners survive both a full queue and driver destruction. */
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  for (unsigned index = 0; index < count; ++index) {
    check_payload(events[index], index);
    rumqttc_event_destroy(events[index]);
  }
}

int main(void) {
  for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
    run(protocol, 0);
    run(protocol, 1);
  }
  return 0;
}
