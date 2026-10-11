#include "example_common.h"

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  rumqttc_client_t *client;
  rumqttc_diagnostics_snapshot_t *snapshot = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_diagnostics_status_t status = RUMQTTC_DIAGNOSTICS_STATUS_INIT;
  rumqttc_diagnostics_queues_t queues = RUMQTTC_DIAGNOSTICS_QUEUES_INIT;
  rumqttc_diagnostics_outbound_t outbound = RUMQTTC_DIAGNOSTICS_OUTBOUND_INIT;
  rumqttc_diagnostics_group_info_t info = RUMQTTC_DIAGNOSTICS_GROUP_INFO_INIT;
  int result = 1;
  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }
  client = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10),
                           "c-diagnostics", RUMQTTC_ACK_AUTOMATIC);
  if (client == NULL)
    return 1;
  if (example_report(rumqttc_client_diagnostics_snapshot(client, &snapshot, &error),
                     &error, "snapshot"))
    goto cleanup;
  /* The owned snapshot neither retains the client nor changes after close.
   * Native ages continue increasing; wrapper and native groups need not agree
   * at one instant. Acquire another snapshot to observe a later lifecycle. */
  if (example_report(rumqttc_client_close_now_timeout_ms(client, 5000, &error),
                     &error, "close"))
    goto cleanup;
  example_destroy_client(&client);
  if (example_report(rumqttc_diagnostics_snapshot_status(snapshot, &status, &error),
                     &error, "status") ||
      example_report(rumqttc_diagnostics_snapshot_queues(snapshot, &queues, &error),
                     &error, "queues") ||
      example_report(rumqttc_diagnostics_snapshot_outbound(snapshot, &outbound, &error),
                     &error, "outbound") ||
      example_report(rumqttc_diagnostics_snapshot_group_info(
                         snapshot, RUMQTTC_DIAGNOSTICS_GROUP_REDIRECT, &info, &error),
                     &error, "redirect availability"))
    goto cleanup;
  printf("native capture %" PRIu64 ", age %" PRIu64 " ns; snapshot age %" PRIu64 " ns\n",
         status.native_generation, status.native_age_ns, status.snapshot_age_ns);
  if (queues.availability == RUMQTTC_DIAGNOSTICS_AVAILABLE)
    printf("replay=%" PRIu64 ", scheduler=%" PRIu64 ", request channel=%" PRIu64
           ", control channel=%" PRIu64 ", immediate shutdown=%" PRIu64 "\n",
           queues.pending_replay_len, queues.queued_len, queues.requests_rx_len,
           queues.control_requests_rx_len, queues.immediate_disconnect_rx_len);
  if (outbound.availability == RUMQTTC_DIAGNOSTICS_AVAILABLE)
    printf("inflight=%" PRIu32 "/%" PRIu32 ", identifiers=%" PRIu64 "\n",
           outbound.inflight, outbound.max_inflight, outbound.packet_identifiers_in_use);
  printf("redirect availability=%" PRIu32 "\n", info.availability);
  result = 0;
cleanup:
  rumqttc_diagnostics_snapshot_destroy(snapshot);
  rumqttc_error_destroy(error);
  example_destroy_client(&client);
  return result;
}
