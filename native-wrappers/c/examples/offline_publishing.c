#include "example_common.h"
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_publish_options_t options = example_publish_options(RUMQTTC_QOS_1);
  int failed = 0;
  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }
#define CALL(expression) do { if (example_report((expression), &error, #expression)) { failed = 1; goto done; } } while (0)
  CALL(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, &error));
  CALL(rumqttc_config_set_broker(config, example_string(argv[1]), (uint16_t)strtoul(argv[2], NULL, 10), &error));
  CALL(rumqttc_config_set_client_id(config, example_string("c-offline-publishing"), &error));
  CALL(rumqttc_config_set_v5_publish_admission_policy(config, RUMQTTC_PUBLISH_ADMISSION_EVENT_LOOP_VALIDATED, &error));
  CALL(rumqttc_config_set_v5_publish_budget(config, 8, 64 * 1024, &error));
  CALL(rumqttc_client_start(config, &client, &error));
  /* No Connected wait: native processing validates negotiated capabilities later.
   * Admission is nonblocking. BACKPRESSURE means nothing was enqueued; inspect
   * rumqttc_error_publish_failure before choosing an application retry policy.
   * An admitted command is process-local until checkpointed in protocol state.
   * Keep an application outbox if every submission must survive a crash. */
  CALL(rumqttc_client_publish_tracked(client, example_string("rumqttc/native/offline"),
                                    example_bytes("queued", 6), &options, &completion, &error));
  {
    size_t outstanding = 0, bytes = 0;
    CALL(rumqttc_client_v5_publish_budget_snapshot(client, &outstanding, &bytes, NULL, NULL, &error));
    printf("outstanding publishes: %zu, charged data bytes: %zu\n", outstanding, bytes);
  }
  /* Admission does not mean broker acceptance. Local negotiated rejection is
   * LOCAL_REJECTED with no broker reason. QoS 0 would complete at local flush. */
  failed = example_wait(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
done:
  rumqttc_completion_destroy(completion);
  rumqttc_config_destroy(config);
  rumqttc_error_destroy(error);
  if (client != NULL)
    (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
  example_destroy_client(&client);
  return failed;
}
