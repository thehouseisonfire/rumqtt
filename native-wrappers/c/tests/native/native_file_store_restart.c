/* Reuse the wire assertions, while replacing the application store with the
 * maintained file store. The broker runner starts each stage in a new process. */
#define main callback_store_fixture_main
#include "native_store_restart.c"
#undef main
#if defined(_WIN32)
#include <wchar.h>
#endif

static void file_wait(rumqttc_file_store_operation_t *operation) {
  uint32_t state = 0;
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_operation_wait_timeout_ms */
  CHECK(rumqttc_file_store_operation_wait_timeout_ms(operation, NATIVE_DEADLINE_MS, &state, NULL));
  REQUIRE(state == RUMQTTC_FILE_STORE_STATE_SUCCESS);
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_operation_poll */
  CHECK(rumqttc_file_store_operation_poll(operation, &state, NULL));
  rumqttc_file_store_operation_t *retained = NULL;
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_operation_retain */
  CHECK(rumqttc_file_store_operation_retain(operation, &retained, NULL));
  rumqttc_file_store_operation_release(retained);
}
static void invalid_outputs(rumqttc_protocol_t protocol, rumqttc_path_view_t root) {
  rumqttc_file_store_context_t *context = NULL;
  rumqttc_file_store_t *store = NULL;
  rumqttc_file_store_operation_t *operation = NULL;
  rumqttc_file_store_result_t result = RUMQTTC_FILE_STORE_RESULT_INIT;
  rumqttc_file_store_error_details_t detail = RUMQTTC_FILE_STORE_ERROR_DETAILS_INIT;
  rumqttc_string_view_t text = native_string("key");
  uint32_t state = 0, encoding = 0;
  size_t length = 0;
  /* ERROR_OUT_FAILURE: rumqttc_file_store_context_new */
  REQUIRE(rumqttc_file_store_context_new(NULL, NULL, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_context_retain */
  REQUIRE(rumqttc_file_store_context_retain(NULL, &context, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_retain */
  REQUIRE(rumqttc_file_store_retain(NULL, &store, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_operation_retain */
  REQUIRE(rumqttc_file_store_operation_retain(NULL, &operation, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_begin_open */
  REQUIRE(rumqttc_file_store_begin_open(NULL, protocol, root, NULL, &operation, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_config_set_file_session_store */
  REQUIRE(rumqttc_config_set_file_session_store(NULL, NULL, text, 1000, 1024, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_operation_poll */
  REQUIRE(rumqttc_file_store_operation_poll(NULL, &state, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_operation_wait_timeout_ms */
  REQUIRE(rumqttc_file_store_operation_wait_timeout_ms(NULL, 0, &state, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_operation_get_store */
  REQUIRE(rumqttc_file_store_operation_get_store(NULL, &store, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_inspect */
  REQUIRE(rumqttc_file_store_inspect(NULL, text, text, &operation, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_quarantine */
  REQUIRE(rumqttc_file_store_quarantine(NULL, text, text, &operation, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_clear */
  REQUIRE(rumqttc_file_store_clear(NULL, text, text, &operation, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_cleanup */
  REQUIRE(rumqttc_file_store_cleanup(NULL, 1, &operation, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_close */
  REQUIRE(rumqttc_file_store_close(NULL, &operation, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_context_request_shutdown */
  REQUIRE(rumqttc_file_store_context_request_shutdown(NULL, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_context_join_timeout_ms */
  REQUIRE(rumqttc_file_store_context_join_timeout_ms(NULL, 0, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_operation_result */
  REQUIRE(rumqttc_file_store_operation_result(NULL, &result, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_operation_path_copy */
  REQUIRE(rumqttc_file_store_operation_path_copy(NULL, NULL, 0, &length, &encoding, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_operation_entry */
  REQUIRE(rumqttc_file_store_operation_entry(NULL, 0, 0, &text, NULL) != RUMQTTC_OK);
  /* ERROR_OUT_FAILURE: rumqttc_file_store_operation_cleanup_failure */
  REQUIRE(rumqttc_file_store_operation_cleanup_failure(NULL, 0, &detail, NULL) != RUMQTTC_OK);
}
int main(int argc, char **argv) {
  uint64_t capabilities = rumqttc_library_capabilities();
  if (!(capabilities & RUMQTTC_CAP_FILE_SESSION_STORE))
    return 77;
  REQUIRE(argc >= 2);
  int seed = strncmp(argv[1], "seed", 4) == 0;
  rumqttc_protocol_t protocol = strstr(argv[1], "v5") ? RUMQTTC_PROTOCOL_V5 : RUMQTTC_PROTOCOL_V4;
  const char *id = protocol == RUMQTTC_PROTOCOL_V4 ? "native-store-restart-v4" : "native-store-restart-v5";
  rumqttc_path_view_t root;
#if defined(_WIN32)
  const wchar_t *root_text = _wgetenv(L"RUMQTTC_TEST_FILE_STORE_ROOT");
  REQUIRE(root_text != NULL);
  root = (rumqttc_path_view_t){root_text, wcslen(root_text), RUMQTTC_PATH_WINDOWS_UTF16, 0};
#else
  const char *root_text = getenv("RUMQTTC_TEST_FILE_STORE_ROOT");
  REQUIRE(root_text != NULL);
  root = (rumqttc_path_view_t){root_text, strlen(root_text), RUMQTTC_PATH_UNIX_BYTES, 0};
#endif
  invalid_outputs(protocol, root);
  rumqttc_file_store_context_t *context = NULL;
  rumqttc_file_store_t *store = NULL;
  rumqttc_file_store_operation_t *operation = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_context_new */
  CHECK(rumqttc_file_store_context_new(NULL, &context, NULL));
  rumqttc_file_store_context_t *retained_context = NULL;
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_context_retain */
  CHECK(rumqttc_file_store_context_retain(context, &retained_context, NULL));
  rumqttc_file_store_context_release(context);
  context = retained_context;
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_begin_open */
  CHECK(rumqttc_file_store_begin_open(context, protocol, root, NULL, &operation, NULL));
  file_wait(operation);
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_operation_get_store */
  CHECK(rumqttc_file_store_operation_get_store(operation, &store, NULL));
  rumqttc_file_store_operation_release(operation);
  rumqttc_file_store_t *retained_store = NULL;
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_retain */
  CHECK(rumqttc_file_store_retain(store, &retained_store, NULL));
  rumqttc_file_store_release(store);
  store = retained_store;
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_cleanup */
  CHECK(rumqttc_file_store_cleanup(store, 1, &operation, NULL));
#if defined(_WIN32)
  file_wait(operation);
  rumqttc_file_store_result_t cleanup = RUMQTTC_FILE_STORE_RESULT_INIT;
  CHECK(rumqttc_file_store_operation_result(operation, &cleanup, NULL));
  REQUIRE(cleanup.kind == RUMQTTC_FILE_STORE_RESULT_CLEANUP);
  for (size_t i = 0; i < cleanup.failure_count; ++i) {
    rumqttc_file_store_error_details_t detail = RUMQTTC_FILE_STORE_ERROR_DETAILS_INIT;
    /* ERROR_OUT_SUCCESS: rumqttc_file_store_operation_cleanup_failure */
    CHECK(rumqttc_file_store_operation_cleanup_failure(operation, i, &detail, NULL));
    REQUIRE(detail.kind == RUMQTTC_FILE_STORE_FAILURE_IO);
  }
#else
  uint32_t cleanup_state = 0;
  REQUIRE(rumqttc_file_store_operation_wait_timeout_ms(operation, NATIVE_DEADLINE_MS, &cleanup_state, NULL) ==
          RUMQTTC_CONFIG_ERROR);
  REQUIRE(cleanup_state == RUMQTTC_FILE_STORE_STATE_FAILURE);
#endif
  rumqttc_file_store_operation_release(operation);
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
  CHECK(rumqttc_config_set_emit_outgoing_events(config, 1, NULL));
  if (protocol == RUMQTTC_PROTOCOL_V4)
    CHECK(rumqttc_config_set_v4_clean_session(config, 0, NULL));
  else
    CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_file_session_store */
  CHECK(rumqttc_config_set_file_session_store(config, store, native_string("file-restart"), NATIVE_DEADLINE_MS,
                                              16u * 1024u * 1024u, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  {
    rumqttc_file_store_operation_t *rejected = NULL;
    REQUIRE(rumqttc_file_store_clear(store, native_string("file-restart"), native_string(id), &rejected, NULL) ==
            RUMQTTC_INVALID_STATE);
    REQUIRE(rejected == NULL);
  }
  if (seed) {
    rumqttc_event_destroy(native_wait_event(client, RUMQTTC_EVENT_CONNECTED));
    rumqttc_subscription_t sub = native_subscription("rumqttc/native/restart/incoming", RUMQTTC_QOS_2);
    rumqttc_subscribe_options_t subscribe = RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
    rumqttc_publish_options_t publish = RUMQTTC_PUBLISH_OPTIONS_INIT;
    rumqttc_completion_t *a = NULL, *b = NULL, *c = NULL;
    CHECK(rumqttc_client_subscribe_tracked(client, &sub, 1, &subscribe, &a, NULL));
    publish.qos = RUMQTTC_QOS_1;
    CHECK(rumqttc_client_publish_tracked(client, native_string("rumqttc/native/restart/qos1"),
                                         native_bytes((const uint8_t *)"persist", 7), &publish, &b, NULL));
    publish.qos = RUMQTTC_QOS_2;
    CHECK(rumqttc_client_publish_tracked(client, native_string("rumqttc/native/restart/qos2"),
                                         native_bytes((const uint8_t *)"persist", 7), &publish, &c, NULL));
    wait_restart_activity(client, 0);
    /* Abrupt process loss retains the last complete protocol checkpoint. */
    _Exit(0);
  } else {
    wait_restart_activity(client, 1);
    wait_for_restored_operations(client);
  }
  native_close_destroy(client);
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_inspect */
  CHECK(rumqttc_file_store_inspect(store, native_string("file-restart"), native_string(id), &operation, NULL));
  file_wait(operation);
  rumqttc_file_store_result_t info = RUMQTTC_FILE_STORE_RESULT_INIT;
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_operation_result */
  CHECK(rumqttc_file_store_operation_result(operation, &info, NULL));
  REQUIRE(info.kind == 1 && info.present && info.size_present && info.size > 0);
  rumqttc_file_store_operation_release(operation);
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_quarantine */
  CHECK(rumqttc_file_store_quarantine(store, native_string("file-restart"), native_string(id), &operation, NULL));
  file_wait(operation);
  info = (rumqttc_file_store_result_t)RUMQTTC_FILE_STORE_RESULT_INIT;
  CHECK(rumqttc_file_store_operation_result(operation, &info, NULL));
  REQUIRE(info.kind == 2);
  rumqttc_string_view_t identifier = {NULL, 0};
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_operation_entry */
  CHECK(rumqttc_file_store_operation_entry(operation, 0, 0, &identifier, NULL));
  REQUIRE(identifier.len > 0);
  size_t path_length = 0;
  uint32_t encoding = 0;
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_operation_path_copy */
  CHECK(rumqttc_file_store_operation_path_copy(operation, NULL, 0, &path_length, &encoding, NULL));
  REQUIRE(path_length > 0 && encoding == root.encoding);
  uint8_t *path_copy = malloc(path_length);
  REQUIRE(path_copy != NULL);
  CHECK(rumqttc_file_store_operation_path_copy(operation, path_copy, path_length, &path_length, &encoding, NULL));
  free(path_copy);
  rumqttc_file_store_operation_release(operation);
  CHECK(rumqttc_file_store_inspect(store, native_string("file-restart"), native_string(id), &operation, NULL));
  file_wait(operation);
  info = (rumqttc_file_store_result_t)RUMQTTC_FILE_STORE_RESULT_INIT;
  CHECK(rumqttc_file_store_operation_result(operation, &info, NULL));
  REQUIRE(!info.present);
  rumqttc_file_store_operation_release(operation);
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_clear */
  CHECK(rumqttc_file_store_clear(store, native_string("file-restart"), native_string(id), &operation, NULL));
  file_wait(operation);
  rumqttc_file_store_operation_release(operation);
  rumqttc_config_destroy(config);
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_close */
  CHECK(rumqttc_file_store_close(store, &operation, NULL));
  file_wait(operation);
  rumqttc_file_store_operation_release(operation);
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_context_request_shutdown */
  CHECK(rumqttc_file_store_context_request_shutdown(context, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_file_store_context_join_timeout_ms */
  CHECK(rumqttc_file_store_context_join_timeout_ms(context, NATIVE_DEADLINE_MS, NULL));
  rumqttc_file_store_release(store);
  rumqttc_file_store_context_release(context);
  return 0;
}
