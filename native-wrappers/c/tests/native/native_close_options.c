#include "native_common.h"

#include <stdatomic.h>
#include <string.h>

typedef struct close_caller {
  rumqttc_client_t *client;
  const rumqttc_disconnect_options_t *options;
  atomic_uint entered;
  rumqttc_status_t status;
} close_caller;

static int wait_close(void *argument) {
  close_caller *caller = argument;
  atomic_store(&caller->entered, 1);
  caller->status =
      rumqttc_client_close_with_options_timeout_ms(caller->client, NATIVE_DEADLINE_MS, caller->options, NULL);
  return 0;
}

static void run_case(int escalate) {
  const char *client_id = escalate ? "native-close-options-immediate" : "native-close-options-graceful";
  rumqttc_client_t *client = native_start_client(RUMQTTC_PROTOCOL_V5, client_id, RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
  rumqttc_publish_options_t publish = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *pending = NULL;
  rumqttc_v5_disconnect_properties_t properties = RUMQTTC_V5_DISCONNECT_PROPERTIES_INIT;
  rumqttc_disconnect_options_t options = RUMQTTC_DISCONNECT_OPTIONS_INIT;
  rumqttc_user_property_t user_properties[2] = {RUMQTTC_USER_PROPERTY_INIT, RUMQTTC_USER_PROPERTY_INIT};
  char reason[] = "selected", key[] = "k", first[] = "1", second[] = "2";
  properties.session_expiry_present = 1;
  properties.session_expiry_interval = 0;
  properties.reason_string_present = 1;
  properties.reason_string = native_string(reason);
  user_properties[0].name = native_string(key);
  user_properties[0].value = native_string(first);
  user_properties[1].name = native_string(key);
  user_properties[1].value = native_string(second);
  properties.user_properties = user_properties;
  properties.user_property_count = 2;
  options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  options.v5_properties = &properties;
  CHECK(rumqttc_client_publish_tracked(client, native_string("native/close/stall"), native_bytes(NULL, 0), &publish,
                                       &pending, NULL));
  rumqttc_event_t *ready = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
  rumqttc_event_destroy(ready);
  close_caller caller = {.client = client, .options = &options};
  native_thread_t *worker = native_thread_start(wait_close, &caller);
  rumqttc_publish_options_t probe = native_publish_options(RUMQTTC_QOS_0);
  unsigned closing = 0;
  for (unsigned attempt = 0; attempt < 500; ++attempt) {
    uint64_t operation_id = 0;
    rumqttc_status_t status = rumqttc_client_try_publish(client, native_string("native/close/probe"),
                                                         native_bytes(NULL, 0), &probe, &operation_id, NULL);
    if (status == RUMQTTC_INVALID_STATE) {
      closing = 1;
      break;
    }
    REQUIRE(status == RUMQTTC_OK || status == RUMQTTC_BACKPRESSURE);
    native_sleep_ms(1);
  }
  REQUIRE(closing);
  REQUIRE(rumqttc_client_close_with_options_timeout_ms(client, 20, &options, NULL) == RUMQTTC_TIMEOUT);
  memset(reason, 'x', strlen(reason));
  key[0] = first[0] = second[0] = 'x';
  properties.reason_string = native_string("selected");
  user_properties[0].name = user_properties[1].name = native_string("k");
  user_properties[0].value = native_string("1");
  user_properties[1].value = native_string("2");
  REQUIRE(atomic_load(&caller.entered));
  REQUIRE(rumqttc_client_close_with_options_timeout_ms(client, 10, &options, NULL) == RUMQTTC_TIMEOUT);
  rumqttc_v5_disconnect_properties_t conflicting_properties = properties;
  conflicting_properties.reason_string = native_string("conflict");
  rumqttc_disconnect_options_t conflicting = options;
  conflicting.v5_properties = &conflicting_properties;
  rumqttc_error_t *error = NULL;
  REQUIRE(rumqttc_client_close_now_with_options_timeout_ms(client, NATIVE_DEADLINE_MS, &conflicting, &error) !=
          RUMQTTC_OK);
  uint32_t delivery = 0;
  CHECK(rumqttc_error_context(error, NULL, NULL, NULL, NULL, &delivery));
  REQUIRE(delivery == RUMQTTC_DELIVERY_NOT_ADMITTED);
  rumqttc_error_destroy(error);
  if (escalate) {
    CHECK(rumqttc_client_close_now_with_options_timeout_ms(client, NATIVE_DEADLINE_MS, &options, NULL));
  } else {
    rumqttc_client_t *control =
        native_start_client(RUMQTTC_PROTOCOL_V5, "native-close-control", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
    rumqttc_completion_t *released = NULL;
    publish.qos = RUMQTTC_QOS_0;
    CHECK(rumqttc_client_publish_tracked(control, native_string("native/close/release"),
                                         native_bytes((const uint8_t *)client_id, strlen(client_id)), &publish,
                                         &released, NULL));
    native_wait_completion(released, RUMQTTC_COMPLETION_QOS0_FLUSHED);
    rumqttc_completion_destroy(released);
    native_close_destroy(control);
  }
  REQUIRE(native_thread_join(worker) == 0);
  if (escalate) {
    if (caller.status != RUMQTTC_AMBIGUOUS)
      native_fail(__FILE__, __LINE__, "escalated graceful caller", caller.status);
    REQUIRE(rumqttc_completion_wait_timeout_ms(pending, NATIVE_DEADLINE_MS, &error) == RUMQTTC_AMBIGUOUS);
    CHECK(rumqttc_error_context(error, NULL, NULL, NULL, NULL, &delivery));
    REQUIRE(delivery == RUMQTTC_DELIVERY_AMBIGUOUS);
    rumqttc_error_destroy(error);
  } else {
    CHECK(caller.status);
    native_wait_completion(pending, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  }
  rumqttc_completion_destroy(pending);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
}

int main(void) {
  run_case(0);
  run_case(1);
  return 0;
}
