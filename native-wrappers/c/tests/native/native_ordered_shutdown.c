#include "native_common.h"

#include <stdio.h>
#include <string.h>

typedef struct producer {
  rumqttc_client_t *client;
  uint8_t value;
  uint64_t id;
  rumqttc_completion_t *completion;
} producer;

static int produce(void *argument) {
  producer *item = argument;
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  CHECK(rumqttc_client_publish_tracked(item->client, native_string("native/ordered/b"), native_bytes(&item->value, 1),
                                       &options, &item->completion, NULL));
  CHECK(rumqttc_completion_operation_id(item->completion, &item->id));
  return 0;
}

static rumqttc_client_t *start(rumqttc_protocol_t protocol, const char *id) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
  CHECK(rumqttc_config_set_request_capacity(config, 64, NULL));
  CHECK(rumqttc_config_set_max_request_batch(config, 1, NULL));
  if (protocol == RUMQTTC_PROTOCOL_V4)
    CHECK(rumqttc_config_set_v4_inflight_limit(config, 1, NULL));
  else
    CHECK(rumqttc_config_set_v5_outgoing_inflight_upper_limit(config, 1, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_config_destroy(config);
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(event);
  return client;
}

static void concurrent_case(rumqttc_protocol_t protocol) {
  const char *id = protocol == RUMQTTC_PROTOCOL_V4 ? "native-ordered-v4" : "native-ordered-v5";
  rumqttc_client_t *client = start(protocol, id);
  rumqttc_publish_options_t publish = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *first = NULL, *close = NULL;
  CHECK(rumqttc_client_publish_tracked(client, native_string("native/close/stall"), native_bytes(NULL, 0), &publish,
                                       &first, NULL));
  rumqttc_event_t *ready = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
  rumqttc_event_destroy(ready);
  producer items[16];
  native_thread_t *workers[16];
  memset(items, 0, sizeof(items));
  for (unsigned index = 0; index < 16; ++index) {
    items[index].client = client;
    items[index].value = (uint8_t)index;
    workers[index] = native_thread_start(produce, &items[index]);
  }
  for (unsigned index = 0; index < 16; ++index)
    REQUIRE(native_thread_join(workers[index]) == 0);
  rumqttc_v5_disconnect_properties_t properties = RUMQTTC_V5_DISCONNECT_PROPERTIES_INIT;
  rumqttc_disconnect_options_t options = RUMQTTC_DISCONNECT_OPTIONS_INIT;
  char reason[] = "ordered";
  if (protocol == RUMQTTC_PROTOCOL_V5) {
    properties.reason_string_present = 1;
    properties.reason_string = native_string(reason);
    options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
    options.v5_properties = &properties;
  }
  CHECK(rumqttc_client_disconnect_after_queued_with_options_timeout_ms_tracked(client, NATIVE_DEADLINE_MS, &options,
                                                                               &close, NULL));
  memset(reason, 'x', sizeof(reason) - 1);
  uint64_t late_id = 99;
  REQUIRE(rumqttc_client_try_publish(client, native_string("late"), native_bytes(NULL, 0), &publish, &late_id, NULL) ==
          RUMQTTC_INVALID_STATE);
  REQUIRE(late_id == 0);
  REQUIRE(rumqttc_client_close_timeout_ms(client, 0, NULL) != RUMQTTC_OK);
  REQUIRE(rumqttc_completion_wait_timeout_ms(close, 0, NULL) == RUMQTTC_TIMEOUT);
  rumqttc_completion_t *diagnostics = NULL;
  CHECK(rumqttc_client_diagnostics_tracked(client, &diagnostics, NULL));
  native_wait_completion(diagnostics, RUMQTTC_COMPLETION_DIAGNOSTICS);
  rumqttc_ordered_shutdown_diagnostics_t snapshot = RUMQTTC_ORDERED_SHUTDOWN_DIAGNOSTICS_INIT;
  CHECK(rumqttc_completion_ordered_shutdown_diagnostics(diagnostics, &snapshot, NULL));
  REQUIRE(snapshot.present && snapshot.fence_present && snapshot.deadline_present);
  struct {
    uint32_t struct_size;
    uint32_t sentinel;
  } small = {sizeof(uint32_t), UINT32_MAX};
  REQUIRE(rumqttc_completion_ordered_shutdown_diagnostics(diagnostics, (rumqttc_ordered_shutdown_diagnostics_t *)&small,
                                                          NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(small.sentinel == UINT32_MAX);
  rumqttc_completion_t *duplicate = (rumqttc_completion_t *)(uintptr_t)1;
  REQUIRE(rumqttc_client_disconnect_after_queued_timeout_ms_tracked(client, 0, &duplicate, NULL) ==
              RUMQTTC_INVALID_STATE &&
          duplicate == NULL);
  rumqttc_completion_destroy(diagnostics);

  /* Tell the independent broker the actual successful admission order before releasing A. */
  uint8_t release[128];
  size_t prefix = strlen(id);
  memcpy(release, id, prefix);
  release[prefix++] = '|';
  for (unsigned slot = 0; slot < 16; ++slot) {
    unsigned selected = 0;
    for (unsigned index = 1; index < 16; ++index)
      if (items[index].id < items[selected].id)
        selected = index;
    release[prefix + slot] = items[selected].value;
    items[selected].id = UINT64_MAX;
  }
  rumqttc_client_t *control =
      native_start_client(protocol, "native-ordered-control", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
  rumqttc_completion_t *sent = NULL;
  publish.qos = RUMQTTC_QOS_0;
  CHECK(rumqttc_client_publish_tracked(control, native_string("native/ordered/release"),
                                       native_bytes(release, prefix + 16), &publish, &sent, NULL));
  native_wait_completion(sent, RUMQTTC_COMPLETION_QOS0_FLUSHED);
  rumqttc_completion_destroy(sent);
  native_close_destroy(control);
  native_wait_completion(close, RUMQTTC_COMPLETION_ORDERED_SHUTDOWN);
  properties.reason_string = native_string("ordered");
  CHECK(rumqttc_client_close_after_queued_with_options_timeout_ms(client, NATIVE_DEADLINE_MS, &options, NULL));
  CHECK(rumqttc_client_close_after_queued_with_options_timeout_ms(client, NATIVE_DEADLINE_MS, &options, NULL));
  for (unsigned index = 0; index < 16; ++index) {
    native_wait_completion(items[index].completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    rumqttc_completion_destroy(items[index].completion);
  }
  native_wait_completion(first, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  rumqttc_completion_destroy(first);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  native_wait_completion(close, RUMQTTC_COMPLETION_ORDERED_SHUTDOWN);
  rumqttc_completion_destroy(close);
}

static void qos_case(rumqttc_protocol_t protocol, rumqttc_qos_t qos) {
  rumqttc_client_t *client = native_start_client(protocol, "native-ordered-qos", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
  rumqttc_publish_options_t options = native_publish_options(qos);
  rumqttc_completion_t *publication = NULL, *close = NULL;
  CHECK(rumqttc_client_publish_tracked(client, native_string("native/ordered/qos"),
                                       native_bytes((const uint8_t *)"qos", 3), &options, &publication, NULL));
  CHECK(rumqttc_client_disconnect_after_queued_timeout_ms_tracked(client, NATIVE_DEADLINE_MS, &close, NULL));
  native_wait_completion(close, RUMQTTC_COMPLETION_ORDERED_SHUTDOWN);
  native_wait_completion(publication,
                         qos == RUMQTTC_QOS_0 ? RUMQTTC_COMPLETION_QOS0_FLUSHED : RUMQTTC_COMPLETION_QOS2_COMPLETED);
  rumqttc_ordered_shutdown_diagnostics_t wrong_kind = RUMQTTC_ORDERED_SHUTDOWN_DIAGNOSTICS_INIT;
  REQUIRE(rumqttc_completion_ordered_shutdown_diagnostics(publication, &wrong_kind, NULL) == RUMQTTC_INVALID_STATE);
  REQUIRE(wrong_kind.present == 0);
  rumqttc_completion_destroy(publication);
  /* Destroying the foreign observer does not cancel the admitted native fence. */
  rumqttc_completion_destroy(close);
  CHECK(rumqttc_client_close_after_queued_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
}

static void timeout_context_case(rumqttc_protocol_t protocol) {
  rumqttc_client_t *client = start(protocol, "native-ordered-timeout-context");
  rumqttc_completion_t *close = NULL;
  rumqttc_error_t *error = NULL;
  CHECK(rumqttc_client_disconnect_after_queued_timeout_ms_tracked(client, 0, &close, NULL));
  uint64_t operation = 0;
  CHECK(rumqttc_completion_operation_id(close, &operation));
  for (unsigned observation = 0; observation < 3; ++observation) {
    if (observation == 1) {
      REQUIRE(rumqttc_client_close_after_queued_timeout_ms(client, NATIVE_DEADLINE_MS, &error) == RUMQTTC_TIMEOUT);
      CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    } else {
      REQUIRE(rumqttc_completion_wait_timeout_ms(close, NATIVE_DEADLINE_MS, &error) == RUMQTTC_TIMEOUT);
    }
    uint8_t present = 0;
    uint32_t failure = 0, observed_protocol = 0, phase = 0, delivery = 0;
    uint64_t generation = 0, observed_operation = 0;
    CHECK(rumqttc_error_ordered_disconnect_failure(error, &present, &failure));
    REQUIRE(present && failure == RUMQTTC_ORDERED_FAILURE_TIMEOUT);
    CHECK(rumqttc_error_context(error, &observed_protocol, &phase, &present, &generation, &delivery));
    REQUIRE(observed_protocol == protocol && phase == RUMQTTC_CONNECTION_PHASE_ESTABLISHED);
    REQUIRE(present && generation == 1 && delivery == RUMQTTC_DELIVERY_AMBIGUOUS);
    CHECK(rumqttc_error_operation_id(error, &present, &observed_operation));
    REQUIRE(present && observed_operation == operation);
    rumqttc_error_destroy(error);
    error = NULL;
  }
  rumqttc_completion_destroy(close);
}

static void rejected_case(void) {
  rumqttc_client_t *client =
      native_start_client(RUMQTTC_PROTOCOL_V5, "native-ordered-rejection", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *publication = NULL, *close = NULL;
  rumqttc_error_t *error = NULL;
  CHECK(rumqttc_client_publish_tracked(client, native_string("rumqttc/native/reject"), native_bytes(NULL, 0), &options,
                                       &publication, NULL));
  REQUIRE(rumqttc_completion_wait_timeout_ms(publication, NATIVE_DEADLINE_MS, &error) == RUMQTTC_BROKER_REJECTED);
  rumqttc_error_destroy(error);
  error = NULL;
  CHECK(rumqttc_client_disconnect_after_queued_timeout_ms_tracked(client, NATIVE_DEADLINE_MS, &close, NULL));
  REQUIRE(rumqttc_completion_wait_timeout_ms(close, NATIVE_DEADLINE_MS, &error) == RUMQTTC_AMBIGUOUS);
  uint8_t present = 0;
  uint32_t failure = 0;
  CHECK(rumqttc_error_ordered_disconnect_failure(error, &present, &failure));
  REQUIRE(present && failure == RUMQTTC_ORDERED_FAILURE_PUBLISH);
  uint8_t reason = 0;
  CHECK(rumqttc_error_broker_reason(error, &present, &reason));
  REQUIRE(present && reason == 0x87);
  rumqttc_error_destroy(error);
  rumqttc_completion_destroy(publication);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  REQUIRE(rumqttc_completion_wait_timeout_ms(close, 0, NULL) == RUMQTTC_AMBIGUOUS);
  rumqttc_completion_destroy(close);
}

int main(void) {
  if (!(rumqttc_library_capabilities() & RUMQTTC_CAP_ORDERED_SHUTDOWN)) {
    rumqttc_client_t *client =
        native_start_client(RUMQTTC_PROTOCOL_V4, "native-disabled-ordered", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
    uint64_t id = 99;
    rumqttc_completion_t *completion = (rumqttc_completion_t *)(uintptr_t)1;
    REQUIRE(rumqttc_client_try_disconnect_after_queued(client, &id, NULL) == RUMQTTC_CONFIG_ERROR && id == 0);
    REQUIRE(rumqttc_client_disconnect_after_queued_tracked(client, &completion, NULL) == RUMQTTC_CONFIG_ERROR &&
            completion == NULL);
    REQUIRE(rumqttc_client_try_disconnect_after_queued_timeout_ms(client, 0, &id, NULL) == RUMQTTC_CONFIG_ERROR &&
            id == 0);
    REQUIRE(rumqttc_client_try_disconnect_after_queued_with_options(client, NULL, &id, NULL) == RUMQTTC_CONFIG_ERROR &&
            id == 0);
    REQUIRE(rumqttc_client_try_disconnect_after_queued_with_options_timeout_ms(client, 0, NULL, &id, NULL) ==
                RUMQTTC_CONFIG_ERROR &&
            id == 0);
    REQUIRE(rumqttc_client_disconnect_after_queued_timeout_ms_tracked(client, 0, &completion, NULL) ==
                RUMQTTC_CONFIG_ERROR &&
            completion == NULL);
    REQUIRE(rumqttc_client_disconnect_after_queued_with_options_tracked(client, NULL, &completion, NULL) ==
                RUMQTTC_CONFIG_ERROR &&
            completion == NULL);
    REQUIRE(rumqttc_client_disconnect_after_queued_with_options_timeout_ms_tracked(client, 0, NULL, &completion,
                                                                                   NULL) == RUMQTTC_CONFIG_ERROR &&
            completion == NULL);
    REQUIRE(rumqttc_client_close_after_queued_timeout_ms(client, 0, NULL) == RUMQTTC_CONFIG_ERROR);
    REQUIRE(rumqttc_client_close_after_queued_with_options_timeout_ms(client, 0, NULL, NULL) == RUMQTTC_CONFIG_ERROR);
    /* Unsupported calls must leave ordinary publish admission and close usable. */
    rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_0);
    CHECK(rumqttc_client_publish_tracked(client, native_string("native/ordered/disabled"), native_bytes(NULL, 0),
                                         &options, &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_QOS0_FLUSHED);
    rumqttc_completion_destroy(completion);
    native_close_destroy(client);
    return 0;
  }
  qos_case(RUMQTTC_PROTOCOL_V4, RUMQTTC_QOS_0);
  qos_case(RUMQTTC_PROTOCOL_V5, RUMQTTC_QOS_0);
  qos_case(RUMQTTC_PROTOCOL_V4, RUMQTTC_QOS_2);
  qos_case(RUMQTTC_PROTOCOL_V5, RUMQTTC_QOS_2);
  timeout_context_case(RUMQTTC_PROTOCOL_V4);
  timeout_context_case(RUMQTTC_PROTOCOL_V5);
  rejected_case();
  concurrent_case(RUMQTTC_PROTOCOL_V4);
  concurrent_case(RUMQTTC_PROTOCOL_V5);
  return 0;
}
