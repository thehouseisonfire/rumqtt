#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>

static int subscribe(rumqttc_client_t *client,
                     const rumqttc_subscription_t *subscription) {
  rumqttc_completion_t *completion = NULL;
  rumqttc_error_t *error = NULL;
  int failed = example_report(rumqttc_client_subscribe_tracked(
                                  client, subscription, 1, NULL, &completion, &error),
                              &error, "subscribe");

  if (!failed)
    failed = example_wait(completion, RUMQTTC_COMPLETION_SUBSCRIBE);

  rumqttc_completion_destroy(completion);
  rumqttc_error_destroy(error);
  return failed;
}

static int publish(rumqttc_client_t *client, rumqttc_string_view_t topic) {
  rumqttc_publish_options_t options = example_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *completion = NULL;
  rumqttc_error_t *error = NULL;
  int failed = example_report(
      rumqttc_client_publish_tracked(client, topic, example_bytes("hello", 5),
                                     &options, &completion, &error),
      &error, "publish");

  if (!failed)
    failed = example_wait(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);

  rumqttc_completion_destroy(completion);
  rumqttc_error_destroy(error);
  return failed;
}

static int run_example(rumqttc_client_t *client) {
  rumqttc_subscription_t subscription = RUMQTTC_SUBSCRIPTION_INIT;
  rumqttc_event_t *event;

  subscription.filter = example_string("rumqttc/native/event-example");
  subscription.qos = RUMQTTC_QOS_1;

  if (subscribe(client, &subscription) || publish(client, subscription.filter))
    return 1;

  event = example_next_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
  if (event == NULL)
    return 1;

  puts("received publish while polling one event at a time");
  rumqttc_event_destroy(event);
  return 0;
}

int main(int argc, char **argv) {
  rumqttc_client_t *client;
  int result;

  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }

  client = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10),
                           "c-event-poll", RUMQTTC_ACK_AUTOMATIC);
  if (client == NULL)
    return 1;

  result = run_example(client);
  (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
  example_destroy_client(&client);
  return result;
}
