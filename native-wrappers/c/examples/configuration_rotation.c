#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Use the application's own secret source in production. Never log these values. */
static const char *secret(const char *name, const char *fallback) {
  const char *value = getenv(name);
  return value != NULL ? value : fallback;
}

int main(int argc, char **argv) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_runtime_update_t *update = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_configuration_receipt_t *receipt = NULL;
  rumqttc_configuration_snapshot_t *snapshot = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_event_t *event = NULL;
  rumqttc_configuration_receipt_status_t activation = RUMQTTC_CONFIGURATION_RECEIPT_STATUS_INIT;
  rumqttc_configuration_status_t observed = RUMQTTC_CONFIGURATION_STATUS_INIT;
  rumqttc_publish_options_t publish = RUMQTTC_PUBLISH_OPTIONS_INIT;
  const char *username = secret("RUMQTTC_USERNAME", "rotation-user");
  const char *password = secret("RUMQTTC_PASSWORD", "initial-token");
  const char *replacement = secret("RUMQTTC_NEXT_PASSWORD", "replacement-token");
  int await_reconnect = strcmp(secret("RUMQTTC_AWAIT_RECONNECT", "0"), "1") == 0;
  int failed = 1;
  rumqttc_protocol_t protocol;

  if (argc != 3 && argc != 4) {
    fprintf(stderr, "usage: %s HOST PORT [4|5]\n", argv[0]);
    return 2;
  }
  protocol = argc == 4 && strcmp(argv[3], "4") == 0 ? RUMQTTC_PROTOCOL_V4 : RUMQTTC_PROTOCOL_V5;
#define CALL(expression) do { if (example_report((expression), &error, #expression)) goto cleanup; } while (0)
  CALL(rumqttc_config_new(protocol, &config, &error));
  CALL(rumqttc_config_set_broker(config, example_string(argv[1]), (uint16_t)strtoul(argv[2], NULL, 10), &error));
  CALL(rumqttc_config_set_client_id(config, example_string("c-rotation"), &error));
  CALL(rumqttc_config_set_keep_alive_seconds(config, 0, &error));
  CALL(rumqttc_config_set_username(config, example_string(username), &error));
  CALL(rumqttc_config_set_password(config, example_bytes(password, strlen(password)), &error));
  CALL(rumqttc_client_start(config, &client, &error));
  rumqttc_config_destroy(config);
  config = NULL;
  event = example_next_event(client, RUMQTTC_EVENT_CONNECTED);
  if (event == NULL) goto cleanup;
  rumqttc_event_destroy(event);
  event = NULL;

  CALL(rumqttc_runtime_update_new(&update, &error));
  /* Username and password are one value. TLS and network replacements can be
   * staged in this same update to select one coherent next-attempt profile. */
  CALL(rumqttc_runtime_update_set_credentials(update, 1, example_string(username), 1,
                                             example_bytes(replacement, strlen(replacement)), &error));
  CALL(rumqttc_runtime_update_set_max_request_batch(update, 8, &error));
  CALL(rumqttc_client_update_configuration_tracked(client, update, &completion, &error));
  rumqttc_runtime_update_destroy(update);
  update = NULL;
  if (example_wait(completion, RUMQTTC_COMPLETION_CONFIGURATION_STAGED)) goto cleanup;
  CALL(rumqttc_completion_configuration_receipt(completion, &receipt, &error));
  rumqttc_completion_destroy(completion);
  completion = NULL;
  CALL(rumqttc_configuration_receipt_status(receipt, &activation, &error));
  printf("configuration revision %llu staged; connection activation %u\n",
         (unsigned long long)activation.revision, (unsigned)activation.connection_state);

  /* Staging does not interrupt an idle poll or force a reconnect. This publish
   * also lets the test broker know it may close the first connection. */
  publish.qos = RUMQTTC_QOS_0;
  CALL(rumqttc_client_publish_tracked(client, example_string("rumqttc/native/rotation-ready"),
                                     example_bytes("ready", 5), &publish, &completion, &error));
  if (example_wait(completion, RUMQTTC_COMPLETION_QOS0_FLUSHED)) goto cleanup;
  if (await_reconnect) {
    event = example_next_event(client, RUMQTTC_EVENT_CONNECTED);
    if (event == NULL) goto cleanup;
    rumqttc_event_destroy(event);
    event = NULL;
    CALL(rumqttc_client_configuration_snapshot(client, &snapshot, &error));
    CALL(rumqttc_configuration_snapshot_status(snapshot, &observed, &error));
    CALL(rumqttc_configuration_receipt_status(receipt, &activation, &error));
    if (activation.connection_state != RUMQTTC_CONFIG_ACTIVATION_ACTIVATED ||
        !(observed.flags & RUMQTTC_CONFIG_STATUS_SUCCESSFUL_REVISION_PRESENT) ||
        observed.successful_connection_revision != activation.revision) goto cleanup;
    puts("reconnected with the staged origin profile");
  }
  failed = 0;

cleanup:
  rumqttc_event_destroy(event);
  rumqttc_configuration_snapshot_destroy(snapshot);
  rumqttc_configuration_receipt_destroy(receipt);
  rumqttc_completion_destroy(completion);
  rumqttc_runtime_update_destroy(update);
  rumqttc_config_destroy(config);
  if (client != NULL) {
    if (rumqttc_client_close_now_timeout_ms(client, 5000, NULL) != RUMQTTC_OK) failed = 1;
    example_destroy_client(&client);
  }
  rumqttc_error_destroy(error);
  return failed;
}
