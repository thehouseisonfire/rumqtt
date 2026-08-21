#include "native_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const uint8_t BINARY_PAYLOAD[] = {0, 1, 0, 2, 255, 0};
void native_test_error_out_contract(void);
void native_test_protocol_options(void);

static void test_protocol_round_trip(rumqttc_protocol_t protocol) {
  const rumqttc_completion_kind_t expected[] = {
      RUMQTTC_COMPLETION_QOS0_FLUSHED,
      RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED,
      RUMQTTC_COMPLETION_QOS2_COMPLETED,
  };
  rumqttc_client_t *client = native_start_client(
      protocol, protocol == RUMQTTC_PROTOCOL_V4 ? "native-v4" : "native-v5",
      RUMQTTC_ACK_AUTOMATIC, 16, 16, 1000);
  size_t qos;
  for (qos = 0; qos <= 2; ++qos) {
    rumqttc_publish_options_t options =
        native_publish_options((rumqttc_qos_t)qos);
    rumqttc_completion_t *completion = NULL;
    CHECK(rumqttc_client_publish_tracked(
        client, native_string("rumqttc/native/binary"),
        native_bytes(BINARY_PAYLOAD, sizeof(BINARY_PAYLOAD)), &options,
        &completion, NULL));
    native_wait_completion(completion, expected[qos]);
    rumqttc_completion_destroy(completion);
  }

  {
    rumqttc_subscription_t subscription =
        native_subscription("rumqttc/native/incoming", RUMQTTC_QOS_1);
    rumqttc_completion_t *completion = NULL;
    rumqttc_event_t *event;
    rumqttc_string_view_t topic = {NULL, 0};
    rumqttc_bytes_view_t payload = {NULL, 0};
    rumqttc_qos_t qos_value = 0;
    uint8_t retain = 0, duplicate = 0, ack_available = 0;
    CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, NULL,
                                           &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_SUBSCRIBE);
    rumqttc_completion_destroy(completion);
    event = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
    CHECK(rumqttc_event_publish(event, &topic, &payload, &qos_value, &retain,
                                &duplicate, &ack_available));
    REQUIRE(topic.len == strlen("rumqttc/native/incoming"));
    REQUIRE(payload.len == 8 && memcmp(payload.data, "\0native\0", 8) == 0);
    REQUIRE(qos_value == RUMQTTC_QOS_1 && ack_available == 0);
    CHECK(rumqttc_event_publish(event, NULL, &payload, NULL, NULL, NULL, NULL));
    REQUIRE(payload.len == 8);
    CHECK(rumqttc_event_publish(event, &topic, NULL, NULL, NULL, NULL, NULL));
    REQUIRE(topic.len == strlen("rumqttc/native/incoming"));
    REQUIRE(rumqttc_event_publish(event, NULL, NULL, NULL, NULL, NULL, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    if (protocol == RUMQTTC_PROTOCOL_V5) {
      uint8_t present = 0;
      uint64_t scalar = 0;
      rumqttc_string_view_t string_value = {(const char *)(uintptr_t)1, 99};
      rumqttc_bytes_view_t bytes_value = {(const uint8_t *)(uintptr_t)1, 99};
      REQUIRE(rumqttc_event_v5_scalar(event, 99, &present, &scalar) ==
              RUMQTTC_INVALID_ARGUMENT);
      CHECK(rumqttc_event_v5_scalar(event, RUMQTTC_V5_SCALAR_PAYLOAD_FORMAT,
                                    &present, NULL));
      REQUIRE(present == 1);
      CHECK(rumqttc_event_v5_scalar(event, RUMQTTC_V5_SCALAR_PAYLOAD_FORMAT,
                                    NULL, &scalar));
      REQUIRE(scalar == 0);
      REQUIRE(rumqttc_event_v5_scalar(event, RUMQTTC_V5_SCALAR_PAYLOAD_FORMAT,
                                      NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
      CHECK(rumqttc_event_v5_response_topic(event, &present, NULL));
      CHECK(rumqttc_event_v5_response_topic(event, NULL, &string_value));
      REQUIRE(string_value.data == NULL && string_value.len == 0);
      REQUIRE(rumqttc_event_v5_response_topic(event, NULL, NULL) ==
              RUMQTTC_INVALID_ARGUMENT);
      CHECK(rumqttc_event_v5_correlation_data(event, &present, NULL));
      REQUIRE(present == 1);
      CHECK(rumqttc_event_v5_correlation_data(event, NULL, &bytes_value));
      REQUIRE(bytes_value.len == 3 &&
              memcmp(bytes_value.data, "\0\5\0", 3) == 0);
      CHECK(rumqttc_event_v5_content_type(event, &present, NULL));
      CHECK(rumqttc_event_v5_content_type(event, NULL, &string_value));
      string_value.data = (const char *)(uintptr_t)1;
      string_value.len = 99;
      CHECK(rumqttc_event_v5_user_property_at(event, 0, &string_value, NULL));
      REQUIRE(string_value.len == 1 && string_value.data[0] == 'k');
      CHECK(rumqttc_event_v5_user_property_at(event, 0, NULL, &string_value));
      REQUIRE(string_value.len == 1 && string_value.data[0] == 'v');
      REQUIRE(rumqttc_event_v5_user_property_at(event, 0, NULL, NULL) ==
              RUMQTTC_INVALID_ARGUMENT);
    }
    rumqttc_event_destroy(event);

    completion = NULL;
    {
      rumqttc_string_view_t filter = native_string("rumqttc/native/incoming");
      CHECK(rumqttc_client_unsubscribe_tracked(client, &filter, 1, NULL, &completion,
                                               NULL));
    }
    native_wait_completion(completion, RUMQTTC_COMPLETION_UNSUBSCRIBE);
    rumqttc_completion_destroy(completion);
  }
  CHECK(rumqttc_client_close_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));
}

static void test_manual_ack(rumqttc_protocol_t protocol) {
  rumqttc_client_t *client = native_start_client(
      protocol, "native-manual", RUMQTTC_ACK_MANUAL, 16, 16, 1000);
  rumqttc_subscription_t subscription =
      native_subscription("rumqttc/native/incoming", RUMQTTC_QOS_1);
  rumqttc_completion_t *completion = NULL;
  rumqttc_event_t *event;
  uint8_t ack_available = 0, retain = 0, duplicate = 0;
  rumqttc_qos_t qos = 0;
  rumqttc_string_view_t topic;
  rumqttc_bytes_view_t payload;
  CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, NULL, &completion,
                                         NULL));
  native_wait_completion(completion, RUMQTTC_COMPLETION_SUBSCRIBE);
  rumqttc_completion_destroy(completion);
  event = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
  CHECK(rumqttc_event_publish(event, &topic, &payload, &qos, &retain,
                              &duplicate, &ack_available));
  REQUIRE(ack_available == 1);
  completion = NULL;
  CHECK(rumqttc_client_acknowledge_tracked(client, event, &completion, NULL));
  native_wait_completion(completion, RUMQTTC_COMPLETION_ACKNOWLEDGED);
  rumqttc_completion_destroy(completion);
  completion = (rumqttc_completion_t *)(uintptr_t)1;
  REQUIRE(rumqttc_client_acknowledge_tracked(client, event, &completion,
                                             NULL) == RUMQTTC_INVALID_STATE);
  REQUIRE(completion == NULL);
  rumqttc_event_destroy(event);
  native_close_destroy(client);
}

static void test_invalid_inputs(void) {
  rumqttc_config_t *config = (rumqttc_config_t *)(uintptr_t)1;
  rumqttc_error_t *error = NULL;
  rumqttc_string_view_t null_string = {NULL, 1};
  rumqttc_bytes_view_t null_bytes = {NULL, 1};
  rumqttc_publish_options_t options = native_publish_options(99);
  size_t required = 0;
  REQUIRE(rumqttc_config_new(99, &config, &error) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(config == NULL && error != NULL);
  {
    uint8_t value = 99;
    uint8_t present = 99;
    uint64_t operation = UINT64_MAX;
    CHECK(rumqttc_error_flags(error, &value, NULL));
    CHECK(rumqttc_error_flags(error, NULL, &value));
    CHECK(rumqttc_error_broker_reason(error, &present, NULL));
    CHECK(rumqttc_error_broker_reason(error, NULL, &value));
    CHECK(rumqttc_error_operation_id(error, &present, NULL));
    CHECK(rumqttc_error_operation_id(error, NULL, &operation));
    REQUIRE(rumqttc_error_flags(error, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    value = 99;
    REQUIRE(rumqttc_error_flags(NULL, &value, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(value == 0);
  }
  rumqttc_error_destroy(error);
  REQUIRE(rumqttc_config_new(99, &config, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, NULL, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &config, NULL));
  REQUIRE(rumqttc_config_set_client_id(config, null_string, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  {
    const unsigned char invalid_utf8[] = {0xff};
    rumqttc_string_view_t invalid = {(const char *)invalid_utf8,
                                     sizeof(invalid_utf8)};
    REQUIRE(rumqttc_config_set_client_id(config, invalid, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
  }
  REQUIRE(rumqttc_config_set_ack_mode(config, 99, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_set_password(config, null_bytes, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_config_set_broker(NULL, native_string("x"), 1, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_string_copy(null_string, NULL, 0, &required) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_bytes_copy(null_bytes, NULL, 0, &required) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(options.qos == 99);
  rumqttc_config_destroy(config);
  rumqttc_config_destroy(NULL);
  rumqttc_completion_destroy(NULL);
  rumqttc_event_destroy(NULL);
  rumqttc_error_destroy(NULL);
  CHECK(rumqttc_client_destroy_timeout_ms(NULL, 5000, NULL));
}

static void test_reconnect(rumqttc_protocol_t protocol) {
  rumqttc_client_t *client = native_start_client(
      protocol, "native-reconnect", RUMQTTC_ACK_AUTOMATIC, 16, 16, 1000);
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_0);
  rumqttc_completion_t *completion = NULL;
  rumqttc_event_t *event;
  CHECK(rumqttc_client_publish_tracked(
      client, native_string("rumqttc/native/interrupt"), native_bytes(NULL, 0),
      &options, &completion, NULL));
  native_wait_completion(completion, RUMQTTC_COMPLETION_QOS0_FLUSHED);
  rumqttc_completion_destroy(completion);
  event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
  {
    uint32_t phase = 0;
    rumqttc_error_t *event_error = NULL;
    CHECK(rumqttc_event_disconnected(event, &phase, NULL));
    REQUIRE(phase == RUMQTTC_CONNECTION_PHASE_ESTABLISHED);
    CHECK(rumqttc_event_disconnected(event, NULL, &event_error));
    REQUIRE(event_error != NULL);
    rumqttc_error_destroy(event_error);
    REQUIRE(rumqttc_event_disconnected(event, NULL, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
  }
  rumqttc_event_destroy(event);
  event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(event);
  native_close_destroy(client);
}

static void test_backpressure_and_overflow(void) {
  rumqttc_client_t *client =
      native_start_client(RUMQTTC_PROTOCOL_V4, "native-pressure",
                          RUMQTTC_ACK_AUTOMATIC, 1, 8, 1000);
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  uint64_t operation = 0;
  size_t index;
  int saw_backpressure = 0;
  for (index = 0; index < 4096; ++index) {
    rumqttc_status_t status = rumqttc_client_try_publish(
        client, native_string("rumqttc/native/stall"),
        native_bytes(BINARY_PAYLOAD, 1), &options, &operation, NULL);
    if (status == RUMQTTC_BACKPRESSURE) {
      saw_backpressure = 1;
      break;
    }
    REQUIRE(status == RUMQTTC_OK);
  }
  REQUIRE(saw_backpressure);
  native_close_destroy(client);

  client = native_start_client(RUMQTTC_PROTOCOL_V4, "native-overflow",
                               RUMQTTC_ACK_AUTOMATIC, 8, 1, 50);
  {
    rumqttc_subscription_t subscription =
        native_subscription("rumqttc/native/overflow", RUMQTTC_QOS_0);
    rumqttc_completion_t *completion = NULL;
    rumqttc_event_t *event;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, NULL,
                                           &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_SUBSCRIBE);
    rumqttc_completion_destroy(completion);
    native_sleep_ms(200);
    event = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
    rumqttc_event_destroy(event);
    event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
    CHECK(rumqttc_event_kind(event, &kind));
    REQUIRE(kind == RUMQTTC_EVENT_DRIVER_TERMINATED);
    rumqttc_event_destroy(event);
  }
  CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));
}

typedef struct publish_thread_args {
  rumqttc_client_t *client;
  unsigned index;
} publish_thread_args_t;

static int publish_thread(void *opaque) {
  publish_thread_args_t *args = opaque;
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  unsigned index;
  for (index = 0; index < 20; ++index) {
    rumqttc_completion_t *completion = NULL;
    CHECK(rumqttc_client_publish_tracked(
        args->client, native_string("rumqttc/native/concurrent"),
        native_bytes((const uint8_t *)&args->index, sizeof(args->index)),
        &options, &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    rumqttc_completion_destroy(completion);
  }
  return 0;
}

static int blocking_receiver(void *opaque) {
  rumqttc_client_t *client = opaque;
  rumqttc_event_t *event = NULL;
  rumqttc_status_t status =
      rumqttc_client_event_recv_timeout_ms(client, 500, &event, NULL);
  rumqttc_event_destroy(event);
  return status == RUMQTTC_TIMEOUT ? 0 : 1;
}

static void test_native_concurrency(void) {
  enum { THREADS = 4 };
  rumqttc_client_t *client =
      native_start_client(RUMQTTC_PROTOCOL_V5, "native-concurrent",
                          RUMQTTC_ACK_AUTOMATIC, 128, 32, 1000);
  native_thread_t *threads[THREADS];
  publish_thread_args_t args[THREADS];
  unsigned index;
  for (index = 0; index < THREADS; ++index) {
    args[index].client = client;
    args[index].index = index;
    threads[index] = native_thread_start(publish_thread, &args[index]);
  }
  for (index = 0; index < THREADS; ++index) {
    REQUIRE(native_thread_join(threads[index]) == 0);
  }
  {
    native_thread_t *receiver = native_thread_start(blocking_receiver, client);
    rumqttc_event_t *event = NULL;
    native_sleep_ms(50);
    REQUIRE(rumqttc_client_event_recv_timeout_ms(client, 10, &event, NULL) ==
            RUMQTTC_INVALID_STATE);
    REQUIRE(event == NULL);
    REQUIRE(native_thread_join(receiver) == 0);
  }
  native_close_destroy(client);
}

static void test_close_and_pending_handles(void) {
  rumqttc_client_t *client = native_start_client(
      RUMQTTC_PROTOCOL_V4, "native-close", RUMQTTC_ACK_AUTOMATIC, 8, 8, 1000);
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *completion = NULL;
  rumqttc_error_t *error = NULL;
  CHECK(rumqttc_client_publish_tracked(
      client, native_string("rumqttc/native/stall"),
      native_bytes(BINARY_PAYLOAD, 2), &options, &completion, NULL));
  REQUIRE(rumqttc_client_close_timeout_ms(client, 25, &error) ==
          RUMQTTC_TIMEOUT);
  REQUIRE(error != NULL);
  rumqttc_error_destroy(error);
  rumqttc_completion_destroy(completion);
  CHECK(rumqttc_client_close_now_timeout_ms(client, 5000, NULL));
  CHECK(rumqttc_client_close_now_timeout_ms(client, 5000, NULL));
  CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));

  client = native_start_client(RUMQTTC_PROTOCOL_V5, "native-event-destroy",
                               RUMQTTC_ACK_AUTOMATIC, 8, 16, 1000);
  {
    rumqttc_subscription_t subscription =
        native_subscription("rumqttc/native/overflow", RUMQTTC_QOS_0);
    rumqttc_completion_t *subscribe = NULL;
    rumqttc_event_t *retained_event;
    CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, NULL, &subscribe,
                                           NULL));
    native_wait_completion(subscribe, RUMQTTC_COMPLETION_SUBSCRIBE);
    rumqttc_completion_destroy(subscribe);
    retained_event = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
    CHECK(rumqttc_client_close_now_timeout_ms(client, 5000, NULL));
    CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));
    /* Event storage is independently owned and remains destroyable post-client.
     */
    rumqttc_event_destroy(retained_event);
  }
}

int main(void) {
  test_invalid_inputs();
  native_test_error_out_contract();
  native_test_protocol_options();
  test_protocol_round_trip(RUMQTTC_PROTOCOL_V4);
  test_protocol_round_trip(RUMQTTC_PROTOCOL_V5);
  test_manual_ack(RUMQTTC_PROTOCOL_V4);
  test_manual_ack(RUMQTTC_PROTOCOL_V5);
  test_reconnect(RUMQTTC_PROTOCOL_V4);
  test_reconnect(RUMQTTC_PROTOCOL_V5);
  test_backpressure_and_overflow();
  test_native_concurrency();
  test_close_and_pending_handles();
  puts("native C integration suite passed");
  return 0;
}
