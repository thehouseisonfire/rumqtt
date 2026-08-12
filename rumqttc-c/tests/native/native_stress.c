#include "native_common.h"

#include <stdio.h>
#include <stdlib.h>

static unsigned stress_iterations(void) {
  const char *value = getenv("RUMQTTC_C_STRESS_ITERATIONS");
  char *end = NULL;
  unsigned long parsed;
  if (value == NULL) {
    return 10;
  }
  parsed = strtoul(value, &end, 10);
  REQUIRE(end != value && *end == '\0' && parsed > 0 && parsed <= 100000);
  return (unsigned)parsed;
}

static void wait_for_thread_baseline(size_t baseline) {
  unsigned attempt;
  for (attempt = 0; attempt < 100; ++attempt) {
    if (native_process_thread_count() == baseline) {
      return;
    }
    native_sleep_ms(10);
  }
  REQUIRE(native_process_thread_count() == baseline);
}

int main(void) {
  const size_t baseline = native_process_thread_count();
  const unsigned iterations = stress_iterations();
  unsigned iteration;
  for (iteration = 0; iteration < iterations; ++iteration) {
    rumqttc_protocol_t protocol =
        iteration % 2 == 0 ? RUMQTTC_PROTOCOL_V311 : RUMQTTC_PROTOCOL_V5;
    rumqttc_client_t *client = native_start_client(
        protocol, "native-stress", RUMQTTC_ACK_AUTOMATIC, 8, 8, 250);
    rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_0);
    rumqttc_completion_t *completion = NULL;
    rumqttc_event_t *event;
    CHECK(rumqttc_client_publish_tracked(
        client, native_string("rumqttc/native/interrupt"),
        native_bytes(NULL, 0), &options, &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_QOS0_FLUSHED);
    rumqttc_completion_destroy(completion);
    event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
    rumqttc_event_destroy(event);
    event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    rumqttc_event_destroy(event);
    native_close_destroy(client);
    wait_for_thread_baseline(baseline);
  }
  printf("native C stress suite passed (%u iterations)\n", iterations);
  return 0;
}
