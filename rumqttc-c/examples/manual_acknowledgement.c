#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>

static int subscribe(rumqttc_client_t *client) {
  rumqttc_subscription_t subscription = RUMQTTC_SUBSCRIPTION_INIT;
  rumqttc_completion_t *completion = NULL;
  rumqttc_error_t *error = NULL;
  int failed;

  subscription.filter = example_string("rumqttc/native/incoming");
  subscription.qos = RUMQTTC_QOS_1;
  failed = example_report(rumqttc_client_subscribe_tracked(
                              client, &subscription, 1, &completion, &error),
                          &error, "subscribe");
  if (!failed)
    failed = example_wait(completion, RUMQTTC_COMPLETION_SUBSCRIBE);

  rumqttc_completion_destroy(completion);
  rumqttc_error_destroy(error);
  return failed;
}

static int acknowledge_next_publish(rumqttc_client_t *client) {
  rumqttc_event_t *event =
      example_next_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
  rumqttc_completion_t *completion = NULL;
  rumqttc_error_t *error = NULL;
  int failed;

  if (event == NULL)
    return 1;

  failed = example_report(
      rumqttc_client_acknowledge_tracked(client, event, &completion, &error),
      &error, "acknowledge");
  if (!failed)
    failed = example_wait(completion, RUMQTTC_COMPLETION_ACKNOWLEDGED);

  rumqttc_completion_destroy(completion);
  rumqttc_event_destroy(event);
  rumqttc_error_destroy(error);
  return failed;
}

int main(int argc, char **argv) {
  rumqttc_client_t *client;
  int result;

  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }

  client = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10),
                           "c-manual-ack", RUMQTTC_ACK_MANUAL);
  if (client == NULL)
    return 1;

  result = subscribe(client) || acknowledge_next_publish(client);
  if (result == 0)
    puts("manual acknowledgement completed");

  (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
  example_destroy_client(&client);
  return result;
}
