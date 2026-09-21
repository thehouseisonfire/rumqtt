#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_event_t *event = NULL;
  rumqttc_error_t *error = NULL;
  int result = 1;
  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }

  if (example_report(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, &error),
                     &error, "config_new") ||
      example_report(rumqttc_config_set_broker(
                         config, example_string(argv[1]),
                         (uint16_t)strtoul(argv[2], NULL, 10), &error),
                     &error, "set_broker") ||
      example_report(rumqttc_config_set_client_id(
                         config, example_string("c-resource-limits"), &error),
                     &error, "set_client_id") ||
      example_report(rumqttc_config_set_max_request_batch(config, 16, &error),
                     &error, "set_max_request_batch") ||
      example_report(rumqttc_config_set_read_batch_size(config, 16, &error),
                     &error, "set_read_batch_size") ||
      example_report(rumqttc_config_set_local_incoming_packet_limit_bytes(
                         config, 64 * 1024, &error),
                     &error, "set_local_incoming_packet_limit_bytes") ||
      example_report(rumqttc_config_set_v5_advertised_max_packet_size_bytes(
                         config, 64 * 1024, &error),
                     &error, "set_v5_advertised_max_packet_size_bytes") ||
      example_report(rumqttc_config_set_v5_outgoing_inflight_upper_limit(
                         config, 32, &error),
                     &error, "set_v5_outgoing_inflight_upper_limit") ||
      example_report(rumqttc_client_start(config, &client, &error), &error,
                     "client_start"))
    goto cleanup;

  event = example_next_event(client, RUMQTTC_EVENT_CONNECTED);
  if (event == NULL)
    goto cleanup;
  rumqttc_event_destroy(event);
  event = NULL;

  if (example_report(rumqttc_client_close_timeout_ms(client, 5000, &error),
                     &error, "close"))
    goto cleanup;
  result = 0;

cleanup:
  rumqttc_event_destroy(event);
  rumqttc_config_destroy(config);
  rumqttc_error_destroy(error);
  example_destroy_client(&client);
  return result;
}
