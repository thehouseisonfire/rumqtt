#include "example_common.h"

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_completion_t *publish = NULL;
  rumqttc_reconnect_options_t retry = RUMQTTC_RECONNECT_OPTIONS_INIT;
  rumqttc_publish_options_t options = example_publish_options(RUMQTTC_QOS_1);
  int result = 1;
  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }
  /* Leave budget_kind=UNLIMITED for a long-running service. This bounded job
   * allows five retries after its free initial cycle. Wait timeouts observe
   * operations; they do not spend retries or cancel admitted work. */
  retry.budget_kind = RUMQTTC_RECONNECT_BUDGET_FINITE;
  retry.retry_limit = 5;
  retry.initial_delay_ms = 250;
  retry.maximum_delay_ms = 4000;
  if (example_report(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, &error), &error, "config") ||
      example_report(rumqttc_config_set_broker(config, example_string(argv[1]),
                                               (uint16_t)strtoul(argv[2], NULL, 10), &error), &error, "broker") ||
      example_report(rumqttc_config_set_client_id(config, example_string("c-reconnect-example"), &error),
                     &error, "client id") ||
      example_report(rumqttc_config_set_reconnect_policy(config, &retry, &error), &error, "retry policy") ||
      example_report(rumqttc_client_start(config, &client, &error), &error, "start"))
    goto cleanup;
  for (;;) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    rumqttc_status_t status = rumqttc_client_event_recv_timeout_ms(client, 5000, &event, &error);
    if (status == RUMQTTC_TIMEOUT) {
      rumqttc_error_destroy(error);
      error = NULL;
      continue;
    }
    if (example_report(status, &error, "event") ||
        example_report(rumqttc_event_kind(event, &kind), &error, "event kind")) {
      rumqttc_event_destroy(event);
      goto cleanup;
    }
    if (kind == RUMQTTC_EVENT_DRIVER_TERMINATED) {
      rumqttc_error_t *failure = NULL;
      uint8_t exhausted = 0;
      uint64_t cycles = 0;
      rumqttc_event_disconnected(event, NULL, &failure);
      rumqttc_error_reconnect_exhaustion(failure, &exhausted, &cycles, NULL);
      if (!exhausted) {
        rumqttc_reconnect_diagnostics_t snapshot = RUMQTTC_RECONNECT_DIAGNOSTICS_INIT;
        if (rumqttc_client_reconnect_diagnostics(client, &snapshot, NULL) == RUMQTTC_OK)
          cycles = snapshot.cycles_started;
      }
      fprintf(stderr, "client stopped%s after %" PRIu64 " cycles; unfinished delivery may be ambiguous\n",
              exhausted ? " by retry exhaustion" : " by terminal failure", cycles);
      rumqttc_error_destroy(failure);
      rumqttc_event_destroy(event);
      goto cleanup;
    }
    rumqttc_event_destroy(event);
    if (kind == RUMQTTC_EVENT_CONNECTED)
      break;
    if (kind == RUMQTTC_EVENT_DISCONNECTED) {
      rumqttc_reconnect_diagnostics_t snapshot = RUMQTTC_RECONNECT_DIAGNOSTICS_INIT;
      if (!example_report(rumqttc_client_reconnect_diagnostics(client, &snapshot, &error), &error, "retry observation"))
        printf("retry after cycle %" PRIu64 ": delay at capture %" PRIu64 " ms\n",
               snapshot.cycles_started, snapshot.remaining_delay_at_capture_ms);
    }
  }
  if (!example_report(rumqttc_client_publish_tracked(client, example_string("rumqttc/native/reconnect"),
                                                    example_bytes("recovered", 9), &options, &publish, &error),
                      &error, "publish"))
    result = example_wait(publish, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
cleanup:
  rumqttc_completion_destroy(publish);
  example_destroy_client(&client);
  rumqttc_config_destroy(config);
  rumqttc_error_destroy(error);
  return result;
}
