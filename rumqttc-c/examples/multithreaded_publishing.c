#include "example_common.h"

#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct producer_args {
  rumqttc_client_t *client;
  _Atomic int *stop;
  unsigned producer;
} producer_args_t;

static int producer(void *opaque) {
  producer_args_t *args = opaque;
  rumqttc_publish_options_t options = example_publish_options(RUMQTTC_QOS_1);
  unsigned message;
  for (message = 0; message < 10; ++message) {
    rumqttc_completion_t *completion = NULL;
    rumqttc_error_t *error = NULL;
    if (atomic_load_explicit(args->stop, memory_order_relaxed) != 0)
      return 0;
    rumqttc_status_t status = rumqttc_client_publish_tracked(
        args->client, example_string("rumqttc/native/threaded"),
        example_bytes(&args->producer, sizeof(args->producer)), &options,
        &completion, &error);
    if (example_report(status, &error, "threaded publish") ||
        example_wait(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED)) {
      rumqttc_completion_destroy(completion);
      rumqttc_error_destroy(error);
      return 1;
    }
    rumqttc_completion_destroy(completion);
  }
  return 0;
}

static int join_producers(example_thread_t **threads, unsigned started,
                          uint32_t timeout_ms, int *result) {
  unsigned index;
  for (index = 0; index < started; ++index) {
    int producer_result = 0;
    if (threads[index] == NULL)
      continue;
    if (example_thread_join(threads[index], timeout_ms, &producer_result) != 0)
      return -1;
    threads[index] = NULL;
    if (producer_result != 0)
      *result = 1;
  }
  return 0;
}

int main(int argc, char **argv) {
  enum { PRODUCERS = 4 };
  rumqttc_client_t *client = NULL;
  example_thread_t *threads[PRODUCERS] = {NULL};
  producer_args_t arguments[PRODUCERS];
  _Atomic int stop = 0;
  unsigned started = 0, index;
  int result = 1;
  if (argc != 3)
    return 2;
  client = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10),
                           "c-multithread", RUMQTTC_ACK_AUTOMATIC);
  if (client == NULL)
    goto cleanup;
  for (index = 0; index < PRODUCERS; ++index) {
    arguments[index].client = client;
    arguments[index].stop = &stop;
    arguments[index].producer = index;
    threads[index] = example_thread_start(producer, &arguments[index]);
    if (threads[index] == NULL)
      goto cleanup;
    ++started;
  }
  result = 0;
cleanup:
  if (join_producers(threads, started, 60000, &result) != 0) {
    result = 1;
    atomic_store_explicit(&stop, 1, memory_order_relaxed);
    if (client != NULL)
      (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
    if (join_producers(threads, started, 5000, &result) != 0) {
      /* A live producer may still reference arguments and client. Do not run
       * normal cleanup or return through main while either can be reclaimed. */
      fputs("a producer thread did not stop; skipping unsafe cleanup\n",
            stderr);
      (void)fflush(NULL);
      _Exit(EXIT_FAILURE);
    }
  }
  /* The shared client remains alive until every producer has joined. */
  if (client != NULL)
    (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
  example_destroy_client(&client);
  if (result == 0)
    puts("all native producer threads completed");
  return result;
}
