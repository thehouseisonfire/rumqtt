#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>

static int publish_and_wait(rumqttc_client_t *client) {
  rumqttc_publish_options_t options = example_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *completion = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_status_t poll_status;
  int failed = example_report(
      rumqttc_client_publish_tracked(
          client, example_string("rumqttc/native/completion"),
          example_bytes("tracked", 7), &options, &completion, &error),
      &error, "publish");

  if (!failed) {
    poll_status = rumqttc_completion_poll(completion, &error);
    if (poll_status != RUMQTTC_OK && poll_status != RUMQTTC_WOULD_BLOCK)
      failed = example_report(poll_status, &error, "completion_poll");
  }

  if (!failed)
    failed = example_wait(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);

  rumqttc_completion_destroy(completion);
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
                           "c-completion", RUMQTTC_ACK_AUTOMATIC);
  if (client == NULL)
    return 1;

  result = publish_and_wait(client);
  if (result == 0)
    puts("tracked publish reached its MQTT acknowledgement milestone");

  (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
  example_destroy_client(&client);
  return result;
}
