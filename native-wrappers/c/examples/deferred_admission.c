#include "example_common.h"

#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>

struct producer {
  rumqttc_client_t *client;
  atomic_int finished;
};

static int produce(void *data) {
  struct producer *producer = data;
  rumqttc_publish_options_t options = example_publish_options(RUMQTTC_QOS_1);
  int failed = 0;
  /* One pending request per producer, in addition to finite library limits. */
  for (unsigned i = 0; i < 16 && !failed; ++i) {
    rumqttc_pending_admission_t *pending = NULL;
    rumqttc_completion_t *completion = NULL;
    rumqttc_error_t *error = NULL;
    uint32_t state = RUMQTTC_ADMISSION_WAITING;
    failed = example_report(rumqttc_client_publish_deferred(
        producer->client, example_string("rumqttc/native/deferred-example"),
        example_bytes("hello", 5), &options, &pending, &error), &error, "create admission");
    if (!failed)
      failed = example_report(rumqttc_pending_admission_wait_timeout_ms(
          pending, 5000, &state, NULL, &completion, &error), &error, "observe admission");
    if (!failed && state == RUMQTTC_ADMISSION_ADMITTED)
      failed = example_wait(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    else {
      failed = 1;
      /* Requesting cancellation is not proof of non-admission. Release lets the
       * owner finish arbitration; this example does not retry the submission. */
      if (pending != NULL) (void)rumqttc_pending_admission_cancel(pending, NULL);
    }
    rumqttc_completion_destroy(completion);
    rumqttc_pending_admission_release(pending);
    rumqttc_error_destroy(error);
  }
  atomic_store(&producer->finished, 1);
  return failed;
}

int main(int argc, char **argv) {
  struct producer producer;
  example_thread_t *thread;
  int result = 0;
  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }
  producer.client = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10),
                                    "c-deferred-admission", RUMQTTC_ACK_AUTOMATIC);
  if (producer.client == NULL) return 1;
  atomic_init(&producer.finished, 0);
  thread = example_thread_start(produce, &producer);
  if (thread == NULL) {
    example_destroy_client(&producer.client);
    return 1;
  }
  /* MQTT execution and this event consumer keep progressing while the producer
   * waits first for admission and then for protocol completion. */
  while (!atomic_load(&producer.finished)) {
    rumqttc_event_t *event = NULL;
    rumqttc_error_t *error = NULL;
    rumqttc_status_t status = rumqttc_client_event_recv_timeout_ms(producer.client, 50, &event, &error);
    rumqttc_event_destroy(event);
    if (status != RUMQTTC_OK && status != RUMQTTC_TIMEOUT) {
      (void)example_report(status, &error, "receive event");
      (void)rumqttc_client_close_now_timeout_ms(producer.client, 5000, NULL);
      result = 1;
      break;
    }
    rumqttc_error_destroy(error);
  }
  {
    int producer_result = 0;
    if (example_thread_join(thread, 5000, &producer_result) != 0) {
      fputs("producer did not stop; client and arguments remain in use\n", stderr);
      _Exit(EXIT_FAILURE);
    }
    if (producer_result != 0) result = 1;
  }
  (void)rumqttc_client_close_now_timeout_ms(producer.client, 5000, NULL);
  example_destroy_client(&producer.client);
  return result;
}
