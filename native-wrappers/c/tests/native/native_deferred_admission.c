#include "native_common.h"

#include <stdatomic.h>
#include <stdio.h>

struct producer {
  rumqttc_client_t *client;
  atomic_uint *finished;
};

static rumqttc_completion_t *admitted(rumqttc_pending_admission_t *pending) {
  uint32_t state = UINT32_MAX;
  uint64_t operation = 0;
  rumqttc_completion_t *completion = NULL;
  /* ERROR_OUT_FAILURE: rumqttc_pending_admission_wait_timeout_ms */
  REQUIRE(rumqttc_pending_admission_wait_timeout_ms(NULL, 1, &state, &operation, &completion, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_pending_admission_wait_timeout_ms */
  CHECK(rumqttc_pending_admission_wait_timeout_ms(pending, NATIVE_DEADLINE_MS, &state, &operation, &completion, NULL));
  REQUIRE(state == RUMQTTC_ADMISSION_ADMITTED && operation != 0 && completion != NULL);
  /* ERROR_OUT_FAILURE: rumqttc_pending_admission_poll */
  REQUIRE(rumqttc_pending_admission_poll(NULL, &state, NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_pending_admission_poll */
  CHECK(rumqttc_pending_admission_poll(pending, &state, NULL, NULL, NULL));
  REQUIRE(state == RUMQTTC_ADMISSION_ADMITTED);
  return completion;
}

static int produce(void *data) {
  struct producer *producer = data;
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  for (unsigned i = 0; i < 16; ++i) {
    rumqttc_pending_admission_t *pending = NULL, *retained = NULL;
    rumqttc_completion_t *completion;
    size_t count = 0, bytes = 0;
    /* ERROR_OUT_FAILURE: rumqttc_client_publish_deferred */
    REQUIRE(rumqttc_client_publish_deferred(NULL, native_string("a"), native_bytes(NULL, 0), &options, &pending, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_client_publish_deferred */
    CHECK(rumqttc_client_publish_deferred(producer->client, native_string("rumqttc/native/deferred"), native_bytes((const uint8_t *)"data", 4), &options, &pending, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_pending_admission_retain */
    REQUIRE(rumqttc_pending_admission_retain(NULL, &retained, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_pending_admission_retain */
    CHECK(rumqttc_pending_admission_retain(pending, &retained, NULL));
    rumqttc_pending_admission_release(pending);
    completion = admitted(retained);
    /* ERROR_OUT_FAILURE: rumqttc_pending_admission_cancel */
    REQUIRE(rumqttc_pending_admission_cancel(NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_pending_admission_cancel */
    CHECK(rumqttc_pending_admission_cancel(retained, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    rumqttc_completion_destroy(completion);
    rumqttc_pending_admission_release(retained);
    /* ERROR_OUT_FAILURE: rumqttc_client_pending_admission_snapshot */
    REQUIRE(rumqttc_client_pending_admission_snapshot(NULL, &count, &bytes, NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_SUCCESS: rumqttc_client_pending_admission_snapshot */
    CHECK(rumqttc_client_pending_admission_snapshot(producer->client, &count, &bytes, NULL, NULL, NULL));
    REQUIRE(count <= 4 && bytes <= 4096);
  }
  atomic_fetch_add(producer->finished, 1);
  return 0;
}

static void exercise(rumqttc_protocol_t protocol, int shared) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_execution_context_t *context = NULL;
  rumqttc_pending_admission_limits_t limits = RUMQTTC_PENDING_ADMISSION_LIMITS_INIT;
  rumqttc_pending_admission_t *pending = NULL;
  rumqttc_completion_t *completion;
  rumqttc_subscription_t subscription = native_subscription("rumqttc/native/incoming", RUMQTTC_QOS_1);
  rumqttc_event_t *event;
  rumqttc_ack_token_t *token = NULL;
  atomic_uint finished;
  struct producer arguments;
  native_thread_t *threads[2];
  char client_id[64];
  uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
  REQUIRE(snprintf(client_id, sizeof(client_id), "native-deferred-%u-%d", (unsigned)protocol, shared) > 0);
  atomic_init(&finished, 0);
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  CHECK(rumqttc_config_set_request_capacity(config, 1, NULL));
  CHECK(rumqttc_config_set_ack_mode(config, RUMQTTC_ACK_MANUAL, NULL));
  limits.max_pending = 4;
  limits.max_bytes = 4096;
  /* ERROR_OUT_FAILURE: rumqttc_config_set_pending_admission_limits */
  REQUIRE(rumqttc_config_set_pending_admission_limits(NULL, &limits, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_pending_admission_limits */
  CHECK(rumqttc_config_set_pending_admission_limits(config, &limits, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_reset_pending_admission_limits */
  REQUIRE(rumqttc_config_reset_pending_admission_limits(NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_config_reset_pending_admission_limits */
  CHECK(rumqttc_config_reset_pending_admission_limits(config, NULL));
  CHECK(rumqttc_config_set_pending_admission_limits(config, &limits, NULL));
  if (shared) {
    CHECK(rumqttc_execution_context_new(NULL, &context, NULL));
    CHECK(rumqttc_config_set_execution_context(config, context, NULL));
  }
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_config_destroy(config);
  rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
  /* ERROR_OUT_FAILURE: rumqttc_client_subscribe_deferred */
  REQUIRE(rumqttc_client_subscribe_deferred(NULL, &subscription, 1, NULL, &pending, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_client_subscribe_deferred */
  CHECK(rumqttc_client_subscribe_deferred(client, &subscription, 1, NULL, &pending, NULL));
  completion = admitted(pending);
  native_wait_completion(completion, RUMQTTC_COMPLETION_SUBSCRIBE);
  rumqttc_completion_destroy(completion);
  rumqttc_pending_admission_release(pending);
  event = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
  CHECK(rumqttc_event_take_ack_token(event, &token, NULL));
  rumqttc_event_destroy(event);
  /* ERROR_OUT_FAILURE: rumqttc_client_acknowledge_token_deferred */
  REQUIRE(rumqttc_client_acknowledge_token_deferred(NULL, token, NULL, &pending, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_client_acknowledge_token_deferred */
  CHECK(rumqttc_client_acknowledge_token_deferred(client, token, NULL, &pending, NULL));
  completion = admitted(pending);
  native_wait_completion(completion, RUMQTTC_COMPLETION_ACKNOWLEDGED);
  rumqttc_completion_destroy(completion);
  rumqttc_pending_admission_release(pending);
  rumqttc_ack_token_release(token);
  /* ERROR_OUT_FAILURE: rumqttc_client_unsubscribe_deferred */
  REQUIRE(rumqttc_client_unsubscribe_deferred(NULL, &subscription.filter, 1, NULL, &pending, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_client_unsubscribe_deferred */
  CHECK(rumqttc_client_unsubscribe_deferred(client, &subscription.filter, 1, NULL, &pending, NULL));
  completion = admitted(pending);
  native_wait_completion(completion, RUMQTTC_COMPLETION_UNSUBSCRIBE);
  rumqttc_completion_destroy(completion);
  rumqttc_pending_admission_release(pending);
  arguments.client = client;
  arguments.finished = &finished;
  for (unsigned i = 0; i < 2; ++i) threads[i] = native_thread_start(produce, &arguments);
  while (atomic_load(&finished) != 2) {
    REQUIRE(native_monotonic_ms() < deadline);
    event = NULL;
    rumqttc_status_t status = rumqttc_client_event_recv_timeout_ms(client, 10, &event, NULL);
    REQUIRE(status == RUMQTTC_OK || status == RUMQTTC_TIMEOUT);
    rumqttc_event_destroy(event);
  }
  for (unsigned i = 0; i < 2; ++i) REQUIRE(native_thread_join(threads[i]) == 0);
  native_close_destroy(client);
  if (context != NULL) {
    CHECK(rumqttc_execution_context_request_shutdown(context, NULL));
    CHECK(rumqttc_execution_context_join_timeout_ms(context, NATIVE_DEADLINE_MS, NULL));
    rumqttc_execution_context_release(context);
  }
}

int main(void) {
  REQUIRE((rumqttc_library_capabilities() & RUMQTTC_CAP_DEFERRED_ADMISSION) != 0);
  for (unsigned protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol)
    for (int shared = 0; shared < 2; ++shared) exercise(protocol, shared);
  return 0;
}
