#include "native_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static rumqttc_client_t *start(rumqttc_protocol_t protocol, const char *scenario,
                               int refused) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_reconnect_options_t retry = RUMQTTC_RECONNECT_OPTIONS_INIT;
  char id[96];
  uint16_t port = native_test_port();
  if (refused) {
    const char *value = getenv("RUMQTTC_TEST_REFUSED_PORT_1");
    REQUIRE(value != NULL);
    port = (uint16_t)strtoul(value, NULL, 10);
  }
  REQUIRE(snprintf(id, sizeof(id), "native-failure-%s-%u", scenario, (unsigned)protocol) > 0);
  retry.budget_kind = RUMQTTC_RECONNECT_BUDGET_FINITE;
  retry.retry_limit = 0;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), port, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
  CHECK(rumqttc_config_set_keep_alive_seconds(config, 1, NULL));
  CHECK(rumqttc_config_set_reconnect_policy(config, &retry, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_config_destroy(config);
  return client;
}

static void protocol_failure(rumqttc_protocol_t protocol, const char *scenario,
                              uint32_t expected_code) {
  rumqttc_client_t *client = start(protocol, scenario, 0);
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_completion_t *completion = NULL;
  rumqttc_subscription_t subscription = native_subscription("safe/filter", RUMQTTC_QOS_1);
  rumqttc_error_t *error = NULL;
  rumqttc_error_t *operation_error = NULL;
  rumqttc_error_t *last = NULL;
  rumqttc_native_failure_details_t details = RUMQTTC_NATIVE_FAILURE_DETAILS_INIT;
  rumqttc_native_failure_details_t retained = RUMQTTC_NATIVE_FAILURE_DETAILS_INIT;
  rumqttc_native_failure_details_t operation = RUMQTTC_NATIVE_FAILURE_DETAILS_INIT;
  rumqttc_event_destroy(event);
  CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, NULL, &completion, NULL));
  event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
  CHECK(rumqttc_event_disconnected(event, NULL, &error));
  /* Missed PING is retryable and reaches finite-budget exhaustion. */
  if (expected_code == RUMQTTC_NATIVE_FAILURE_PING_RESPONSE_TIMEOUT) {
    rumqttc_error_t *cause = NULL;
    CHECK(rumqttc_error_reconnect_last_error(error, &cause, NULL));
    rumqttc_error_destroy(error);
    error = cause;
  }
  CHECK(rumqttc_error_native_failure_details(error, &details));
  REQUIRE(details.present && details.code == expected_code);
  CHECK(rumqttc_client_reconnect_last_error(client, &last, NULL));
  REQUIRE(last != NULL);
  REQUIRE(rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, &operation_error) != RUMQTTC_OK);
  REQUIRE(operation_error != NULL);
  if (expected_code == RUMQTTC_NATIVE_FAILURE_PING_RESPONSE_TIMEOUT) {
    rumqttc_error_t *cause = NULL;
    CHECK(rumqttc_error_reconnect_last_error(operation_error, &cause, NULL));
    rumqttc_error_destroy(operation_error);
    operation_error = cause;
  }
  rumqttc_event_destroy(event);
  rumqttc_completion_destroy(completion);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_error_native_failure_details(last, &retained));
  CHECK(rumqttc_error_native_failure_details(operation_error, &operation));
  REQUIRE(memcmp(&details, &retained, sizeof(details)) == 0);
  REQUIRE(memcmp(&details, &operation, sizeof(details)) == 0);
  if (expected_code == RUMQTTC_NATIVE_FAILURE_UNSOLICITED_ACK) {
    REQUIRE((details.fields & RUMQTTC_NATIVE_FIELD_PACKET_ID) != 0);
    REQUIRE(details.packet_id == 0x4242);
    REQUIRE((details.fields & RUMQTTC_NATIVE_FIELD_PACKET_KIND) == 0);
  }
  if (expected_code == RUMQTTC_NATIVE_FAILURE_INCOMING_TOO_LARGE) {
    REQUIRE(details.actual == (protocol == RUMQTTC_PROTOCOL_V4 ? 16384u : 16388u));
    REQUIRE(details.quantity == (protocol == RUMQTTC_PROTOCOL_V4 ?
        RUMQTTC_NATIVE_QUANTITY_REMAINING_BYTES : RUMQTTC_NATIVE_QUANTITY_PACKET_BYTES));
    REQUIRE(((details.fields & RUMQTTC_NATIVE_FIELD_MAXIMUM) != 0) == (protocol == RUMQTTC_PROTOCOL_V5));
  }
  rumqttc_error_destroy(operation_error);
  rumqttc_error_destroy(last);
  rumqttc_error_destroy(error);
}

static void connection_refused(rumqttc_protocol_t protocol) {
  rumqttc_client_t *client = start(protocol, "refused", 1);
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
  rumqttc_error_t *exhaustion = NULL;
  rumqttc_error_t *last = NULL;
  rumqttc_native_failure_details_t details = RUMQTTC_NATIVE_FAILURE_DETAILS_INIT;
  CHECK(rumqttc_event_disconnected(event, NULL, &exhaustion));
  CHECK(rumqttc_error_native_failure_details(exhaustion, &details));
  REQUIRE(!details.present);
  CHECK(rumqttc_error_reconnect_last_error(exhaustion, &last, NULL));
  rumqttc_event_destroy(event);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_error_native_failure_details(last, &details));
  REQUIRE(details.present && details.domain == RUMQTTC_NATIVE_DOMAIN_IO);
  REQUIRE(details.io_kind == RUMQTTC_NATIVE_IO_CONNECTION_REFUSED);
  REQUIRE((details.fields & RUMQTTC_NATIVE_FIELD_OS_ERROR) != 0);
  REQUIRE(details.os_domain != RUMQTTC_NATIVE_OS_UNKNOWN);
  rumqttc_error_destroy(last);
  rumqttc_error_destroy(exhaustion);
}

int main(void) {
  const rumqttc_protocol_t protocols[] = {RUMQTTC_PROTOCOL_V4, RUMQTTC_PROTOCOL_V5};
  size_t i;
  for (i = 0; i < sizeof(protocols) / sizeof(protocols[0]); ++i) {
    connection_refused(protocols[i]);
    protocol_failure(protocols[i], "duplicate", RUMQTTC_NATIVE_FAILURE_DUPLICATE_CONN_ACK);
    protocol_failure(protocols[i], "unsolicited", RUMQTTC_NATIVE_FAILURE_UNSOLICITED_ACK);
    protocol_failure(protocols[i], "malformed", RUMQTTC_NATIVE_FAILURE_MALFORMED_PACKET);
    protocol_failure(protocols[i], "oversized", RUMQTTC_NATIVE_FAILURE_INCOMING_TOO_LARGE);
    protocol_failure(protocols[i], "ping", RUMQTTC_NATIVE_FAILURE_PING_RESPONSE_TIMEOUT);
  }
  return 0;
}
