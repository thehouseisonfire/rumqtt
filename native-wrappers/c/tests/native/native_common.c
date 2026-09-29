#include "native_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#include <windows.h>
#include <tlhelp32.h>
#else
#include <pthread.h>
#include <time.h>
#if defined(__APPLE__)
#include <mach/mach.h>
#else
#include <dirent.h>
#endif
#endif

struct native_thread {
  native_thread_fn function;
  void *argument;
  int result;
#if defined(_WIN32)
  HANDLE handle;
#else
  pthread_t handle;
  pthread_mutex_t mutex;
  pthread_cond_t finished_changed;
  int finished;
#endif
};

void native_fail(const char *file, int line, const char *expression,
                 rumqttc_status_t status) {
  if (status == UINT32_MAX) {
    fprintf(stderr, "%s:%d: requirement failed: %s\n", file, line, expression);
  } else {
    fprintf(stderr, "%s:%d: %s returned status %u\n", file, line, expression,
            status);
  }
  fflush(stderr);
  abort();
}

uint64_t native_monotonic_ms(void) {
#if defined(_WIN32)
  return GetTickCount64();
#else
  struct timespec now;
  REQUIRE(clock_gettime(CLOCK_MONOTONIC, &now) == 0);
  return (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
#endif
}

static void fixture_path(char *path, size_t capacity, const char *name, const char *suffix) {
  const char *directory = getenv("RUMQTTC_TEST_CONTROL_DIR");
  REQUIRE(directory != NULL && strchr(name, '/') == NULL && strchr(name, '\\') == NULL);
  int length = snprintf(path, capacity, "%s/%s%s", directory, name, suffix);
  REQUIRE(length > 0 && (size_t)length < capacity);
}

void native_fixture_write(const char *name, uint64_t value) {
  char path[1024], temporary[1024];
  fixture_path(path, sizeof(path), name, "");
  fixture_path(temporary, sizeof(temporary), name, ".tmp");
  FILE *file = fopen(temporary, "w");
  REQUIRE(file != NULL);
  REQUIRE(fprintf(file, "%llu\n", (unsigned long long)value) > 0 && fclose(file) == 0);
  REQUIRE(rename(temporary, path) == 0);
}

uint64_t native_fixture_read(const char *name) {
  char path[1024];
  fixture_path(path, sizeof(path), name, "");
  uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
  do {
    FILE *file = fopen(path, "r");
    if (file != NULL) {
      unsigned long long value = 0;
#if defined(_MSC_VER)
      REQUIRE(fscanf_s(file, "%llu", &value) == 1 && fclose(file) == 0);
#else
      REQUIRE(fscanf(file, "%llu", &value) == 1 && fclose(file) == 0);
#endif
      return (uint64_t)value;
    }
    native_sleep_ms(1);
  } while (native_monotonic_ms() < deadline);
  native_fail(__FILE__, __LINE__, name, RUMQTTC_TIMEOUT);
  return 0;
}

rumqttc_string_view_t native_string(const char *value) {
  rumqttc_string_view_t view = {value, strlen(value)};
  return view;
}

rumqttc_bytes_view_t native_bytes(const uint8_t *data, size_t length) {
  rumqttc_bytes_view_t view = {data, length};
  return view;
}

rumqttc_publish_options_t native_publish_options(rumqttc_qos_t qos) {
  rumqttc_publish_options_t options = RUMQTTC_PUBLISH_OPTIONS_INIT;
  options.qos = qos;
  return options;
}

rumqttc_subscription_t native_subscription(const char *filter,
                                           rumqttc_qos_t qos) {
  rumqttc_subscription_t subscription = RUMQTTC_SUBSCRIPTION_INIT;
  subscription.filter = native_string(filter);
  subscription.qos = qos;
  return subscription;
}

uint16_t native_test_port(void) {
  const char *value = getenv("RUMQTTC_TEST_PORT");
  char *end = NULL;
  unsigned long port;
  REQUIRE(value != NULL);
  port = strtoul(value, &end, 10);
  REQUIRE(end != value && *end == '\0' && port > 0 && port <= UINT16_MAX);
  return (uint16_t)port;
}

rumqttc_client_t *
native_start_client(rumqttc_protocol_t protocol, const char *client_id,
                    rumqttc_ack_mode_t ack_mode, uint32_t request_capacity,
                    uint32_t event_capacity, uint64_t event_timeout_ms) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  CHECK(rumqttc_config_set_request_capacity(config, request_capacity, NULL));
  CHECK(rumqttc_config_set_event_capacity(config, event_capacity, NULL));
  CHECK(rumqttc_config_set_event_delivery_timeout_ms(config, event_timeout_ms,
                                                     NULL));
  CHECK(rumqttc_config_set_ack_mode(config, ack_mode, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_config_destroy(config);
  {
    rumqttc_event_t *connected =
        native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    rumqttc_protocol_t observed_protocol = 0;
    uint8_t session_present = 99;
    CHECK(rumqttc_event_connected(connected, &observed_protocol, NULL));
    REQUIRE(observed_protocol == protocol);
    CHECK(rumqttc_event_connected(connected, NULL, &session_present));
    REQUIRE(session_present <= 1);
    {
      uint8_t reason = 255;
      uint8_t present = 255;
      CHECK(rumqttc_event_connack_reason(connected, &reason));
      REQUIRE(reason == 0);
      if (protocol == RUMQTTC_PROTOCOL_V5) {
        uint64_t scalar = UINT64_MAX;
        size_t property_count = SIZE_MAX;
        rumqttc_string_view_t text = {(const char *)(uintptr_t)1, 99};
        rumqttc_bytes_view_t data = {(const uint8_t *)(uintptr_t)1, 99};
        CHECK(rumqttc_event_connack_v5_scalar(
            connected, RUMQTTC_CONNACK_SCALAR_RECEIVE_MAXIMUM, &present,
            &scalar));
        REQUIRE(present == 0 && scalar == 0);
        CHECK(rumqttc_event_connack_v5_string(
            connected, RUMQTTC_CONNACK_STRING_REASON, &present, &text));
        REQUIRE(present == 0 && text.data == NULL && text.len == 0);
        CHECK(rumqttc_event_connack_v5_authentication_data(connected, &present,
                                                            &data));
        REQUIRE(present == 0 && data.data == NULL && data.len == 0);
        CHECK(rumqttc_event_user_property_count(
            connected, RUMQTTC_EVENT_PROPERTIES_CONNACK, &property_count));
        REQUIRE(property_count == 0);
      } else {
        uint64_t scalar = UINT64_MAX;
        REQUIRE(rumqttc_event_connack_v5_scalar(
                    connected, RUMQTTC_CONNACK_SCALAR_RECEIVE_MAXIMUM,
                    &present, &scalar) == RUMQTTC_INVALID_STATE);
        REQUIRE(present == 0 && scalar == 0);
      }
    }
    REQUIRE(rumqttc_event_connected(connected, NULL, NULL) ==
            RUMQTTC_INVALID_ARGUMENT);
    rumqttc_event_destroy(connected);
  }
  return client;
}

rumqttc_event_t *native_wait_event(rumqttc_client_t *client,
                                   rumqttc_event_kind_t expected) {
  rumqttc_event_t *event = NULL;
  rumqttc_event_kind_t kind = 0;
  for (;;) {
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS,
                                               &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    native_check_event_accessors(event);
    if (kind == expected) {
      return event;
    }
    rumqttc_event_destroy(event);
    event = NULL;
  }
}

void native_wait_completion(rumqttc_completion_t *completion,
                            rumqttc_completion_kind_t expected) {
  rumqttc_completion_kind_t kind = 0;
  CHECK(
      rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_completion_kind(completion, &kind, NULL));
  REQUIRE(kind == expected);
}

void native_close_destroy(rumqttc_client_t *client) {
  if (client != NULL) {
    CHECK(rumqttc_client_close_now_timeout_ms(client, 5000, NULL));
    CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));
  }
}

void native_sleep_ms(uint32_t milliseconds) {
#if defined(_WIN32)
  Sleep(milliseconds);
#else
  struct timespec duration = {
      (time_t)(milliseconds / 1000),
      (long)(milliseconds % 1000) * 1000000L,
  };
  while (nanosleep(&duration, &duration) != 0) {
  }
#endif
}

size_t native_process_thread_count(void) {
#if defined(_WIN32)
  HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
  THREADENTRY32 entry = {sizeof(entry)};
  size_t count = 0;
  DWORD process = GetCurrentProcessId();
  REQUIRE(snapshot != INVALID_HANDLE_VALUE);
  if (Thread32First(snapshot, &entry)) {
    do {
      if (entry.th32OwnerProcessID == process) {
        ++count;
      }
    } while (Thread32Next(snapshot, &entry));
  }
  CloseHandle(snapshot);
  return count;
#elif defined(__APPLE__)
  thread_act_array_t threads = NULL;
  mach_msg_type_number_t count = 0;
  kern_return_t result = task_threads(mach_task_self(), &threads, &count);
  REQUIRE(result == KERN_SUCCESS);
  if (threads != NULL) {
    vm_deallocate(mach_task_self(), (vm_address_t)threads,
                  (vm_size_t)(count * sizeof(thread_t)));
  }
  return (size_t)count;
#else
  DIR *directory = opendir("/proc/self/task");
  struct dirent *entry;
  size_t count = 0;
  REQUIRE(directory != NULL);
  while ((entry = readdir(directory)) != NULL) {
    if (entry->d_name[0] != '.') {
      ++count;
    }
  }
  closedir(directory);
  return count;
#endif
}

#if defined(_WIN32)
static DWORD WINAPI native_thread_entry(LPVOID argument) {
  native_thread_t *thread = argument;
  thread->result = thread->function(thread->argument);
  return 0;
}
#else
static void *native_thread_entry(void *argument) {
  native_thread_t *thread = argument;
  thread->result = thread->function(thread->argument);
  REQUIRE(pthread_mutex_lock(&thread->mutex) == 0);
  thread->finished = 1;
  REQUIRE(pthread_cond_broadcast(&thread->finished_changed) == 0);
  REQUIRE(pthread_mutex_unlock(&thread->mutex) == 0);
  return NULL;
}
#endif

native_thread_t *native_thread_start(native_thread_fn function,
                                     void *argument) {
  native_thread_t *thread = calloc(1, sizeof(*thread));
  REQUIRE(thread != NULL);
  thread->function = function;
  thread->argument = argument;
#if defined(_WIN32)
  thread->handle = CreateThread(NULL, 0, native_thread_entry, thread, 0, NULL);
  REQUIRE(thread->handle != NULL);
#else
  REQUIRE(pthread_mutex_init(&thread->mutex, NULL) == 0);
  REQUIRE(pthread_cond_init(&thread->finished_changed, NULL) == 0);
  REQUIRE(pthread_create(&thread->handle, NULL, native_thread_entry, thread) ==
          0);
#endif
  return thread;
}

int native_thread_join(native_thread_t *thread) {
  int result;
  REQUIRE(thread != NULL);
#if defined(_WIN32)
  REQUIRE(WaitForSingleObject(thread->handle, NATIVE_DEADLINE_MS) ==
          WAIT_OBJECT_0);
  CloseHandle(thread->handle);
#else
  {
    struct timespec deadline;
    REQUIRE(clock_gettime(CLOCK_REALTIME, &deadline) == 0);
    deadline.tv_sec += NATIVE_DEADLINE_MS / 1000;
    deadline.tv_nsec += (long)(NATIVE_DEADLINE_MS % 1000) * 1000000L;
    if (deadline.tv_nsec >= 1000000000L) {
      ++deadline.tv_sec;
      deadline.tv_nsec -= 1000000000L;
    }
    REQUIRE(pthread_mutex_lock(&thread->mutex) == 0);
    while (!thread->finished) {
      REQUIRE(pthread_cond_timedwait(&thread->finished_changed, &thread->mutex,
                                     &deadline) == 0);
    }
    REQUIRE(pthread_mutex_unlock(&thread->mutex) == 0);
  }
  REQUIRE(pthread_join(thread->handle, NULL) == 0);
  REQUIRE(pthread_cond_destroy(&thread->finished_changed) == 0);
  REQUIRE(pthread_mutex_destroy(&thread->mutex) == 0);
#endif
  result = thread->result;
  free(thread);
  return result;
}
