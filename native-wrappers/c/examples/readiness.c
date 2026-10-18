/* One host loop observes first connection, events, deferred admission, and MQTT completion.
 * Build the native library with --features readiness. No observer thread is needed. */
#include "example_common.h"
#include <stdio.h>
#include <stdlib.h>
#if defined(_WIN32)
#include <windows.h>
#else
#include <errno.h>
#include <poll.h>
#endif

#define RUN(call) do { if ((call) != RUMQTTC_OK) { fputs(#call " failed\n", stderr); goto cleanup; } } while (0)

static int wait_for_change(rumqttc_notification_context_t *context) {
#if defined(_WIN32)
  void *handle = NULL;
  if (rumqttc_notification_borrowed_windows_handle(context, &handle, NULL) != RUMQTTC_OK) return 1;
  return WaitForSingleObject((HANDLE)handle, 5000) != WAIT_OBJECT_0;
#else
  int32_t fd = -1;
  struct pollfd descriptor;
  int result;
  if (rumqttc_notification_borrowed_posix_fd(context, &fd, NULL) != RUMQTTC_OK) return 1;
  descriptor.fd = fd; descriptor.events = POLLIN; descriptor.revents = 0;
  do { result = poll(&descriptor, 1, 5000); } while (result < 0 && errno == EINTR);
  return result != 1 || (descriptor.revents & POLLIN) == 0;
#endif
}

int main(int argc, char **argv) {
  rumqttc_notification_context_t *context = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_pending_admission_t *pending = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_first_connection_t *connection = NULL;
  rumqttc_notification_key_t connection_key = {0, 0};
  rumqttc_notification_key_t event_key = {0, 0}, pending_key = {0, 0}, completion_key = {0, 0};
  rumqttc_publish_options_t options = example_publish_options(RUMQTTC_QOS_1);
  int finished = 0, connected = 0, result = 1;
  if (argc != 3) { fprintf(stderr, "usage: %s HOST PORT\n", argv[0]); return 2; }
  if (rumqttc_notification_capabilities() == 0) return 77;
  RUN(rumqttc_notification_context_new(NULL, &context, NULL));
  client = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10), "c-readiness", RUMQTTC_ACK_AUTOMATIC);
  if (client == NULL) goto cleanup;
  /* The startup helper already observed success; late registrations report it too. */
  RUN(rumqttc_client_first_connection(client, &connection, NULL));
  RUN(rumqttc_notification_register_first_connection(context, connection, 4, &connection_key, NULL));
  RUN(rumqttc_notification_register_events(context, client, 1, &event_key, NULL));
  RUN(rumqttc_client_publish_deferred(client, example_string("rumqttc/native/readiness-example"),
      example_bytes("hello", 5), &options, &pending, NULL));
  RUN(rumqttc_notification_register_pending_admission(context, pending, 2, &pending_key, NULL));
  while (!finished || !connected) {
    rumqttc_notification_ready_t entries[8];
    size_t count = 0;
    uint32_t service_again = 0;
    rumqttc_status_t status = rumqttc_notification_collect(context, entries, sizeof(entries[0]), 8, &count, NULL);
    if (status != RUMQTTC_OK && status != RUMQTTC_WOULD_BLOCK) goto cleanup;
    for (size_t i = 0; i < count; ++i) {
      rumqttc_notification_ready_t *entry = &entries[i];
      if (entry->source_kind == RUMQTTC_NOTIFICATION_EVENTS) {
        for (;;) {
          rumqttc_event_t *event = NULL;
          status = rumqttc_client_event_try_recv(client, &event, NULL);
          if (status == RUMQTTC_WOULD_BLOCK) break;
          if (status != RUMQTTC_OK) goto cleanup;
          /* Process publication/connection data here before releasing its event. */
          rumqttc_event_destroy(event);
        }
        RUN(rumqttc_notification_acknowledge(context, entry->key, entry->revision, NULL));
      } else if (entry->source_kind == RUMQTTC_NOTIFICATION_FIRST_CONNECTION) {
        uint32_t state = RUMQTTC_FIRST_CONNECTION_UNOBSERVED;
        RUN(rumqttc_first_connection_poll(connection, &state, NULL, NULL, NULL));
        if (state != RUMQTTC_FIRST_CONNECTION_SUCCEEDED) goto cleanup;
        /* Historical success; reconnect events still belong to the event consumer. */
        RUN(rumqttc_notification_acknowledge(context, entry->key, entry->revision, NULL));
        connected = 1;
      } else if (entry->source_kind == RUMQTTC_NOTIFICATION_PENDING_ADMISSION) {
        uint32_t state = RUMQTTC_ADMISSION_WAITING;
        RUN(rumqttc_pending_admission_poll(pending, &state, NULL, &completion, NULL));
        if (state != RUMQTTC_ADMISSION_ADMITTED || completion == NULL) goto cleanup;
        /* Admission and MQTT completion are different sources. */
        RUN(rumqttc_notification_deregister(context, pending_key, NULL));
        pending_key.generation = 0;
        rumqttc_pending_admission_release(pending); pending = NULL;
        RUN(rumqttc_notification_register_completion(context, completion, 3, &completion_key, NULL));
      } else if (entry->source_kind == RUMQTTC_NOTIFICATION_COMPLETION) {
        RUN(rumqttc_completion_poll(completion, NULL));
        RUN(rumqttc_notification_acknowledge(context, entry->key, entry->revision, NULL));
        finished = 1;
      }
    }
    RUN(rumqttc_notification_prepare_wait(context, &service_again, NULL));
    if ((!finished || !connected) && !service_again && wait_for_change(context) != 0) goto cleanup;
  }
  result = 0;
cleanup:
  if (context != NULL) {
    /* No host waits remain active here; detach any persistent reactor wait set here too.
     * Closing drops registrations. For a final waiting pending owner this requests
     * cancellation, which alone does not prove that admission lost the race. */
    if (rumqttc_notification_context_request_close(context, NULL) != RUMQTTC_OK) result = 1;
    if (rumqttc_notification_context_try_join(context, NULL) != RUMQTTC_OK) {
      /* A reusable host retains the context and retries from its loop. This
       * standalone program releases ownership and exits without unloading;
       * in-flight signal leases keep resources valid until they finish. */
      result = 1;
    }
    rumqttc_notification_context_release(context);
  }
  rumqttc_first_connection_release(connection);
  rumqttc_pending_admission_release(pending);
  rumqttc_completion_destroy(completion);
  example_destroy_client(&client);
  return result;
}
