#include "native_common.h"

void native_test_session_recovery(void);

static void load_empty(void *data, const rumqttc_store_request_t *request,
                       rumqttc_callback_completion_t *completion) {
  (void)data;
  REQUIRE(request->operation == RUMQTTC_STORE_LOAD);
  CHECK(rumqttc_callback_store_load_complete(completion, RUMQTTC_STORE_NOT_FOUND, native_bytes(NULL, 0)));
}

static void fail_clear(void *data, const rumqttc_store_request_t *request,
                       rumqttc_callback_completion_t *completion) {
  (void)data;
  REQUIRE(request->operation == RUMQTTC_STORE_CLEAR);
  CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FAILED));
}

static void save_success(void *data, const rumqttc_store_request_t *request,
                         rumqttc_callback_completion_t *completion) {
  (void)data;
  REQUIRE(request->operation == RUMQTTC_STORE_SAVE);
  CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
}

static void destroy_store(void *data) { (void)data; }

static void clear_failure(rumqttc_protocol_t protocol) {
  rumqttc_store_vtable_t table = RUMQTTC_STORE_VTABLE_INIT;
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_completion_t *rejected = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_reconnect_options_t retry = RUMQTTC_RECONNECT_OPTIONS_INIT;
  rumqttc_session_recovery_snapshot_t progress = RUMQTTC_SESSION_RECOVERY_SNAPSHOT_INIT;
  rumqttc_event_t *event;
  uint8_t present;
  uint32_t failure;
  table.load = load_empty;
  table.save = save_success;
  table.clear = fail_clear;
  table.destroy = destroy_store;
  retry.initial_delay_ms = retry.maximum_delay_ms = 1000;
  CHECK(rumqttc_store_registration_new(&table, NULL, &registration, NULL));
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(protocol == RUMQTTC_PROTOCOL_V4
        ? "native-session-recovery-clear-v4" : "native-session-recovery-clear-v5"), NULL));
  if (protocol == RUMQTTC_PROTOCOL_V4)
    CHECK(rumqttc_config_set_v4_clean_session(config, 0, NULL));
  else
    CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_config_set_session_store(config, registration, native_string("recovery"),
                                       NATIVE_DEADLINE_MS, 1024 * 1024, NULL));
  CHECK(rumqttc_config_set_reconnect_policy(config, &retry, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_config_destroy(config);
  rumqttc_store_registration_destroy(registration);
  event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
  rumqttc_event_destroy(event);
  CHECK(rumqttc_client_recover_session_tracked(client, &completion, NULL));
  REQUIRE(rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, &error) != RUMQTTC_OK);
  CHECK(rumqttc_error_session_recovery_failure(error, &present, &failure));
  REQUIRE(present && failure == RUMQTTC_RECOVERY_FAILURE_PERSISTENCE);
  CHECK(rumqttc_error_store_failure(error, &present, &failure));
  REQUIRE(present && failure == RUMQTTC_STORE_FAILURE_CLEAR);
  CHECK(rumqttc_completion_session_recovery_snapshot(completion, &progress, NULL));
  REQUIRE(progress.phase == RUMQTTC_RECOVERY_PHASE_FAILED);
  REQUIRE(progress.failure_phase == RUMQTTC_RECOVERY_PHASE_CLEARING_CHECKPOINT);
  REQUIRE(progress.abandonment_committed && !progress.checkpoint_cleared && !progress.fresh_established);
  REQUIRE(rumqttc_client_recover_session_tracked(client, &rejected, NULL) != RUMQTTC_OK);
  REQUIRE(rejected == NULL);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_completion_session_recovery_snapshot(completion, &progress, NULL));
  REQUIRE(progress.phase == RUMQTTC_RECOVERY_PHASE_FAILED && progress.abandonment_committed);
  rumqttc_error_destroy(error);
  rumqttc_completion_destroy(completion);
}

int main(void) {
  native_test_session_recovery();
  clear_failure(RUMQTTC_PROTOCOL_V4);
  clear_failure(RUMQTTC_PROTOCOL_V5);
  return 0;
}
