#include "native_common.h"

#include <stdio.h>
#include <string.h>

static rumqttc_client_t *start(rumqttc_protocol_t protocol, const char *scenario,
                             const rumqttc_reconnect_options_t *options) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_reconnect_options_t invalid = *options;
  char id[80];
  REQUIRE(snprintf(id, sizeof(id), "native-reconnect-%s-%u", scenario,
                   (unsigned)protocol) > 0);
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                 native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
  CHECK(rumqttc_config_set_reconnect_policy(config, options, NULL));
  invalid.reserved = 1;
  REQUIRE(rumqttc_config_set_reconnect_policy(config, &invalid, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_config_destroy(config);
  return client;
}

static void expect_code(const rumqttc_error_t *error, const char *code) {
  rumqttc_string_view_t value = {NULL, 0};
  CHECK(rumqttc_error_code(error, &value));
  REQUIRE(value.len == strlen(code) && memcmp(value.data, code, value.len) == 0);
}

static void exhaustion(rumqttc_protocol_t protocol) {
  rumqttc_reconnect_options_t options = RUMQTTC_RECONNECT_OPTIONS_INIT;
  rumqttc_reconnect_diagnostics_t diagnostics = RUMQTTC_RECONNECT_DIAGNOSTICS_INIT;
  rumqttc_client_t *client;
  rumqttc_event_t *event;
  rumqttc_error_t *error = NULL;
  rumqttc_error_t *last = NULL;
  uint8_t present = 0;
  uint64_t cycles = 0, retries = 0;
  options.budget_kind = RUMQTTC_RECONNECT_BUDGET_FINITE;
  options.retry_limit = 2;
  options.initial_delay_ms = options.maximum_delay_ms = 10;
  options.jitter = RUMQTTC_RECONNECT_JITTER_NONE;
  client = start(protocol, "exhaust", &options);
  event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
  CHECK(rumqttc_event_disconnected(event, NULL, &error));
  expect_code(error, "RECONNECT_EXHAUSTED");
  CHECK(rumqttc_error_reconnect_exhaustion(error, &present, &cycles, &retries));
  REQUIRE(present && cycles == 3 && retries == 2);
  CHECK(rumqttc_error_reconnect_last_error(error, &last, NULL));
  REQUIRE(last != NULL);
  CHECK(rumqttc_client_reconnect_diagnostics(client, &diagnostics, NULL));
  REQUIRE(diagnostics.mode == RUMQTTC_RECONNECT_CLASSIFIED);
  REQUIRE(diagnostics.cycles_started == 3 && diagnostics.retries_since_reset == 2);
  REQUIRE(diagnostics.stop_reason == RUMQTTC_RECONNECT_STOP_EXHAUSTED);
  rumqttc_event_destroy(event);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  expect_code(last, "BROKER_REJECTED");
  CHECK(rumqttc_error_reconnect_exhaustion(error, &present, NULL, NULL));
  REQUIRE(present);
  rumqttc_error_destroy(last);
  rumqttc_error_destroy(error);
}

static void backoff_close(rumqttc_protocol_t protocol) {
  rumqttc_reconnect_options_t options = RUMQTTC_RECONNECT_OPTIONS_INIT;
  rumqttc_reconnect_diagnostics_t diagnostics = RUMQTTC_RECONNECT_DIAGNOSTICS_INIT;
  rumqttc_completion_t *observation = NULL;
  rumqttc_error_t *last = NULL;
  rumqttc_event_t *event;
  rumqttc_client_t *client;
  options.initial_delay_ms = options.maximum_delay_ms = 30000;
  options.jitter = RUMQTTC_RECONNECT_JITTER_NONE;
  client = start(protocol, "close", &options);
  event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
  rumqttc_event_destroy(event);
  CHECK(rumqttc_client_diagnostics_tracked(client, &observation, NULL));
  CHECK(rumqttc_completion_wait_timeout_ms(observation, NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_completion_reconnect_diagnostics(observation, &diagnostics, NULL));
  REQUIRE(diagnostics.phase == RUMQTTC_RECONNECT_PHASE_WAITING);
  REQUIRE(diagnostics.remaining_delay_at_capture_ms > 20000);
  CHECK(rumqttc_completion_reconnect_last_error(observation, &last, NULL));
  REQUIRE(last != NULL);
  CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_client_reconnect_diagnostics(client, &diagnostics, NULL));
  REQUIRE(diagnostics.cycles_started == 1 && diagnostics.stop_reason == RUMQTTC_RECONNECT_STOP_SHUTDOWN);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_completion_reconnect_diagnostics(observation, &diagnostics, NULL));
  REQUIRE(diagnostics.phase == RUMQTTC_RECONNECT_PHASE_WAITING);
  expect_code(last, "BROKER_REJECTED");
  rumqttc_error_destroy(last);
  rumqttc_completion_destroy(observation);
}

static void recovery(rumqttc_protocol_t protocol) {
  static const uint8_t payload[] = "recovered";
  rumqttc_reconnect_options_t options = RUMQTTC_RECONNECT_OPTIONS_INIT;
  rumqttc_reconnect_diagnostics_t diagnostics = RUMQTTC_RECONNECT_DIAGNOSTICS_INIT;
  rumqttc_publish_options_t publish = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *completion = NULL;
  rumqttc_event_t *event;
  rumqttc_client_t *client;
  options.initial_delay_ms = options.maximum_delay_ms = 10;
  options.jitter = RUMQTTC_RECONNECT_JITTER_NONE;
  client = start(protocol, "recover", &options);
  event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(event);
  CHECK(rumqttc_client_reconnect_diagnostics(client, &diagnostics, NULL));
  REQUIRE(diagnostics.cycles_started == 3 && diagnostics.retries_since_reset == 2);
  CHECK(rumqttc_client_publish_tracked(client, native_string("reconnect"),
                                      native_bytes(payload, sizeof(payload) - 1),
                                      &publish, &completion, NULL));
  CHECK(rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, NULL));
  native_close_destroy(client);
  CHECK(rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, NULL));
  rumqttc_completion_destroy(completion);
}

static void authentication(rumqttc_protocol_t protocol) {
  rumqttc_reconnect_options_t options = RUMQTTC_RECONNECT_OPTIONS_INIT;
  rumqttc_reconnect_diagnostics_t diagnostics = RUMQTTC_RECONNECT_DIAGNOSTICS_INIT;
  rumqttc_client_t *client = start(protocol, "auth", &options);
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
  CHECK(rumqttc_client_reconnect_diagnostics(client, &diagnostics, NULL));
  REQUIRE(diagnostics.cycles_started == 1 && diagnostics.stop_reason == RUMQTTC_RECONNECT_STOP_TERMINAL_FAILURE);
  rumqttc_event_destroy(event);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
}

int main(void) {
  for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4;
       protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
    exhaustion(protocol);
    backoff_close(protocol);
    recovery(protocol);
    authentication(protocol);
  }
  return 0;
}
