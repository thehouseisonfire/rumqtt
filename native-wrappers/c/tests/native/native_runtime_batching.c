#include "native_common.h"

#include <stdio.h>
#include <string.h>

static void batching(rumqttc_protocol_t protocol, int reads, unsigned batch) {
  char id[96], admitted[112], measured[112], released[112];
  REQUIRE(snprintf(id, sizeof(id), "native-batch-%u-%s-%u", protocol, reads ? "read" : "request", batch) > 0);
  REQUIRE(snprintf(admitted, sizeof(admitted), "%s-admitted", id) > 0);
  REQUIRE(snprintf(measured, sizeof(measured), "%s-measured", id) > 0);
  REQUIRE(snprintf(released, sizeof(released), "%s-released", id) > 0);
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *pending[12];
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_0);
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_request_capacity(config, 64, NULL));
  CHECK(rumqttc_config_set_event_capacity(config, 1, NULL));
  CHECK(rumqttc_config_set_event_delivery_timeout_ms(config, 2 * NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_config_set_emit_outgoing_events(config, !reads, NULL));
  CHECK(rumqttc_config_set_max_request_batch(config, reads ? 0 : batch, NULL));
  CHECK(rumqttc_config_set_read_batch_size(config, reads ? batch : 1, NULL));
  /* Adaptive reads with an outgoing window of sixteen select eight packets. */
  if (protocol == RUMQTTC_PROTOCOL_V4)
    CHECK(rumqttc_config_set_v4_inflight_limit(config, 16, NULL));
  else
    CHECK(rumqttc_config_set_v5_outgoing_inflight_upper_limit(config, 16, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  if (!reads) {
    /* The broker withholds CONNACK until all twelve requests have been admitted. */
    for (uint8_t index = 0; index < 12; ++index)
      CHECK(rumqttc_client_publish_tracked(client, native_string("batch"), native_bytes(&index, 1), &options,
                                           &pending[index], NULL));
  }
  native_fixture_write(admitted, 1);
  unsigned expected = batch ? batch : reads ? 8 : 1;
  /* Connected fills the event queue. Delivery of the first batch event parks the driver;
     broker wire counts therefore measure a single batch, independently of TCP segmentation. */
  REQUIRE(native_fixture_read(measured) == expected);
  native_fixture_write(released, 1);
  rumqttc_event_t *event = NULL;
  uint32_t kind = 0;
  CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
  CHECK(rumqttc_event_kind(event, &kind));
  REQUIRE(kind == RUMQTTC_EVENT_CONNECTED);
  native_check_event_accessors(event);
  rumqttc_event_destroy(event);
  for (uint8_t index = 0; index < 12; ++index) {
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    native_check_event_accessors(event);
    if (reads) {
      rumqttc_bytes_view_t payload = {NULL, 0};
      REQUIRE(kind == RUMQTTC_EVENT_INCOMING_PUBLISH);
      CHECK(rumqttc_event_publish(event, NULL, &payload, NULL, NULL, NULL, NULL));
      REQUIRE(payload.len == 1 && payload.data[0] == index);
    } else {
      uint32_t outgoing = 0;
      REQUIRE(kind == RUMQTTC_EVENT_OUTGOING);
      CHECK(rumqttc_event_outgoing_kind(event, &outgoing));
      REQUIRE(outgoing == RUMQTTC_OUTGOING_PUBLISH);
    }
    rumqttc_event_destroy(event);
  }
  if (!reads) {
    for (unsigned index = 0; index < 12; ++index) {
      native_wait_completion(pending[index], RUMQTTC_COMPLETION_QOS0_FLUSHED);
      rumqttc_completion_destroy(pending[index]);
    }
  }
  REQUIRE(native_fixture_read(id) == 12);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

int main(void) {
  const unsigned batches[] = {0, 1, 8};
  for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol)
    for (unsigned reads = 0; reads < 2; ++reads)
      for (size_t index = 0; index < sizeof(batches) / sizeof(batches[0]); ++index)
        batching(protocol, (int)reads, batches[index]);
  return 0;
}
