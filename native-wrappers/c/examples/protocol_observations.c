#include "example_common.h"
#include <stdio.h>
#include <stdlib.h>

/* Explicit diagnostics: continue draining the bounded queue while operations run.
 * Outgoing events describe scheduling; completion remains the outcome authority. */
static int observe(rumqttc_client_t *client) {
  rumqttc_publish_options_t options = example_publish_options(RUMQTTC_QOS_2);
  rumqttc_completion_t *completion = NULL;
  if (rumqttc_client_publish_tracked(client, example_string("rumqttc/observations"), example_bytes("hello", 5),
                                     &options, &completion, NULL) != RUMQTTC_OK)
    return 1;
  int finished = 0, failed = 0;
  while (!finished && !failed) {
    rumqttc_event_t *event = NULL;
    uint32_t kind = 0;
    if (rumqttc_client_event_recv_timeout_ms(client, 5000, &event, NULL) != RUMQTTC_OK) {
      failed = 1;
      break;
    }
    if (rumqttc_event_kind(event, &kind) != RUMQTTC_OK)
      failed = 1;
    if (kind == RUMQTTC_EVENT_OUTGOING || kind == RUMQTTC_EVENT_PROTOCOL_OBSERVATION) {
      rumqttc_protocol_observation_details_t info = RUMQTTC_PROTOCOL_OBSERVATION_DETAILS_INIT;
      if (rumqttc_event_protocol_observation(event, &info, NULL) != RUMQTTC_OK)
        failed = 1;
      else if (info.present) {
        printf("direction=%u packet=%u id_present=%u id=%u\n", (unsigned)info.direction, (unsigned)info.packet_kind,
               (unsigned)info.packet_id_present, (unsigned)info.packet_id);
        finished = info.direction == RUMQTTC_DIRECTION_INCOMING &&
                   (info.packet_kind == RUMQTTC_PACKET_PUBCOMP ||
                    (info.packet_kind == RUMQTTC_PACKET_PUBREC && info.reason_present && info.reason >= 0x80));
      }
    } else if (kind == RUMQTTC_EVENT_DRIVER_TERMINATED || kind == RUMQTTC_EVENT_DISCONNECTED)
      failed = 1;
    rumqttc_event_destroy(event);
  }
  if (!failed)
    failed = example_wait(completion, RUMQTTC_COMPLETION_QOS2_COMPLETED);
  rumqttc_completion_destroy(completion);
  return failed;
}

int main(int argc, char **argv) {
  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  int failed = rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL) != RUMQTTC_OK;
  if (!failed)
    failed = rumqttc_config_set_broker(config, example_string(argv[1]), (uint16_t)strtoul(argv[2], NULL, 10), NULL) !=
             RUMQTTC_OK;
  if (!failed)
    failed = rumqttc_config_set_client_id(config, example_string("c-protocol-observations"), NULL) != RUMQTTC_OK;
  if (!failed)
    failed = rumqttc_config_set_protocol_observations(
                 config, RUMQTTC_OBSERVE_INCOMING_CONTROL | RUMQTTC_OBSERVE_PRECISE_OUTGOING, NULL) != RUMQTTC_OK;
  if (!failed)
    failed = rumqttc_client_start(config, &client, NULL) != RUMQTTC_OK;
  rumqttc_config_destroy(config);
  if (!failed) {
    rumqttc_event_t *connected = example_next_event(client, RUMQTTC_EVENT_CONNECTED);
    failed = connected == NULL;
    rumqttc_event_destroy(connected);
  }
  if (!failed)
    failed = observe(client);
  if (client) {
    (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
    example_destroy_client(&client);
  }
  return failed;
}
