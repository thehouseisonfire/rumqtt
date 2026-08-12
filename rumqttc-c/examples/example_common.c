#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#include <windows.h>
#else
#include <pthread.h>
#include <time.h>
#endif

struct example_thread {
  example_thread_fn function;
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

rumqttc_string_view_t example_string(const char *value) {
  rumqttc_string_view_t view = {value, strlen(value)};
  return view;
}

rumqttc_bytes_view_t example_bytes(const void *data, size_t length) {
  rumqttc_bytes_view_t view = {data, length};
  return view;
}

rumqttc_publish_options_t example_publish_options(rumqttc_qos_t qos) {
  rumqttc_publish_options_t options = RUMQTTC_PUBLISH_OPTIONS_INIT;
  options.qos = qos;
  return options;
}

int example_report(rumqttc_status_t status, rumqttc_error_t **error,
                   const char *operation) {
  rumqttc_string_view_t message = {NULL, 0};
  if (status == RUMQTTC_OK) {
    return 0;
  }
  if (error != NULL && *error != NULL &&
      rumqttc_error_message(*error, &message) == RUMQTTC_OK) {
    fprintf(stderr, "%s failed (%u): %.*s\n", operation, status,
            (int)message.len, message.data);
  } else {
    fprintf(stderr, "%s failed with status %u\n", operation, status);
  }
  if (error != NULL) {
    rumqttc_error_destroy(*error);
    *error = NULL;
  }
  return -1;
}

rumqttc_client_t *example_connect(const char *host, uint16_t port,
                                  const char *client_id,
                                  rumqttc_ack_mode_t ack_mode) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_event_t *event = NULL;
  rumqttc_error_t *error = NULL;
  if (example_report(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, &error),
                     &error, "config_new") ||
      example_report(
          rumqttc_config_set_broker(config, example_string(host), port, &error),
          &error, "set_broker") ||
      example_report(rumqttc_config_set_client_id(
                         config, example_string(client_id), &error),
                     &error, "set_client_id") ||
      example_report(rumqttc_config_set_ack_mode(config, ack_mode, &error),
                     &error, "set_ack_mode") ||
      example_report(rumqttc_client_start(config, &client, &error), &error,
                     "client_start")) {
    rumqttc_config_destroy(config);
    example_destroy_client(&client);
    return NULL;
  }
  rumqttc_config_destroy(config);
  event = example_next_event(client, RUMQTTC_EVENT_CONNECTED);
  if (event == NULL) {
    example_destroy_client(&client);
    return NULL;
  }
  rumqttc_event_destroy(event);
  return client;
}

rumqttc_event_t *example_next_event(rumqttc_client_t *client,
                                    rumqttc_event_kind_t wanted) {
  for (;;) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    rumqttc_error_t *error = NULL;
    rumqttc_status_t status =
        rumqttc_client_event_recv_timeout_ms(client, 5000, &event, &error);
    if (example_report(status, &error, "event_recv")) {
      return NULL;
    }
    if (rumqttc_event_kind(event, &kind) != RUMQTTC_OK) {
      rumqttc_event_destroy(event);
      return NULL;
    }
    if (kind == wanted) {
      return event;
    }
    rumqttc_event_destroy(event);
  }
}

int example_wait(rumqttc_completion_t *completion,
                 rumqttc_completion_kind_t wanted) {
  rumqttc_error_t *error = NULL;
  rumqttc_completion_kind_t kind = 0;
  if (example_report(
          rumqttc_completion_wait_timeout_ms(completion, 5000, &error), &error,
          "completion_wait") ||
      example_report(rumqttc_completion_kind(completion, &kind, &error), &error,
                     "completion_kind")) {
    return -1;
  }
  return kind == wanted ? 0 : -1;
}

void example_destroy_client(rumqttc_client_t **client) {
  rumqttc_error_t *error = NULL;
  rumqttc_status_t status;
  if (client == NULL || *client == NULL) {
    return;
  }
  status = rumqttc_client_destroy_timeout_ms(*client, 5000, &error);
  if (status != RUMQTTC_OK) {
    (void)example_report(status, &error, "client_destroy");
    rumqttc_client_abandon(*client);
  }
  *client = NULL;
}

#if defined(_WIN32)
static DWORD WINAPI example_thread_entry(LPVOID argument) {
  example_thread_t *thread = argument;
  thread->result = thread->function(thread->argument);
  return 0;
}
#else
static void *example_thread_entry(void *argument) {
  example_thread_t *thread = argument;
  thread->result = thread->function(thread->argument);
  (void)pthread_mutex_lock(&thread->mutex);
  thread->finished = 1;
  (void)pthread_cond_broadcast(&thread->finished_changed);
  (void)pthread_mutex_unlock(&thread->mutex);
  return NULL;
}
#endif

example_thread_t *example_thread_start(example_thread_fn function,
                                       void *argument) {
  example_thread_t *thread = calloc(1, sizeof(*thread));
  if (thread == NULL) {
    return NULL;
  }
  thread->function = function;
  thread->argument = argument;
#if defined(_WIN32)
  thread->handle = CreateThread(NULL, 0, example_thread_entry, thread, 0, NULL);
  if (thread->handle == NULL) {
    free(thread);
    return NULL;
  }
#else
  if (pthread_mutex_init(&thread->mutex, NULL) != 0) {
    free(thread);
    return NULL;
  }
  if (pthread_cond_init(&thread->finished_changed, NULL) != 0) {
    (void)pthread_mutex_destroy(&thread->mutex);
    free(thread);
    return NULL;
  }
  if (pthread_create(&thread->handle, NULL, example_thread_entry, thread) !=
      0) {
    (void)pthread_cond_destroy(&thread->finished_changed);
    (void)pthread_mutex_destroy(&thread->mutex);
    free(thread);
    return NULL;
  }
#endif
  return thread;
}

int example_thread_join(example_thread_t *thread, uint32_t timeout_ms,
                        int *result_out) {
  int result;
  if (thread == NULL || result_out == NULL) {
    return -1;
  }
#if defined(_WIN32)
  if (WaitForSingleObject(thread->handle, timeout_ms) != WAIT_OBJECT_0) {
    return -1;
  }
  CloseHandle(thread->handle);
#else
  {
    struct timespec deadline;
    if (clock_gettime(CLOCK_REALTIME, &deadline) != 0) {
      return -1;
    }
    deadline.tv_sec += (time_t)(timeout_ms / 1000);
    deadline.tv_nsec += (long)(timeout_ms % 1000) * 1000000L;
    if (deadline.tv_nsec >= 1000000000L) {
      ++deadline.tv_sec;
      deadline.tv_nsec -= 1000000000L;
    }
    if (pthread_mutex_lock(&thread->mutex) != 0) {
      return -1;
    }
    while (!thread->finished) {
      if (pthread_cond_timedwait(&thread->finished_changed, &thread->mutex,
                                 &deadline) != 0) {
        (void)pthread_mutex_unlock(&thread->mutex);
        return -1;
      }
    }
    (void)pthread_mutex_unlock(&thread->mutex);
  }
  if (pthread_join(thread->handle, NULL) != 0) {
    return -1;
  }
  (void)pthread_cond_destroy(&thread->finished_changed);
  (void)pthread_mutex_destroy(&thread->mutex);
#endif
  result = thread->result;
  free(thread);
  *result_out = result;
  return 0;
}
