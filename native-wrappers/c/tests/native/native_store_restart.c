#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

typedef struct store_context {
  uint8_t *checkpoint;
  size_t length;
  unsigned loads;
  unsigned saves;
  unsigned clears;
  atomic_uint active;
  uint32_t last_operation;
} store_context;

static atomic_uint destroyed;

static void load_checkpoint(void *user_data,
                            const rumqttc_store_request_t *request,
                            rumqttc_callback_completion_t *completion) {
  store_context *context = (store_context *)user_data;
  REQUIRE(atomic_fetch_add(&context->active, 1) == 0);
  context->last_operation = RUMQTTC_STORE_LOAD;
  REQUIRE(request->checkpoint.len == 0);
  ++context->loads;
  if (context->checkpoint == NULL) {
    CHECK(rumqttc_callback_store_load_complete(
        completion, RUMQTTC_STORE_NOT_FOUND,
        native_bytes(NULL, 0)));
  } else {
    CHECK(rumqttc_callback_store_load_complete(
        completion, RUMQTTC_STORE_FOUND,
        native_bytes(context->checkpoint, context->length)));
  }
  REQUIRE(atomic_fetch_sub(&context->active, 1) == 1);
}

static void save_checkpoint(void *user_data,
                            const rumqttc_store_request_t *request,
                            rumqttc_callback_completion_t *completion) {
  store_context *context = (store_context *)user_data;
  REQUIRE(atomic_fetch_add(&context->active, 1) == 0);
  REQUIRE(context->last_operation != 0);
  context->last_operation = RUMQTTC_STORE_SAVE;
  uint8_t *copy = malloc(request->checkpoint.len ? request->checkpoint.len : 1);
  REQUIRE(copy != NULL);
  if (request->checkpoint.len)
    memcpy(copy, request->checkpoint.data, request->checkpoint.len);
  free(context->checkpoint);
  context->checkpoint = copy;
  context->length = request->checkpoint.len;
  ++context->saves;
  CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
  REQUIRE(atomic_fetch_sub(&context->active, 1) == 1);
}

static void clear_checkpoint(void *user_data,
                             const rumqttc_store_request_t *request,
                             rumqttc_callback_completion_t *completion) {
  store_context *context = (store_context *)user_data;
  REQUIRE(atomic_fetch_add(&context->active, 1) == 0);
  REQUIRE(context->last_operation == RUMQTTC_STORE_LOAD);
  context->last_operation = RUMQTTC_STORE_CLEAR;
  (void)request;
  free(context->checkpoint);
  context->checkpoint = NULL;
  context->length = 0;
  ++context->clears;
  CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
  REQUIRE(atomic_fetch_sub(&context->active, 1) == 1);
}

static void destroy_store(void *user_data) {
  store_context *context = (store_context *)user_data;
  REQUIRE(atomic_load(&context->active) == 0);
  free(context->checkpoint);
  free(context);
  atomic_fetch_add(&destroyed, 1);
}

static void wait_restart_activity(rumqttc_client_t *client, int restored) {
  unsigned connected = !restored;
  unsigned published = 0;
  unsigned subscribed = 0;
  unsigned acknowledged = 0;
  unsigned incoming = 0;
  while (!connected || published < 2 || subscribed < 1 || acknowledged < 1 ||
         (!restored && incoming < 1)) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS,
                                               &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    native_check_event_accessors(event);
    if (kind == RUMQTTC_EVENT_CONNECTED) {
      connected = 1;
    } else if (kind == RUMQTTC_EVENT_OUTGOING) {
      uint32_t outgoing = 0;
      CHECK(rumqttc_event_outgoing_kind(event, &outgoing));
      if (outgoing == RUMQTTC_OUTGOING_PUBLISH)
        ++published;
      else if (outgoing == RUMQTTC_OUTGOING_SUBSCRIBE)
        ++subscribed;
      else if (outgoing == RUMQTTC_OUTGOING_ACKNOWLEDGEMENT) {
        uint8_t present = 0;
        uint16_t packet_id = 0;
        CHECK(rumqttc_event_outgoing_packet_id(event, &present, &packet_id));
        if (present && packet_id == 100)
          ++acknowledged;
      }
    } else if (kind == RUMQTTC_EVENT_INCOMING_PUBLISH) {
      rumqttc_string_view_t topic = {NULL, 0};
      rumqttc_bytes_view_t payload = {NULL, 0};
      rumqttc_qos_t qos = 0;
      REQUIRE(!restored);
      CHECK(rumqttc_event_publish(event, &topic, &payload, &qos, NULL, NULL, NULL));
      REQUIRE(topic.len == strlen("rumqttc/native/restart/incoming"));
      REQUIRE(memcmp(topic.data, "rumqttc/native/restart/incoming", topic.len) == 0);
      REQUIRE(qos == RUMQTTC_QOS_2);
      REQUIRE(payload.len == 7 && memcmp(payload.data, "inbound", 7) == 0);
      REQUIRE(++incoming == 1);
    }
    rumqttc_event_destroy(event);
  }
}

static void wait_for_restored_operations(rumqttc_client_t *client) {
  for (unsigned attempt = 0; attempt < 500; ++attempt) {
    rumqttc_completion_t *completion = NULL;
    rumqttc_diagnostics_t diagnostics = RUMQTTC_DIAGNOSTICS_INIT;
    CHECK(rumqttc_client_diagnostics_tracked(client, &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_DIAGNOSTICS);
    CHECK(rumqttc_completion_diagnostics(completion, &diagnostics, NULL));
    rumqttc_completion_destroy(completion);
    if (diagnostics.outbound_drained && diagnostics.pending_subscribes == 0 &&
        diagnostics.inflight_publishes == 0)
      return;
    native_sleep_ms(10);
  }
  REQUIRE(0);
}

static void exercise_restart(rumqttc_protocol_t protocol, const char *client_id,
                             int session_lost) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
  store_context *context = calloc(1, sizeof(*context));
  rumqttc_publish_options_t publish = RUMQTTC_PUBLISH_OPTIONS_INIT;
  rumqttc_completion_t *qos1_completion = NULL;
  rumqttc_completion_t *qos2_completion = NULL;
  rumqttc_completion_t *subscribe_completion = NULL;
  rumqttc_subscription_t subscription =
      native_subscription("rumqttc/native/restart/incoming", RUMQTTC_QOS_2);
  rumqttc_subscribe_options_t subscribe = RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
  REQUIRE(context != NULL);
  vtable.load = load_checkpoint;
  vtable.save = save_checkpoint;
  vtable.clear = clear_checkpoint;
  vtable.destroy = destroy_store;
  CHECK(rumqttc_store_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  CHECK(rumqttc_config_set_emit_outgoing_events(config, 1, NULL));
  if (protocol == RUMQTTC_PROTOCOL_V4) {
    CHECK(rumqttc_config_set_v4_clean_session(config, 0, NULL));
  } else {
    CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  }
  CHECK(rumqttc_config_set_session_store(config, registration,
                                         native_string("restart-scope"), 5000,
                                         16u * 1024u * 1024u, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  {
    rumqttc_event_t *connected = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    rumqttc_event_destroy(connected);
  }
  CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, &subscribe,
                                         &subscribe_completion, NULL));
  publish.qos = RUMQTTC_QOS_1;
  CHECK(rumqttc_client_publish_tracked(
      client, native_string("rumqttc/native/restart/qos1"),
      native_bytes((const uint8_t *)"persist", 7), &publish, &qos1_completion, NULL));
  publish.qos = RUMQTTC_QOS_2;
  CHECK(rumqttc_client_publish_tracked(
      client, native_string("rumqttc/native/restart/qos2"),
      native_bytes((const uint8_t *)"persist", 7), &publish, &qos2_completion, NULL));
  wait_restart_activity(client, 0);
  rumqttc_completion_destroy(qos1_completion);
  rumqttc_completion_destroy(qos2_completion);
  rumqttc_completion_destroy(subscribe_completion);
  native_close_destroy(client);
  REQUIRE(context->checkpoint != NULL && context->length > 0 && context->saves > 0);
  unsigned previous_clears = context->clears;
  CHECK(rumqttc_client_start(config, &client, NULL));
  if (session_lost) {
    rumqttc_event_t *connected = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    uint8_t present = 1;
    CHECK(rumqttc_event_connected(connected, NULL, &present));
    REQUIRE(present == 0);
    rumqttc_event_destroy(connected);
    wait_for_restored_operations(client);
    publish.qos = RUMQTTC_QOS_1;
    CHECK(rumqttc_client_publish_tracked(
        client, native_string("rumqttc/native/restart/fresh"),
        native_bytes((const uint8_t *)"fresh", 5), &publish, &qos1_completion, NULL));
    native_wait_completion(qos1_completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    rumqttc_completion_destroy(qos1_completion);
  } else {
    wait_restart_activity(client, 1);
  }
  wait_for_restored_operations(client);
  native_close_destroy(client);
  REQUIRE(context->loads >= 2);
  REQUIRE(context->clears == previous_clears + (unsigned)session_lost);
  rumqttc_config_destroy(config);
  rumqttc_store_registration_destroy(registration);
}

static void exercise_resume_policy(uint32_t policy, const char *client_id) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
  store_context *context = calloc(1, sizeof(*context));
  rumqttc_event_t *event = NULL;
  REQUIRE(context != NULL);
  vtable.load = load_checkpoint;
  vtable.save = save_checkpoint;
  vtable.clear = clear_checkpoint;
  vtable.destroy = destroy_store;
  CHECK(rumqttc_store_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_config_set_v5_broker_session_resume_policy(config, policy, NULL));
  CHECK(rumqttc_config_set_session_store(config, registration,
                                         native_string("resume-policy"), 5000,
                                         16u * 1024u * 1024u, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS,
                                             &event, NULL));
  rumqttc_event_kind_t kind = 0;
  CHECK(rumqttc_event_kind(event, &kind));
  if (policy == 0) {
    rumqttc_error_t *error = NULL;
    rumqttc_error_kind_t error_kind = 0;
    REQUIRE(kind == RUMQTTC_EVENT_DISCONNECTED);
    CHECK(rumqttc_event_disconnected(event, NULL, &error));
    CHECK(rumqttc_error_kind(error, &error_kind));
    REQUIRE(error_kind == RUMQTTC_ERROR_PROTOCOL);
    rumqttc_error_destroy(error);
  } else {
    uint8_t session_present = 0;
    rumqttc_completion_t *completion = NULL;
    uint8_t present = 0, raw = 0, resumed = 0;
    uint32_t diagnostic = 0;
    REQUIRE(kind == RUMQTTC_EVENT_CONNECTED);
    CHECK(rumqttc_event_connected(event, NULL, &session_present));
    REQUIRE(session_present == 1);
    CHECK(rumqttc_client_diagnostics_tracked(client, &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_DIAGNOSTICS);
    CHECK(rumqttc_completion_connack_session_diagnostics(
        completion, &present, &raw, &resumed, &diagnostic, NULL));
    REQUIRE(present == 1 && raw == 1 && resumed == 1);
    REQUIRE(diagnostic == RUMQTTC_CONNACK_DIAGNOSTIC_BROKER_ONLY_SESSION_RESUME);
    rumqttc_completion_destroy(completion);
  }
  rumqttc_event_destroy(event);
  native_close_destroy(client);
  REQUIRE(context->loads > 0 && context->clears == 0);
  rumqttc_config_destroy(config);
  rumqttc_store_registration_destroy(registration);
}

int main(void) {
  exercise_restart(RUMQTTC_PROTOCOL_V4, "native-store-restart-v4", 0);
  exercise_restart(RUMQTTC_PROTOCOL_V5, "native-store-restart-v5", 0);
  exercise_restart(RUMQTTC_PROTOCOL_V4, "native-store-restart-lost-v4", 1);
  exercise_restart(RUMQTTC_PROTOCOL_V5, "native-store-restart-lost-v5", 1);
  exercise_resume_policy(0, "native-store-policy-strict");
  exercise_resume_policy(1, "native-store-policy-allow");
  REQUIRE(atomic_load(&destroyed) == 6);
  return 0;
}
