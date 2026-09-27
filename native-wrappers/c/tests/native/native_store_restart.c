#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

typedef struct store_context {
  uint8_t *checkpoint;
  size_t length;
  unsigned loads;
  unsigned saves;
} store_context;

static atomic_uint destroyed;

static void load_checkpoint(void *user_data,
                            const rumqttc_store_request_t *request,
                            rumqttc_callback_completion_t *completion) {
  store_context *context = (store_context *)user_data;
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
}

static void save_checkpoint(void *user_data,
                            const rumqttc_store_request_t *request,
                            rumqttc_callback_completion_t *completion) {
  store_context *context = (store_context *)user_data;
  uint8_t *copy = malloc(request->checkpoint.len ? request->checkpoint.len : 1);
  REQUIRE(copy != NULL);
  if (request->checkpoint.len)
    memcpy(copy, request->checkpoint.data, request->checkpoint.len);
  free(context->checkpoint);
  context->checkpoint = copy;
  context->length = request->checkpoint.len;
  ++context->saves;
  CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
}

static void clear_checkpoint(void *user_data,
                             const rumqttc_store_request_t *request,
                             rumqttc_callback_completion_t *completion) {
  store_context *context = (store_context *)user_data;
  (void)request;
  free(context->checkpoint);
  context->checkpoint = NULL;
  context->length = 0;
  CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
}

static void destroy_store(void *user_data) {
  store_context *context = (store_context *)user_data;
  free(context->checkpoint);
  free(context);
  atomic_fetch_add(&destroyed, 1);
}

static void wait_for_connection_and_publish(rumqttc_client_t *client) {
  unsigned connected = 0;
  unsigned published = 0;
  while (!connected || published < 2) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS,
                                               &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    if (kind == RUMQTTC_EVENT_CONNECTED) {
      connected = 1;
    } else if (kind == RUMQTTC_EVENT_OUTGOING) {
      uint32_t outgoing = 0;
      CHECK(rumqttc_event_outgoing_kind(event, &outgoing));
      if (outgoing == RUMQTTC_OUTGOING_PUBLISH)
        ++published;
    }
    rumqttc_event_destroy(event);
  }
}

static void exercise_restart(rumqttc_protocol_t protocol, const char *client_id) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
  store_context *context = calloc(1, sizeof(*context));
  rumqttc_publish_options_t publish = RUMQTTC_PUBLISH_OPTIONS_INIT;
  rumqttc_completion_t *qos1_completion = NULL;
  rumqttc_completion_t *qos2_completion = NULL;
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
  publish.qos = RUMQTTC_QOS_1;
  CHECK(rumqttc_client_publish_tracked(
      client, native_string("rumqttc/native/restart/qos1"),
      native_bytes((const uint8_t *)"persist", 7), &publish, &qos1_completion, NULL));
  publish.qos = RUMQTTC_QOS_2;
  CHECK(rumqttc_client_publish_tracked(
      client, native_string("rumqttc/native/restart/qos2"),
      native_bytes((const uint8_t *)"persist", 7), &publish, &qos2_completion, NULL));
  for (unsigned index = 0; index < 2; ++index) {
    rumqttc_event_t *outgoing = native_wait_event(client, RUMQTTC_EVENT_OUTGOING);
    uint32_t kind = 0;
    CHECK(rumqttc_event_outgoing_kind(outgoing, &kind));
    REQUIRE(kind == RUMQTTC_OUTGOING_PUBLISH);
    rumqttc_event_destroy(outgoing);
  }
  rumqttc_completion_destroy(qos1_completion);
  rumqttc_completion_destroy(qos2_completion);
  native_close_destroy(client);
  REQUIRE(context->checkpoint != NULL && context->length > 0 && context->saves > 0);
  CHECK(rumqttc_client_start(config, &client, NULL));
  wait_for_connection_and_publish(client);
  native_close_destroy(client);
  REQUIRE(context->loads >= 2);
  rumqttc_config_destroy(config);
  rumqttc_store_registration_destroy(registration);
}

int main(void) {
  exercise_restart(RUMQTTC_PROTOCOL_V4, "native-store-restart-v4");
  exercise_restart(RUMQTTC_PROTOCOL_V5, "native-store-restart-v5");
  REQUIRE(atomic_load(&destroyed) == 2);
  return 0;
}
