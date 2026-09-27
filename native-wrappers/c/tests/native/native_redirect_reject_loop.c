#include "native_common.h"

#include <string.h>

static void run_case(const char *client_id, uint32_t policy,
                     uint32_t expected_failure, uint32_t expected_decision,
                     uint8_t expected_loop) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  unsigned saw_failure = 0;
  unsigned saw_terminal = 0;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  CHECK(rumqttc_config_set_v5_redirect_policy(
      config, policy, policy == RUMQTTC_REDIRECT_FOLLOW ? 3 : 0,
      RUMQTTC_REDIRECT_TRANSPORT_TCP, NULL, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  while (!saw_terminal) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS,
                                               &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    if (kind == RUMQTTC_EVENT_REDIRECT) {
      uint32_t source = 0;
      uint32_t reason = 0;
      uint8_t failure_present = 0;
      uint32_t failure = 0;
      uint8_t reference_present = 0;
      rumqttc_string_view_t reference = {NULL, 0};
      uint32_t decision = 0;
      uint64_t attempts = 0;
      uint8_t loop = 0;
      CHECK(rumqttc_event_redirect(event, &source, &reason, &failure_present,
                                   &failure, &reference_present, &reference));
      CHECK(rumqttc_event_redirect_diagnostics(event, &decision, &attempts,
                                               NULL, NULL, NULL, &loop, NULL,
                                               NULL, NULL));
      REQUIRE(source == RUMQTTC_REDIRECT_SOURCE_CONNACK);
      REQUIRE(reason == RUMQTTC_REDIRECT_REASON_USE_ANOTHER_SERVER);
      REQUIRE(reference_present == 1 && reference.len > strlen("127.0.0.1:"));
      REQUIRE(memcmp(reference.data, "127.0.0.1:", strlen("127.0.0.1:")) == 0);
      if (failure_present) {
        REQUIRE(failure == expected_failure);
        REQUIRE(decision == expected_decision);
        REQUIRE(loop == expected_loop);
        REQUIRE(attempts <= 1);
        saw_failure = 1;
      }
    } else if (kind == RUMQTTC_EVENT_CONNECTED) {
      REQUIRE(0);
    } else if (kind == RUMQTTC_EVENT_DRIVER_TERMINATED) {
      REQUIRE(saw_failure == 1);
      saw_terminal = 1;
    }
    rumqttc_event_destroy(event);
  }
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

int main(void) {
  run_case("native-v5-redirect-reject", RUMQTTC_REDIRECT_REJECT,
           RUMQTTC_REDIRECT_FAILURE_DISABLED,
           RUMQTTC_REDIRECT_DECISION_REJECT, 0);
  run_case("native-v5-redirect-loop", RUMQTTC_REDIRECT_FOLLOW,
           RUMQTTC_REDIRECT_FAILURE_LOOP,
           RUMQTTC_REDIRECT_DECISION_REJECT, 1);
  return 0;
}
