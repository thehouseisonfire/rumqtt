#include "native_common.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#include <windows.h>
#endif

static const char *checkpoint_path;
static const char *mode;
static unsigned saves;
static unsigned loads;

static uint64_t checksum(const uint8_t *bytes, size_t length) {
  uint64_t value = UINT64_C(14695981039346656037);
  for (size_t index = 0; index < length; ++index) {
    value ^= bytes[index];
    value *= UINT64_C(1099511628211);
  }
  return value;
}

static void sibling_path(char *out, size_t capacity, const char *suffix) {
  int written = snprintf(out, capacity, "%s%s", checkpoint_path, suffix);
  REQUIRE(written > 0 && (size_t)written < capacity);
}

static void load_checkpoint(void *user_data, const rumqttc_store_request_t *request,
                            rumqttc_callback_completion_t *completion) {
  FILE *file;
  uint8_t *bytes;
  long length;
  (void)user_data;
  REQUIRE(request->operation == RUMQTTC_STORE_LOAD);
  ++loads;
  file = fopen(checkpoint_path, "rb");
  if (file == NULL) {
    REQUIRE(strcmp(mode, "seed") == 0);
    CHECK(rumqttc_callback_store_load_complete(completion, RUMQTTC_STORE_NOT_FOUND, native_bytes(NULL, 0)));
    return;
  }
  REQUIRE(fseek(file, 0, SEEK_END) == 0);
  length = ftell(file);
  REQUIRE(length > 0 && fseek(file, 0, SEEK_SET) == 0);
  bytes = malloc((size_t)length);
  REQUIRE(bytes != NULL && fread(bytes, 1, (size_t)length, file) == (size_t)length);
  REQUIRE(fclose(file) == 0);
  if (strcmp(mode, "verify") == 0) {
    char expected_path[1024];
    unsigned long long expected_length = 0, expected_checksum = 0;
    sibling_path(expected_path, sizeof(expected_path), ".expected");
    file = fopen(expected_path, "r");
    REQUIRE(file != NULL);
    REQUIRE(fscanf(file, "%llu %llx", &expected_length, &expected_checksum) == 2);
    REQUIRE(fclose(file) == 0);
    REQUIRE((size_t)expected_length == (size_t)length);
    REQUIRE((uint64_t)expected_checksum == checksum(bytes, (size_t)length));
  }
  CHECK(rumqttc_callback_store_load_complete(completion, RUMQTTC_STORE_FOUND, native_bytes(bytes, (size_t)length)));
  free(bytes);
}

static void save_checkpoint(void *user_data, const rumqttc_store_request_t *request,
                            rumqttc_callback_completion_t *completion) {
  char staged_path[1024];
  FILE *file;
  size_t length;
  (void)user_data;
  REQUIRE(request->operation == RUMQTTC_STORE_SAVE);
  REQUIRE(request->checkpoint.len > 8);
  ++saves;
  if (strcmp(mode, "verify") == 0) {
    CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
    return;
  }
  sibling_path(staged_path, sizeof(staged_path), ".staged");
  file = fopen(staged_path, "wb");
  REQUIRE(file != NULL);
  length = strcmp(mode, "interrupt") == 0 ? request->checkpoint.len / 2 : request->checkpoint.len;
  REQUIRE(fwrite(request->checkpoint.data, 1, length, file) == length);
  REQUIRE(fflush(file) == 0 && fclose(file) == 0);
  if (strcmp(mode, "interrupt") == 0)
    _Exit(0);
#if defined(_WIN32)
  REQUIRE(MoveFileExA(staged_path, checkpoint_path, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) != 0);
#else
  REQUIRE(rename(staged_path, checkpoint_path) == 0);
#endif
  {
    char expected_path[1024];
    sibling_path(expected_path, sizeof(expected_path), ".expected");
    file = fopen(expected_path, "w");
    REQUIRE(file != NULL);
    REQUIRE(fprintf(file, "%llu %llx\n", (unsigned long long)request->checkpoint.len,
                    (unsigned long long)checksum(request->checkpoint.data, request->checkpoint.len)) > 0);
    REQUIRE(fclose(file) == 0);
  }
  CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
}

static void clear_checkpoint(void *user_data, const rumqttc_store_request_t *request,
                             rumqttc_callback_completion_t *completion) {
  (void)user_data;
  REQUIRE(request->operation == RUMQTTC_STORE_CLEAR);
  REQUIRE(strcmp(mode, "seed") == 0);
  REQUIRE(remove(checkpoint_path) == 0 || errno == ENOENT);
  CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
}

static void destroy_store(void *user_data) { free(user_data); }

int main(int argc, char **argv) {
  rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_publish_options_t publish = RUMQTTC_PUBLISH_OPTIONS_INIT;
  rumqttc_event_t *event;
  void *context = malloc(1);
  REQUIRE(argc >= 2 && context != NULL);
  mode = argv[1];
  REQUIRE(strcmp(mode, "seed") == 0 || strcmp(mode, "interrupt") == 0 || strcmp(mode, "verify") == 0);
  checkpoint_path = getenv("RUMQTTC_TEST_ATOMIC_FILE");
  REQUIRE(checkpoint_path != NULL);
  vtable.load = load_checkpoint;
  vtable.save = save_checkpoint;
  vtable.clear = clear_checkpoint;
  vtable.destroy = destroy_store;
  CHECK(rumqttc_store_registration_new(&vtable, context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-store-atomic"), NULL));
  CHECK(rumqttc_config_set_v4_clean_session(config, 0, NULL));
  CHECK(rumqttc_config_set_session_store(config, registration, native_string("atomic-scope"), NATIVE_DEADLINE_MS,
                                         1024 * 1024, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(event);
  if (strcmp(mode, "verify") != 0) {
    publish.qos = RUMQTTC_QOS_1;
    CHECK(rumqttc_client_publish_tracked(client, native_string("atomic/checkpoint"),
                                         native_bytes((const uint8_t *)mode, strlen(mode)), &publish, &completion,
                                         NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    rumqttc_completion_destroy(completion);
    REQUIRE(saves > 0);
  }
  native_close_destroy(client);
  REQUIRE(loads == 1);
  rumqttc_config_destroy(config);
  rumqttc_store_registration_destroy(registration);
  return 0;
}
