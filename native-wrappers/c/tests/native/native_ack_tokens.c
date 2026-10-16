#include "native_common.h"

#include <stdatomic.h>

struct ack_worker {
  rumqttc_client_t *client;
  rumqttc_ack_token_t *token;
  const rumqttc_acknowledgement_options_t *options;
  atomic_uint *start;
  rumqttc_status_t status;
};

static int acknowledge(void *data) {
  struct ack_worker *worker = data;
  uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
  uint64_t operation = UINT64_MAX;
  while (!atomic_load(worker->start)) {
    REQUIRE(native_monotonic_ms() < deadline);
    native_sleep_ms(1);
  }
  worker->status = rumqttc_client_try_acknowledge_token_with_options(
      worker->client, worker->token, worker->options, &operation, NULL);
  REQUIRE(worker->status == RUMQTTC_OK ? operation != 0 : operation == 0);
  rumqttc_ack_token_release(worker->token);
  return 0;
}

static void exercise(rumqttc_protocol_t protocol) {
  const uint32_t reasons[] = {0, 0x80, 0x83, 0x87, 0x90, 0x91, 0x97, 0x99};
  rumqttc_client_t *client = native_start_client(
      protocol,
      protocol == RUMQTTC_PROTOCOL_V5 ? "native-ack-options-tokens-v5"
                                      : "native-ack-options-tokens-v4",
      RUMQTTC_ACK_MANUAL, 4, 8, NATIVE_DEADLINE_MS);
  for (unsigned qos = 1; qos <= 2; ++qos) {
    unsigned count = protocol == RUMQTTC_PROTOCOL_V5 ? 8 : 1;
    for (unsigned index = 0; index < count; ++index) {
      rumqttc_event_t *event =
          native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
      rumqttc_ack_token_t *token = NULL;
      rumqttc_completion_t *completion = NULL;
      rumqttc_v5_acknowledgement_options_t content =
          RUMQTTC_V5_ACKNOWLEDGEMENT_OPTIONS_INIT;
      rumqttc_acknowledgement_options_t options =
          RUMQTTC_ACKNOWLEDGEMENT_OPTIONS_INIT;
      rumqttc_user_property_t properties[2] = {RUMQTTC_USER_PROPERTY_INIT,
                                               RUMQTTC_USER_PROPERTY_INIT};
      uint64_t operation = UINT64_MAX;
      CHECK(rumqttc_event_take_ack_token(event, &token, NULL));
      REQUIRE(rumqttc_client_try_acknowledge(client, event, &operation, NULL) ==
              RUMQTTC_INVALID_STATE);
      REQUIRE(operation == 0);
      rumqttc_event_destroy(event);

      options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
      options.v5_options = &content;
      content.reason_code = protocol == RUMQTTC_PROTOCOL_V5 ? 0x10 : 0;
      REQUIRE(rumqttc_client_acknowledge_token_with_options_tracked(
                  client, token, &options, &completion, NULL) != RUMQTTC_OK);
      REQUIRE(completion == NULL);
      if (protocol == RUMQTTC_PROTOCOL_V4) {
        options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL;
        options.v5_options = NULL;
      } else {
        content.reason_code = reasons[index];
        content.reason_string_present = index % 2;
        content.reason_string = native_string("");
        if (index % 2) {
          properties[0].name = properties[1].name = native_string("k");
          properties[0].value = native_string("v");
          properties[1].value = native_string("");
          content.user_properties = properties;
          content.user_property_count = 2;
        }
      }
      if (index % 2) {
        atomic_uint start;
        struct ack_worker workers[2];
        native_thread_t *threads[2];
        atomic_init(&start, 0);
        for (unsigned i = 0; i < 2; ++i) {
          workers[i].client = client;
          workers[i].options = &options;
          workers[i].start = &start;
          workers[i].status = UINT32_MAX;
          CHECK(rumqttc_ack_token_retain(token, &workers[i].token, NULL));
          threads[i] = native_thread_start(acknowledge, &workers[i]);
        }
        atomic_store(&start, 1);
        for (unsigned i = 0; i < 2; ++i)
          REQUIRE(native_thread_join(threads[i]) == 0);
        REQUIRE((workers[0].status == RUMQTTC_OK) +
                    (workers[1].status == RUMQTTC_OK) ==
                1);
      } else {
        CHECK(rumqttc_client_acknowledge_token_with_options_tracked(
            client, token, &options, &completion, NULL));
        native_wait_completion(completion, RUMQTTC_COMPLETION_ACKNOWLEDGED);
        rumqttc_completion_destroy(completion);
      }
      REQUIRE(rumqttc_client_try_acknowledge_token(
                  client, token, &operation, NULL) == RUMQTTC_INVALID_STATE);
      REQUIRE(operation == 0);
      rumqttc_ack_token_release(token);
    }
  }
  /* The broker sends this after checking every ACK and QoS 2 response on the
   * wire. */
  rumqttc_event_t *done =
      native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
  rumqttc_event_destroy(done);
  native_close_destroy(client);
}

int main(void) {
  exercise(RUMQTTC_PROTOCOL_V4);
  exercise(RUMQTTC_PROTOCOL_V5);
  return 0;
}
