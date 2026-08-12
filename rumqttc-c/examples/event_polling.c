#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_event_t *event = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_subscription_t subscription = RUMQTTC_SUBSCRIPTION_INIT;
  rumqttc_publish_options_t options = example_publish_options(RUMQTTC_QOS_1);
  int result = 1;
  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }
  client = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10),
                           "c-event-poll", RUMQTTC_ACK_AUTOMATIC);
  if (client == NULL)
    goto cleanup;
  subscription.filter = example_string("rumqttc/native/event-example");
  subscription.qos = RUMQTTC_QOS_1;
  if (example_report(rumqttc_client_subscribe_tracked(client, &subscription, 1,
                                                      &completion, &error),
                     &error, "subscribe") ||
      example_wait(completion, RUMQTTC_COMPLETION_SUBSCRIBE))
    goto cleanup;
  rumqttc_completion_destroy(completion);
  completion = NULL;
  if (example_report(rumqttc_client_publish_tracked(
                         client, subscription.filter, example_bytes("hello", 5),
                         &options, &completion, &error),
                     &error, "publish") ||
      example_wait(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED))
    goto cleanup;
  rumqttc_completion_destroy(completion);
  completion = NULL;
  event = example_next_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
  if (event == NULL)
    goto cleanup;
  puts("received publish while polling one event at a time");
  result = 0;
cleanup:
  rumqttc_event_destroy(event);
  rumqttc_completion_destroy(completion);
  if (client != NULL)
    (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
  example_destroy_client(&client);
  rumqttc_error_destroy(error);
  return result;
}
