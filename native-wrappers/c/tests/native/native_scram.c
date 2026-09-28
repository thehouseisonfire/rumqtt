#include "native_common.h"

#include <string.h>

static int contains(rumqttc_string_view_t haystack, const char *needle) {
  size_t length = strlen(needle);
  for (size_t index = 0; index + length <= haystack.len; ++index) {
    if (memcmp(haystack.data + index, needle, length) == 0)
      return 1;
  }
  return 0;
}

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
  {
    rumqttc_error_t *error = NULL;
    uint8_t present = 0;
    uint32_t failure = 0;
    rumqttc_string_view_t message = {NULL, 0};
    rumqttc_string_view_t sources = {NULL, 0};
    uint8_t invalid_password[] = "scram-private-password";
    CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
    CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                    native_test_port(), NULL));
    CHECK(rumqttc_config_set_client_id(config,
                                       native_string("native-v5-scram-invalid"),
                                       NULL));
    CHECK(rumqttc_config_set_v5_scram(
        config, native_string("scram-private-username"),
        native_bytes(invalid_password, sizeof(invalid_password) - 1),
        5000, 4096, NULL));
    memset(invalid_password, 0, sizeof(invalid_password));
    CHECK(rumqttc_client_start(config, &client, NULL));
    connected = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
    CHECK(rumqttc_event_disconnected(connected, NULL, &error));
    REQUIRE(error != NULL);
    CHECK(rumqttc_error_auth_failure(error, &present, &failure));
    REQUIRE(present == 1 && failure == RUMQTTC_AUTH_FAILURE_REJECTED);
    CHECK(rumqttc_error_message(error, &message));
    CHECK(rumqttc_error_source_chain(error, &sources));
    REQUIRE(!contains(message, "scram-private-password"));
    REQUIRE(!contains(message, "scram-private-username"));
    REQUIRE(!contains(message, "v=AAAA"));
    REQUIRE(!contains(sources, "scram-private-password"));
    REQUIRE(!contains(sources, "scram-private-username"));
    REQUIRE(!contains(sources, "v=AAAA"));
    rumqttc_error_destroy(error);
    rumqttc_event_destroy(connected);
    native_close_destroy(client);
    rumqttc_config_destroy(config);
  }
  return 0;
}
