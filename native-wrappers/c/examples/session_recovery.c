#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>

/* Invoking this example is an operator request to discard this client's session
 * at the next disconnected boundary. Production applications should offer their
 * own explicit recovery control and inspect every discarded operation. */
int main(int argc, char **argv) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *recovery = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_reconnect_options_t retry = RUMQTTC_RECONNECT_OPTIONS_INIT;
  rumqttc_session_recovery_snapshot_t snapshot = RUMQTTC_SESSION_RECOVERY_SNAPSHOT_INIT;
  int result = 1;
  rumqttc_status_t observed;
  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }
  retry.initial_delay_ms = retry.maximum_delay_ms = 500;
  retry.jitter = RUMQTTC_RECONNECT_JITTER_NONE;
  if (example_report(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, &error), &error, "config") ||
      example_report(rumqttc_config_set_broker(config, example_string(argv[1]),
                     (uint16_t)strtoul(argv[2], NULL, 10), &error), &error, "broker") ||
      example_report(rumqttc_config_set_client_id(config, example_string("c-session-recovery-example"),
                     &error), &error, "identity") ||
      example_report(rumqttc_config_set_v5_session(config, 0, 1, 60, &error), &error, "persistent policy") ||
      example_report(rumqttc_config_set_reconnect_policy(config, &retry, &error), &error, "retry policy") ||
      example_report(rumqttc_client_start(config, &client, &error), &error, "start"))
    goto cleanup;
  for (;;) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    if (example_report(rumqttc_client_event_recv_timeout_ms(client, 10000, &event, &error),
                       &error, "wait for disconnected client"))
      goto cleanup;
    rumqttc_event_kind(event, &kind);
    rumqttc_event_destroy(event);
    if (kind == RUMQTTC_EVENT_DRIVER_TERMINATED)
      goto cleanup; /* Terminal clients require store administration and replacement. */
    if (kind == RUMQTTC_EVENT_DISCONNECTED)
      break;
  }
  /* The driver may have reconnected since the event: admission can reject safely. */
  if (example_report(rumqttc_client_recover_session_tracked(client, &recovery, &error),
                     &error, "admit explicit recovery"))
    goto cleanup;
  /* This is an observer deadline, not a cancellation or recovery retry budget. */
  observed = rumqttc_completion_wait_timeout_ms(recovery, 10000, &error);
  if (observed == RUMQTTC_OK) {
    result = example_wait(recovery, RUMQTTC_COMPLETION_SESSION_RECOVERED);
    if (!result)
      puts("Fresh session established; recreate required subscriptions and application state.");
  } else {
    example_report(observed, &error, "observe recovery");
    if (rumqttc_completion_session_recovery_snapshot(recovery, &snapshot, NULL) == RUMQTTC_OK) {
      fprintf(stderr, "phase=%u failure_phase=%u abandoned=%u cleared=%u fresh=%u\n",
              snapshot.phase, snapshot.failure_phase, snapshot.abandonment_committed,
              snapshot.checkpoint_cleared, snapshot.fresh_established);
      if (snapshot.abandonment_committed && !snapshot.checkpoint_cleared)
        fputs("Resolve checkpoint clearing before starting a replacement client.\n", stderr);
    }
  }
cleanup:
  example_destroy_client(&client);
  rumqttc_completion_destroy(recovery);
  rumqttc_config_destroy(config);
  rumqttc_error_destroy(error);
  return result;
}
