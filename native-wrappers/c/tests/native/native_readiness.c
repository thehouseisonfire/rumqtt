#include "native_common.h"
#include <stdio.h>
#if defined(_WIN32)
#include <windows.h>
#else
#include <errno.h>
#include <poll.h>
#include <unistd.h>
#endif

#define CLIENTS 12

static void wait_context(rumqttc_notification_context_t *context, uint32_t timeout_ms) {
#if defined(_WIN32)
  void *handle = NULL;
  /* ERROR_OUT_SUCCESS: rumqttc_notification_borrowed_windows_handle */
  CHECK(rumqttc_notification_borrowed_windows_handle(context, &handle, NULL));
  REQUIRE(WaitForSingleObject((HANDLE)handle, timeout_ms) == WAIT_OBJECT_0);
#else
  int32_t fd = -1;
  struct pollfd descriptor;
  /* ERROR_OUT_SUCCESS: rumqttc_notification_borrowed_posix_fd */
  CHECK(rumqttc_notification_borrowed_posix_fd(context, &fd, NULL));
  descriptor.fd = fd;
  descriptor.events = POLLIN;
  descriptor.revents = 0;
  int result;
  do { result = poll(&descriptor, 1, (int)timeout_ms); } while (result < 0 && errno == EINTR);
  REQUIRE(result == 1 && (descriptor.revents & POLLIN) != 0);
#endif
}

static void exercise(rumqttc_protocol_t protocol, int shared) {
  rumqttc_notification_context_t *context = NULL, *retained = NULL;
  rumqttc_execution_context_t *execution = NULL;
  rumqttc_client_t *clients[CLIENTS] = {0};
  rumqttc_pending_admission_t *pending[CLIENTS] = {0};
  rumqttc_completion_t *completions[CLIENTS] = {0};
  rumqttc_notification_key_t event_keys[CLIENTS], pending_keys[CLIENTS], completion_keys[CLIENTS];
  rumqttc_notification_key_t invalid = {UINT64_MAX, UINT64_MAX};
  rumqttc_notification_status_t status = RUMQTTC_NOTIFICATION_STATUS_INIT;
  unsigned completed = 0;
  unsigned invalidated = 0;
  uint32_t ready = 99;
  uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
  size_t threads_before = native_process_thread_count();
  /* ERROR_OUT_FAILURE: rumqttc_notification_context_new */
  REQUIRE(rumqttc_notification_context_new(NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_notification_context_new */
  CHECK(rumqttc_notification_context_new(NULL, &context, NULL));
  REQUIRE(native_process_thread_count() == threads_before);
  /* ERROR_OUT_FAILURE: rumqttc_notification_context_retain */
  REQUIRE(rumqttc_notification_context_retain(NULL, &retained, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_notification_context_retain */
  CHECK(rumqttc_notification_context_retain(context, &retained, NULL));
  rumqttc_notification_context_release(context);
  context = retained;
  /* ERROR_OUT_FAILURE: rumqttc_notification_context_status */
  REQUIRE(rumqttc_notification_context_status(NULL, &status, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_notification_context_status */
  CHECK(rumqttc_notification_context_status(context, &status, NULL));
  REQUIRE(status.state == RUMQTTC_NOTIFICATION_OPEN && status.capacity == 1024 && status.signal_error == 0);
  /* ERROR_OUT_FAILURE: rumqttc_notification_prepare_wait */
  REQUIRE(rumqttc_notification_prepare_wait(NULL, &ready, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_notification_prepare_wait */
  CHECK(rumqttc_notification_prepare_wait(context, &ready, NULL));
  REQUIRE(ready == 0);
  /* ERROR_OUT_FAILURE: rumqttc_notification_context_try_join */
  REQUIRE(rumqttc_notification_context_try_join(context, NULL) == RUMQTTC_WOULD_BLOCK);
  /* ERROR_OUT_FAILURE: rumqttc_notification_register_events */
  REQUIRE(rumqttc_notification_register_events(context, NULL, 0, &invalid, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_FAILURE: rumqttc_notification_register_completion */
  REQUIRE(rumqttc_notification_register_completion(context, NULL, 0, &invalid, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_FAILURE: rumqttc_notification_register_pending_admission */
  REQUIRE(rumqttc_notification_register_pending_admission(context, NULL, 0, &invalid, NULL) == RUMQTTC_INVALID_ARGUMENT);
  invalid.slot = UINT64_MAX; invalid.generation = UINT64_MAX;
  /* ERROR_OUT_FAILURE: rumqttc_notification_acknowledge */
  REQUIRE(rumqttc_notification_acknowledge(context, invalid, 1, NULL) == RUMQTTC_INVALID_STATE);
  /* ERROR_OUT_FAILURE: rumqttc_notification_deregister */
  REQUIRE(rumqttc_notification_deregister(context, invalid, NULL) == RUMQTTC_INVALID_STATE);
  if (shared) CHECK(rumqttc_execution_context_new(NULL, &execution, NULL));
  for (unsigned i = 0; i < CLIENTS; ++i) {
    rumqttc_config_t *config = NULL;
    rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
    char id[64];
    REQUIRE(snprintf(id, sizeof(id), "native-readiness-%u-%d-%u", (unsigned)protocol, shared, i) > 0);
    CHECK(rumqttc_config_new(protocol, &config, NULL));
    CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
    CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
    if (execution != NULL) CHECK(rumqttc_config_set_execution_context(config, execution, NULL));
    CHECK(rumqttc_client_start(config, &clients[i], NULL));
    rumqttc_config_destroy(config);
    /* ERROR_OUT_SUCCESS: rumqttc_notification_register_events */
    CHECK(rumqttc_notification_register_events(context, clients[i], i, &event_keys[i], NULL));
    CHECK(rumqttc_client_publish_deferred(clients[i], native_string("rumqttc/native/readiness"),
        native_bytes((const uint8_t *)"hello", 5), &options, &pending[i], NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_notification_register_pending_admission */
    CHECK(rumqttc_notification_register_pending_admission(context, pending[i], i, &pending_keys[i], NULL));
  }
  while (completed != CLIENTS) {
    rumqttc_notification_ready_t entries[4];
    size_t count = 99;
    /* ERROR_OUT_FAILURE: rumqttc_notification_collect */
    REQUIRE(rumqttc_notification_collect(context, NULL, sizeof(entries[0]), 4, &count, NULL) == RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(count == 0);
    rumqttc_status_t result = rumqttc_notification_collect(context, entries, sizeof(entries[0]), 4, &count, NULL);
    REQUIRE(result == RUMQTTC_OK || result == RUMQTTC_WOULD_BLOCK);
    REQUIRE(native_monotonic_ms() < deadline);
    if (result == RUMQTTC_OK) {
      /* ERROR_OUT_SUCCESS: rumqttc_notification_collect */
      for (size_t j = 0; j < count; ++j) {
        rumqttc_notification_ready_t *entry = &entries[j];
        unsigned i = (unsigned)entry->tag;
        REQUIRE(i < CLIENTS);
        if (entry->source_kind == RUMQTTC_NOTIFICATION_EVENTS) {
          for (;;) {
            rumqttc_event_t *event = NULL;
            rumqttc_status_t received = rumqttc_client_event_try_recv(clients[i], &event, NULL);
            if (received == RUMQTTC_WOULD_BLOCK) break;
            REQUIRE(received == RUMQTTC_OK);
            rumqttc_event_destroy(event);
          }
          /* ERROR_OUT_SUCCESS: rumqttc_notification_acknowledge */
          CHECK(rumqttc_notification_acknowledge(context, entry->key, entry->revision, NULL));
        } else if (entry->source_kind == RUMQTTC_NOTIFICATION_PENDING_ADMISSION) {
          uint32_t state = 99;
          CHECK(rumqttc_pending_admission_poll(pending[i], &state, NULL, &completions[i], NULL));
          REQUIRE(state == RUMQTTC_ADMISSION_ADMITTED && completions[i] != NULL);
          /* ERROR_OUT_SUCCESS: rumqttc_notification_deregister */
          CHECK(rumqttc_notification_deregister(context, entry->key, NULL));
          rumqttc_pending_admission_release(pending[i]); pending[i] = NULL;
          /* ERROR_OUT_SUCCESS: rumqttc_notification_register_completion */
          CHECK(rumqttc_notification_register_completion(context, completions[i], i, &completion_keys[i], NULL));
        } else {
          REQUIRE(entry->source_kind == RUMQTTC_NOTIFICATION_COMPLETION);
          CHECK(rumqttc_completion_poll(completions[i], NULL));
          CHECK(rumqttc_notification_acknowledge(context, entry->key, entry->revision, NULL));
          ++completed;
        }
      }
    }
    CHECK(rumqttc_notification_prepare_wait(context, &ready, NULL));
    if (!ready && completed != CLIENTS) wait_context(context, NATIVE_DEADLINE_MS);
  }
  for (unsigned i = 0; i < CLIENTS; ++i) {
    native_close_destroy(clients[i]); clients[i] = NULL;
    CHECK(rumqttc_completion_poll(completions[i], NULL));
  }
  /* Invalidated event registrations remain available independently of destroyed clients. */
  while (invalidated != CLIENTS) {
    rumqttc_notification_ready_t entry;
    size_t count = 0;
    CHECK(rumqttc_notification_collect(context, &entry, sizeof(entry), 1, &count, NULL));
    REQUIRE(count == 1 && entry.source_kind == RUMQTTC_NOTIFICATION_EVENTS);
    REQUIRE((entry.flags & RUMQTTC_NOTIFICATION_INVALIDATED) != 0);
    CHECK(rumqttc_notification_acknowledge(context, entry.key, entry.revision, NULL));
    CHECK(rumqttc_notification_deregister(context, entry.key, NULL));
    ++invalidated;
  }
  for (unsigned i = 0; i < CLIENTS; ++i) {
    CHECK(rumqttc_notification_deregister(context, completion_keys[i], NULL));
    rumqttc_completion_destroy(completions[i]);
  }
#if defined(_WIN32)
  void *duplicate = NULL;
  /* ERROR_OUT_FAILURE: rumqttc_notification_borrowed_windows_handle */
  REQUIRE(rumqttc_notification_borrowed_windows_handle(context, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_FAILURE: rumqttc_notification_duplicate_windows_handle */
  REQUIRE(rumqttc_notification_duplicate_windows_handle(context, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_notification_duplicate_windows_handle */
  CHECK(rumqttc_notification_duplicate_windows_handle(context, &duplicate, NULL));
#else
  int32_t duplicate = -1;
  /* ERROR_OUT_FAILURE: rumqttc_notification_borrowed_posix_fd */
  REQUIRE(rumqttc_notification_borrowed_posix_fd(context, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_FAILURE: rumqttc_notification_duplicate_posix_fd */
  REQUIRE(rumqttc_notification_duplicate_posix_fd(context, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_notification_duplicate_posix_fd */
  CHECK(rumqttc_notification_duplicate_posix_fd(context, &duplicate, NULL));
#endif
  /* ERROR_OUT_FAILURE: rumqttc_notification_context_request_close */
  REQUIRE(rumqttc_notification_context_request_close(NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  /* ERROR_OUT_SUCCESS: rumqttc_notification_context_request_close */
  CHECK(rumqttc_notification_context_request_close(context, NULL));
  wait_context(context, NATIVE_DEADLINE_MS);
  /* ERROR_OUT_SUCCESS: rumqttc_notification_context_try_join */
  CHECK(rumqttc_notification_context_try_join(context, NULL));
  CHECK(rumqttc_notification_context_status(context, &status, NULL));
  REQUIRE(status.state == RUMQTTC_NOTIFICATION_QUIESCENT && status.registrations == 0);
  /* The local wait set is no longer used. Duplicates have independent ownership. */
  rumqttc_notification_context_release(context);
#if defined(_WIN32)
  REQUIRE(CloseHandle((HANDLE)duplicate) != 0);
#else
  REQUIRE(close(duplicate) == 0);
#endif
  if (execution != NULL) {
    CHECK(rumqttc_execution_context_request_shutdown(execution, NULL));
    CHECK(rumqttc_execution_context_join_timeout_ms(execution, NATIVE_DEADLINE_MS, NULL));
    rumqttc_execution_context_release(execution);
  }
}

int main(void) {
  if (rumqttc_notification_capabilities() == 0) return 77;
  REQUIRE((rumqttc_library_capabilities() & RUMQTTC_CAP_READINESS) != 0);
  for (unsigned protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol)
    for (int shared = 0; shared != 2; ++shared) exercise(protocol, shared);
  return 0;
}
