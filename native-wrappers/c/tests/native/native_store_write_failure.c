#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>

enum write_mode { FAIL_SAVE, TIMEOUT_SAVE, FAIL_CLEAR, TIMEOUT_CLEAR };

typedef struct store_context {
  enum write_mode mode;
  atomic_uint pending_count;
  atomic_uintptr_t pending[16];
  unsigned saves;
  unsigned clears;
} store_context;

static atomic_uint destroyed;

static void load_checkpoint(void *user_data, const rumqttc_store_request_t *request,
                            rumqttc_callback_completion_t *completion) {
  (void)user_data;
  REQUIRE(request->operation == RUMQTTC_STORE_LOAD);
  CHECK(rumqttc_callback_store_load_complete(completion, RUMQTTC_STORE_NOT_FOUND, native_bytes(NULL, 0)));
}

static void finish_write(store_context *context, uint32_t operation, rumqttc_callback_completion_t *completion) {
  enum write_mode mode = context->mode;
  REQUIRE(operation == RUMQTTC_STORE_SAVE || operation == RUMQTTC_STORE_CLEAR);
  if (operation == RUMQTTC_STORE_SAVE)
    ++context->saves;
  else
    ++context->clears;
  if ((operation == RUMQTTC_STORE_SAVE && mode == TIMEOUT_SAVE) ||
      (operation == RUMQTTC_STORE_CLEAR && mode == TIMEOUT_CLEAR)) {
    rumqttc_callback_completion_t *retained = NULL;
    CHECK(rumqttc_callback_completion_retain(completion, &retained));
    unsigned index = atomic_fetch_add(&context->pending_count, 1);
    REQUIRE(index < 16);
    atomic_store(&context->pending[index], (uintptr_t)retained);
  } else if ((operation == RUMQTTC_STORE_SAVE && mode == FAIL_SAVE) ||
             (operation == RUMQTTC_STORE_CLEAR && mode == FAIL_CLEAR)) {
    CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FAILED));
  } else {
    CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
  }
}

static void save_checkpoint(void *user_data, const rumqttc_store_request_t *request,
                            rumqttc_callback_completion_t *completion) {
  REQUIRE(request->checkpoint.len > 0);
  finish_write((store_context *)user_data, request->operation, completion);
}

static void clear_checkpoint(void *user_data, const rumqttc_store_request_t *request,
                             rumqttc_callback_completion_t *completion) {
  finish_write((store_context *)user_data, request->operation, completion);
}

static void destroy_store(void *user_data) {
  free(user_data);
  atomic_fetch_add(&destroyed, 1);
}

static void check_store_error(rumqttc_error_t *error, uint32_t expected_failure) {
  uint8_t present = 0;
  uint32_t failure = 0;
  REQUIRE(error != NULL);
  CHECK(rumqttc_error_store_failure(error, &present, &failure));
  REQUIRE(present == 1 && failure == expected_failure);
}

static void run_case(rumqttc_protocol_t protocol, enum write_mode mode, const char *client_id) {
  rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
  store_context *context = calloc(1, sizeof(*context));
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_publish_options_t publish = RUMQTTC_PUBLISH_OPTIONS_INIT;
  rumqttc_error_t *error = NULL;
  rumqttc_event_t *event;
  uint32_t expected_failure = mode == FAIL_SAVE    ? RUMQTTC_STORE_FAILURE_SAVE
                              : mode == FAIL_CLEAR ? RUMQTTC_STORE_FAILURE_CLEAR
                                                   : RUMQTTC_STORE_FAILURE_TIMEOUT;
  unsigned previous_destroyed = atomic_load(&destroyed);
  REQUIRE(context != NULL);
  context->mode = mode;
  vtable.load = load_checkpoint;
  vtable.save = save_checkpoint;
  vtable.clear = clear_checkpoint;
  vtable.destroy = destroy_store;
  CHECK(rumqttc_store_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  if (protocol == RUMQTTC_PROTOCOL_V4)
    CHECK(rumqttc_config_set_v4_clean_session(config, 0, NULL));
  else
    CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_config_set_session_store(config, registration, native_string("write-failure-scope"),
                                         mode == TIMEOUT_SAVE || mode == TIMEOUT_CLEAR ? 100 : NATIVE_DEADLINE_MS,
                                         1024 * 1024, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  if (mode == FAIL_SAVE || mode == TIMEOUT_SAVE) {
    event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    rumqttc_event_destroy(event);
    publish.qos = RUMQTTC_QOS_1;
    CHECK(rumqttc_client_publish_tracked(client, native_string("store/write-failure"),
                                         native_bytes((const uint8_t *)"x", 1), &publish, &completion, NULL));
  }
  event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
  CHECK(rumqttc_event_disconnected(event, NULL, &error));
  check_store_error(error, expected_failure);
  rumqttc_error_destroy(error);
  rumqttc_event_destroy(event);
  if (completion != NULL) {
    error = NULL;
    REQUIRE(rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, &error) != RUMQTTC_OK);
    REQUIRE(error != NULL);
    rumqttc_error_destroy(error);
    rumqttc_completion_destroy(completion);
  }
  REQUIRE(context->clears >= 1);
  if (mode == FAIL_SAVE || mode == TIMEOUT_SAVE)
    REQUIRE(context->saves >= 1);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
  rumqttc_store_registration_destroy(registration);
  if (mode == TIMEOUT_SAVE || mode == TIMEOUT_CLEAR) {
    unsigned count = atomic_load(&context->pending_count);
    REQUIRE(count > 0);
    for (unsigned index = 0; index < count; ++index) {
      rumqttc_callback_completion_t *pending = (rumqttc_callback_completion_t *)atomic_load(&context->pending[index]);
      REQUIRE(pending != NULL);
      REQUIRE(atomic_load(&destroyed) == previous_destroyed);
      REQUIRE(rumqttc_callback_store_write_complete(pending, RUMQTTC_STORE_FOUND) == RUMQTTC_INVALID_STATE);
      rumqttc_callback_completion_destroy(pending);
    }
  }
  REQUIRE(atomic_load(&destroyed) == previous_destroyed + 1);
}

int main(void) {
  for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
    run_case(protocol, FAIL_SAVE, protocol == RUMQTTC_PROTOCOL_V4 ? "write-fail-save-v4" : "write-fail-save-v5");
    run_case(protocol, TIMEOUT_SAVE,
             protocol == RUMQTTC_PROTOCOL_V4 ? "write-timeout-save-v4" : "write-timeout-save-v5");
    run_case(protocol, FAIL_CLEAR, protocol == RUMQTTC_PROTOCOL_V4 ? "write-fail-clear-v4" : "write-fail-clear-v5");
    run_case(protocol, TIMEOUT_CLEAR,
             protocol == RUMQTTC_PROTOCOL_V4 ? "write-timeout-clear-v4" : "write-timeout-clear-v5");
  }
  return 0;
}
