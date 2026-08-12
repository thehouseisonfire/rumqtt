#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_event_t *event = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_subscription_t subscription = RUMQTTC_SUBSCRIPTION_INIT;
  int result = 1;
  if (argc != 3)
    return 2;
  client = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10),
                           "c-manual-ack", RUMQTTC_ACK_MANUAL);
  if (client == NULL)
    goto cleanup;
  subscription.filter = example_string("rumqttc/native/incoming");
  subscription.qos = RUMQTTC_QOS_1;
  if (example_report(rumqttc_client_subscribe_tracked(client, &subscription, 1,
                                                      &completion, &error),
                     &error, "subscribe") ||
      example_wait(completion, RUMQTTC_COMPLETION_SUBSCRIBE))
    goto cleanup;
  rumqttc_completion_destroy(completion);
  completion = NULL;
  event = example_next_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
  if (event == NULL)
    goto cleanup;
  if (example_report(rumqttc_client_acknowledge_tracked(client, event,
                                                        &completion, &error),
                     &error, "acknowledge") ||
      example_wait(completion, RUMQTTC_COMPLETION_ACKNOWLEDGED))
    goto cleanup;
  puts("manual acknowledgement completed");
  result = 0;
cleanup:
  rumqttc_completion_destroy(completion);
  rumqttc_event_destroy(event);
  if (client != NULL)
    (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
  example_destroy_client(&client);
  rumqttc_error_destroy(error);
  return result;
}
