#if !defined(_WIN32)
#define _POSIX_C_SOURCE 200809L
#endif
#include "rumqttc.h"

#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#if !defined(_WIN32)
#include <time.h>
#endif

#if defined(_WIN32)
#include <windows.h>
typedef HMODULE module_t;
static module_t open_module(const char *path) { return LoadLibraryA(path); }
static void *find_symbol(module_t module, const char *name) {
  FARPROC function = GetProcAddress(module, name);
  void *address = NULL;
  memcpy(&address, &function, sizeof(address));
  return address;
}
static int close_module(module_t module) { return FreeLibrary(module) != 0; }
#else
#include <dlfcn.h>
typedef void *module_t;
static module_t open_module(const char *path) { return dlopen(path, RTLD_NOW | RTLD_LOCAL); }
static void *find_symbol(module_t module, const char *name) { return dlsym(module, name); }
static int close_module(module_t module) { return dlclose(module) == 0; }
#endif

#define REQUIRE(condition)                                                                                             \
  do {                                                                                                                 \
    if (!(condition)) {                                                                                                \
      fprintf(stderr, "%s:%d: requirement failed: %s\n", __FILE__, __LINE__, #condition);                              \
      abort();                                                                                                         \
    }                                                                                                                  \
  } while (0)

typedef struct native_api {
  rumqttc_status_t (*execution_context_new)(const rumqttc_execution_options_t *, rumqttc_execution_context_t **, rumqttc_error_t **);
  rumqttc_status_t (*execution_context_request_shutdown)(const rumqttc_execution_context_t *, rumqttc_error_t **);
  rumqttc_status_t (*execution_context_join_timeout_ms)(const rumqttc_execution_context_t *, uint64_t, rumqttc_error_t **);
  void (*execution_context_release)(rumqttc_execution_context_t *);
  rumqttc_status_t (*config_set_execution_context)(rumqttc_config_t *, const rumqttc_execution_context_t *, rumqttc_error_t **);
  rumqttc_status_t (*store_registration_new)(const rumqttc_store_vtable_t *, void *, rumqttc_store_registration_t **,
                                             rumqttc_error_t **);
  void (*store_registration_destroy)(rumqttc_store_registration_t *);
  rumqttc_status_t (*config_new)(rumqttc_protocol_t, rumqttc_config_t **, rumqttc_error_t **);
  void (*config_destroy)(rumqttc_config_t *);
  rumqttc_status_t (*config_set_broker)(rumqttc_config_t *, rumqttc_string_view_t, uint16_t, rumqttc_error_t **);
  rumqttc_status_t (*config_set_client_id)(rumqttc_config_t *, rumqttc_string_view_t, rumqttc_error_t **);
  rumqttc_status_t (*config_set_v4_clean_session)(rumqttc_config_t *, uint8_t, rumqttc_error_t **);
  rumqttc_status_t (*config_set_session_store)(rumqttc_config_t *, const rumqttc_store_registration_t *,
                                               rumqttc_string_view_t, uint64_t, size_t, rumqttc_error_t **);
  rumqttc_status_t (*client_start)(const rumqttc_config_t *, rumqttc_client_t **, rumqttc_error_t **);
  rumqttc_status_t (*client_close_now_timeout_ms)(rumqttc_client_t *, uint64_t, rumqttc_error_t **);
  rumqttc_status_t (*client_destroy_timeout_ms)(rumqttc_client_t *, uint64_t, rumqttc_error_t **);
  rumqttc_status_t (*callback_completion_retain)(const rumqttc_callback_completion_t *,
                                                 rumqttc_callback_completion_t **);
  void (*callback_completion_destroy)(rumqttc_callback_completion_t *);
  rumqttc_status_t (*callback_store_load_complete)(rumqttc_callback_completion_t *, uint32_t, rumqttc_bytes_view_t);
} native_api;

static native_api api;
static atomic_uintptr_t pending;
static atomic_uint destroyed;

static rumqttc_string_view_t view(const char *value) {
  rumqttc_string_view_t result = {value, strlen(value)};
  return result;
}

static void load(void *user_data, const rumqttc_store_request_t *request, rumqttc_callback_completion_t *completion) {
  rumqttc_callback_completion_t *retained = NULL;
  (void)user_data;
  REQUIRE(request->operation == RUMQTTC_STORE_LOAD);
  REQUIRE(api.callback_completion_retain(completion, &retained) == RUMQTTC_OK);
  REQUIRE(atomic_exchange(&pending, (uintptr_t)retained) == 0);
}

static void unexpected_write(void *user_data, const rumqttc_store_request_t *request,
                             rumqttc_callback_completion_t *completion) {
  (void)user_data;
  (void)request;
  (void)completion;
  REQUIRE(0);
}

static void destroy_store(void *user_data) {
  free(user_data);
  atomic_fetch_add(&destroyed, 1);
}

#define LOAD(name)                                                                                                     \
  do {                                                                                                                 \
    void *address = find_symbol(module, "rumqttc_" #name);                                                             \
    REQUIRE(address != NULL && sizeof(api.name) == sizeof(address));                                                   \
    memcpy(&api.name, &address, sizeof(address));                                                                      \
  } while (0)

int main(int argc, char **argv) {
  module_t module;
  rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_execution_context_t *execution = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_callback_completion_t *completion;
  void *context = malloc(1);
  REQUIRE(argc == 2 && context != NULL);
  module = open_module(argv[1]);
  REQUIRE(module != NULL);
  LOAD(execution_context_new);
  LOAD(execution_context_request_shutdown);
  LOAD(execution_context_join_timeout_ms);
  LOAD(execution_context_release);
  LOAD(config_set_execution_context);
  LOAD(store_registration_new);
  LOAD(store_registration_destroy);
  LOAD(config_new);
  LOAD(config_destroy);
  LOAD(config_set_broker);
  LOAD(config_set_client_id);
  LOAD(config_set_v4_clean_session);
  LOAD(config_set_session_store);
  LOAD(client_start);
  LOAD(client_close_now_timeout_ms);
  LOAD(client_destroy_timeout_ms);
  LOAD(callback_completion_retain);
  LOAD(callback_completion_destroy);
  LOAD(callback_store_load_complete);
  vtable.load = load;
  vtable.save = unexpected_write;
  vtable.clear = unexpected_write;
  vtable.destroy = destroy_store;
  REQUIRE(api.store_registration_new(&vtable, context, &registration, NULL) == RUMQTTC_OK);
  REQUIRE(api.execution_context_new(NULL, &execution, NULL) == RUMQTTC_OK);
  REQUIRE(api.config_new(RUMQTTC_PROTOCOL_V4, &config, NULL) == RUMQTTC_OK);
  REQUIRE(api.config_set_execution_context(config, execution, NULL) == RUMQTTC_OK);
  REQUIRE(api.config_set_broker(config, view("127.0.0.1"), 1883, NULL) == RUMQTTC_OK);
  REQUIRE(api.config_set_client_id(config, view("native-unload"), NULL) == RUMQTTC_OK);
  REQUIRE(api.config_set_v4_clean_session(config, 0, NULL) == RUMQTTC_OK);
  REQUIRE(api.config_set_session_store(config, registration, view("unload-scope"), 5000, 1024, NULL) == RUMQTTC_OK);
  REQUIRE(api.client_start(config, &client, NULL) == RUMQTTC_OK);
  for (unsigned attempt = 0; attempt < 500 && atomic_load(&pending) == 0; ++attempt) {
#if defined(_WIN32)
    Sleep(10);
#else
    struct timespec delay = {0, 10000000};
    nanosleep(&delay, NULL);
#endif
  }
  completion = (rumqttc_callback_completion_t *)atomic_load(&pending);
  REQUIRE(completion != NULL);
  REQUIRE(api.client_close_now_timeout_ms(client, 5000, NULL) == RUMQTTC_OK);
  REQUIRE(api.client_destroy_timeout_ms(client, 5000, NULL) == RUMQTTC_OK);
  REQUIRE(api.execution_context_request_shutdown(execution, NULL) == RUMQTTC_OK);
  REQUIRE(api.execution_context_join_timeout_ms(execution, 5000, NULL) == RUMQTTC_OK);
  api.config_destroy(config);
  api.execution_context_release(execution);
  api.store_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == 0);
  REQUIRE(api.callback_store_load_complete(completion, RUMQTTC_STORE_NOT_FOUND, (rumqttc_bytes_view_t){NULL, 0}) ==
          RUMQTTC_INVALID_STATE);
  api.callback_completion_destroy(completion);
  REQUIRE(atomic_load(&destroyed) == 1);
  REQUIRE(close_module(module));
  return 0;
}
