#include "example_common.h"
#include <stdlib.h>
#include <string.h>
#if defined(_WIN32)
#include <wchar.h>
#endif

static int wait_store(rumqttc_file_store_operation_t *operation) {
  uint32_t state = 0;
  rumqttc_error_t *error = NULL;
  return example_report(rumqttc_file_store_operation_wait_timeout_ms(operation, 5000, &state, &error), &error,
                        "observe store operation");
}
int main(int argc, char **argv) {
  if (argc != 3)
    return 2;
  if (!(rumqttc_library_capabilities() & RUMQTTC_CAP_FILE_SESSION_STORE))
    return 77;
  rumqttc_path_view_t root;
#if defined(_WIN32)
  const wchar_t *text = _wgetenv(L"RUMQTTC_SESSION_ROOT");
  if (!text)
    return 2;
  root = (rumqttc_path_view_t){text, wcslen(text), RUMQTTC_PATH_WINDOWS_UTF16, 0};
#else
  const char *text = getenv("RUMQTTC_SESSION_ROOT");
  if (!text)
    return 2;
  root = (rumqttc_path_view_t){text, strlen(text), RUMQTTC_PATH_UNIX_BYTES, 0};
#endif
  rumqttc_file_store_context_t *context = NULL;
  rumqttc_file_store_t *store = NULL;
  rumqttc_file_store_operation_t *operation = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_error_t *error = NULL;
  unsigned long port = strtoul(argv[2], NULL, 10);
  int failed = port == 0 || port > 65535;
  if (!failed)
    failed = example_report(rumqttc_file_store_context_new(NULL, &context, &error), &error, "create store context");
  if (!failed)
    failed = example_report(rumqttc_file_store_begin_open(context, RUMQTTC_PROTOCOL_V4, root, NULL, &operation, &error),
                            &error, "begin store open");
  if (!failed)
    failed = wait_store(operation);
  if (!failed)
    failed = example_report(rumqttc_file_store_operation_get_store(operation, &store, &error), &error, "retain store");
  rumqttc_file_store_operation_release(operation);
  operation = NULL;
  if (!failed)
    failed = example_report(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &config, &error), &error, "create config");
  if (!failed)
    failed = example_report(rumqttc_config_set_broker(config, example_string(argv[1]), (uint16_t)port, &error), &error,
                            "set broker");
  if (!failed)
    failed = example_report(rumqttc_config_set_client_id(config, example_string("file-store-example"), &error), &error,
                            "set client ID");
  if (!failed)
    failed = example_report(rumqttc_config_set_v4_clean_session(config, 0, &error), &error, "persistent session");
  if (!failed)
    failed = example_report(rumqttc_config_set_file_session_store(config, store, example_string("example"), 5000,
                                                                  16u * 1024u * 1024u, &error),
                            &error, "attach store");
  if (!failed)
    failed = example_report(rumqttc_client_start(config, &client, &error), &error, "start client");
  if (client) {
    failed |= example_report(rumqttc_client_close_now_timeout_ms(client, 5000, &error), &error, "close client");
    if (example_report(rumqttc_client_destroy_timeout_ms(client, 5000, &error), &error, "destroy client")) {
      rumqttc_client_abandon(client);
      failed = 1;
    }
  }
  rumqttc_config_destroy(config);
  if (store) {
    int closing = example_report(rumqttc_file_store_close(store, &operation, &error), &error, "close store");
    failed |= closing;
    if (!closing)
      failed |= wait_store(operation);
    rumqttc_file_store_operation_release(operation);
  }
  if (context) {
    failed |=
        example_report(rumqttc_file_store_context_request_shutdown(context, &error), &error, "shutdown store context");
    failed |=
        example_report(rumqttc_file_store_context_join_timeout_ms(context, 5000, &error), &error, "join store context");
  }
  rumqttc_file_store_release(store);
  rumqttc_file_store_context_release(context);
  rumqttc_error_destroy(error);
  return failed ? 1 : 0;
}
