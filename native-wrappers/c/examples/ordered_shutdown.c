#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  rumqttc_client_t *client;
  rumqttc_error_t *error = NULL;
  rumqttc_completion_t *fence = NULL;
  rumqttc_publish_options_t options = example_publish_options(RUMQTTC_QOS_1);
  uint64_t capabilities = rumqttc_library_capabilities();
  int failed;

  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }
  if ((capabilities & RUMQTTC_CAP_ORDERED_SHUTDOWN) == 0)
    return 77;
  client = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10), "c-ordered-example", RUMQTTC_ACK_AUTOMATIC);
  if (client == NULL)
    return 1;

  for (unsigned index = 0; index < 8; ++index) {
    uint64_t operation_id = 0;
    if (example_report(rumqttc_client_try_publish(client, example_string("rumqttc/native/ordered"),
                                                  example_bytes("burst", 5), &options, &operation_id, &error),
                       &error, "publish admission")) {
      (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
      example_destroy_client(&client);
      return 1;
    }
  }
  /* Admission is nonblocking. A full request queue returns BACKPRESSURE and
   * installs no fence; an application can retry within its admission budget. */
  failed = example_report(rumqttc_client_disconnect_after_queued_timeout_ms_tracked(client, 5000, &fence, &error),
                          &error, "ordered fence admission");
  if (!failed)
    failed = example_wait(fence, RUMQTTC_COMPLETION_ORDERED_SHUTDOWN);
  /* Join has its own caller budget. Repeating this closer preserves the first
   * fence's deadline and result. Timeout never reports successful delivery. */
  if (!failed)
    failed = example_report(rumqttc_client_close_after_queued_timeout_ms(client, 5000, &error), &error,
                            "ordered close join");
  if (failed)
    (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
  example_destroy_client(&client);
  /* The immutable fence result remains observable after client destruction. */
  rumqttc_completion_destroy(fence);
  rumqttc_error_destroy(error);
  return failed;
}
