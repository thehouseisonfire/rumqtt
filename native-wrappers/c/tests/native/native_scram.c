#include "native_common.h"

#include <string.h>

int main(void) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_event_t *connected;
  uint8_t password[] = "scram-private-password";
  if (!(rumqttc_library_capabilities() & RUMQTTC_CAP_SCRAM))
    return 0;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-v5-scram"),
                                     NULL));
  CHECK(rumqttc_config_set_v5_scram(
      config, native_string("scram-private-username"),
      native_bytes(password, sizeof(password) - 1), 5000, 4096, NULL));
  memset(password, 0, sizeof(password));
  CHECK(rumqttc_client_start(config, &client, NULL));
  connected = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(connected);
  CHECK(rumqttc_client_reauthenticate_tracked(client, &completion, NULL));
  native_wait_completion(completion, RUMQTTC_COMPLETION_AUTHENTICATED);
  rumqttc_completion_destroy(completion);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
  return 0;
}
