#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_publish_options_t options = example_publish_options(RUMQTTC_QOS_1);
  int result = 1;
  if (argc != 3)
    return 2;
  client = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10),
                           "c-completion", RUMQTTC_ACK_AUTOMATIC);
  if (client == NULL)
    goto cleanup;
  if (example_report(rumqttc_client_publish_tracked(
                         client, example_string("rumqttc/native/completion"),
                         example_bytes("tracked", 7), &options, &completion,
                         &error),
                     &error, "publish"))
    goto cleanup;
  {
    rumqttc_status_t status = rumqttc_completion_poll(completion, NULL);
    if (status != RUMQTTC_OK && status != RUMQTTC_WOULD_BLOCK)
      goto cleanup;
  }
  if (example_wait(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED))
    goto cleanup;
  puts("tracked publish reached its MQTT acknowledgement milestone");
  result = 0;
cleanup:
  rumqttc_completion_destroy(completion);
  if (client != NULL)
    (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
  example_destroy_client(&client);
  rumqttc_error_destroy(error);
  return result;
}
