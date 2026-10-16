#include "native_common.h"

#include <stdio.h>
#include <string.h>

static void metadata(const rumqttc_event_t *event, uint16_t expected_id, uint64_t expected_generation) {
  /* Exercise every supported output combination against actual wire input. */
  for (unsigned mask = 1; mask <= 3; ++mask) {
    uint8_t present = UINT8_MAX;
    uint16_t packet_id = UINT16_MAX;
    uint64_t generation = UINT64_MAX;
    CHECK(rumqttc_event_publish_packet_id(event, mask & 1 ? &present : NULL, mask & 2 ? &packet_id : NULL));
    if (mask & 1)
      REQUIRE(present == (expected_id != 0));
    if (mask & 2)
      REQUIRE(packet_id == expected_id);
    present = UINT8_MAX;
    CHECK(rumqttc_event_publish_connection_generation(event, mask & 1 ? &present : NULL,
                                                     mask & 2 ? &generation : NULL));
    if (mask & 1)
      REQUIRE(present == 1);
    if (mask & 2)
      REQUIRE(generation == expected_generation);
  }
}

static rumqttc_event_t *receive(rumqttc_client_t *client, const char *payload, uint16_t packet_id,
                                uint64_t generation, unsigned qos, int duplicate, int manual) {
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
  rumqttc_bytes_view_t bytes = {NULL, 0};
  rumqttc_qos_t observed_qos = UINT32_MAX;
  uint8_t observed_duplicate = UINT8_MAX, available = UINT8_MAX;
  CHECK(rumqttc_event_publish(event, NULL, &bytes, &observed_qos, NULL, &observed_duplicate, &available));
  REQUIRE(bytes.len == strlen(payload) && memcmp(bytes.data, payload, bytes.len) == 0);
  REQUIRE(observed_qos == qos && observed_duplicate == duplicate);
  REQUIRE(available == (manual && qos != 0));
  metadata(event, packet_id, generation);
  return event;
}

static void acknowledge(rumqttc_client_t *client, rumqttc_event_t *event, int manual) {
  if (manual) {
    rumqttc_completion_t *completion = NULL;
    CHECK(rumqttc_client_acknowledge_tracked(client, event, &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_ACKNOWLEDGED);
    rumqttc_completion_destroy(completion);
    uint8_t available = UINT8_MAX;
    CHECK(rumqttc_event_publish(event, NULL, NULL, NULL, NULL, NULL, &available));
    REQUIRE(available == 0);
    uint64_t operation = UINT64_MAX;
    REQUIRE(rumqttc_client_try_acknowledge(client, event, &operation, NULL) == RUMQTTC_INVALID_STATE);
    REQUIRE(operation == 0);
  }
}

static void exercise(rumqttc_protocol_t protocol, int manual) {
  char name[80], barrier[96];
  REQUIRE(snprintf(name, sizeof(name), "native-incoming-publish-%u-%d", protocol, manual) > 0);
  rumqttc_client_t *client = native_start_client(protocol, name,
      manual ? RUMQTTC_ACK_MANUAL : RUMQTTC_ACK_AUTOMATIC, 8, 16, NATIVE_DEADLINE_MS);
  rumqttc_event_t *zero = receive(client, "zero", 0, 1, 0, 0, manual);
  rumqttc_event_t *first = receive(client, "one", 0x1234, 1, 1, 0, manual);
  if (manual) {
    char foreign_name[96];
    REQUIRE(snprintf(foreign_name, sizeof(foreign_name), "native-incoming-foreign-%u", protocol) > 0);
    rumqttc_client_t *foreign = native_start_client(protocol, foreign_name, RUMQTTC_ACK_MANUAL,
                                                   8, 8, NATIVE_DEADLINE_MS);
    uint64_t operation = UINT64_MAX;
    /* Foreign/stale token admission uses the existing INVALID_ARGUMENT status;
     * an event whose token was consumed uses INVALID_STATE. */
    REQUIRE(rumqttc_client_try_acknowledge(foreign, first, &operation, NULL) == RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(operation == 0);
    native_close_destroy(foreign);
  }
  acknowledge(client, first, manual);
  metadata(first, 0x1234, 1);
  rumqttc_event_t *repeated = receive(client, "one", 0x1234, 1, 1, 1, manual);
  acknowledge(client, repeated, manual);
  rumqttc_event_t *reused = receive(client, "reuse", 0x1234, 1, 1, 0, manual);
  acknowledge(client, reused, manual);
  rumqttc_event_t *two = receive(client, "two", UINT16_MAX, 1, 2, 0, manual);
  acknowledge(client, two, manual);
  /* The duplicate QoS 2 PUBLISH is acknowledged by native state and suppressed. */
  rumqttc_event_t *done = receive(client, "done", 0, 1, 0, 0, manual);
  rumqttc_event_t *stale = receive(client, "stale", 0x1234, 1, 1, 0, manual);
  REQUIRE(snprintf(barrier, sizeof(barrier), "%s-drop", name) > 0);
  native_fixture_write(barrier, 1);
  rumqttc_event_t *current = receive(client, "new", 0x1234, 2, 1, 0, manual);
  if (manual) {
    rumqttc_completion_t *completion = (rumqttc_completion_t *)(uintptr_t)1;
    REQUIRE(rumqttc_client_acknowledge_tracked(client, stale, &completion, NULL) == RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(completion == NULL);
  }
  metadata(stale, 0x1234, 1);
  acknowledge(client, current, manual);
  native_close_destroy(client);
  metadata(zero, 0, 1);
  metadata(first, 0x1234, 1);
  metadata(repeated, 0x1234, 1);
  metadata(reused, 0x1234, 1);
  metadata(two, UINT16_MAX, 1);
  metadata(done, 0, 1);
  metadata(stale, 0x1234, 1);
  metadata(current, 0x1234, 2);
  rumqttc_event_destroy(zero);
  rumqttc_event_destroy(first);
  rumqttc_event_destroy(repeated);
  rumqttc_event_destroy(reused);
  rumqttc_event_destroy(two);
  rumqttc_event_destroy(done);
  rumqttc_event_destroy(stale);
  rumqttc_event_destroy(current);
}

int main(void) {
  for (int manual = 0; manual <= 1; ++manual) {
    exercise(RUMQTTC_PROTOCOL_V4, manual);
    exercise(RUMQTTC_PROTOCOL_V5, manual);
  }
  return 0;
}
