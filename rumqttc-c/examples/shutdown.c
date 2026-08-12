#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  rumqttc_client_t *graceful = NULL;
  rumqttc_client_t *immediate = NULL;
  rumqttc_error_t *error = NULL;
  int result = 1;
  if (argc != 3)
    return 2;
  graceful = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10),
                             "c-graceful", RUMQTTC_ACK_AUTOMATIC);
  if (graceful == NULL)
    goto cleanup;
  if (example_report(rumqttc_client_close_timeout_ms(graceful, 5000, &error),
                     &error, "graceful close"))
    goto cleanup;
  example_destroy_client(&graceful);
  graceful = NULL;
  immediate = example_connect(argv[1], (uint16_t)strtoul(argv[2], NULL, 10),
                              "c-immediate", RUMQTTC_ACK_AUTOMATIC);
  if (immediate == NULL)
    goto cleanup;
  if (example_report(
          rumqttc_client_close_now_timeout_ms(immediate, 5000, &error), &error,
          "immediate close"))
    goto cleanup;
  puts("graceful and immediate shutdown completed");
  result = 0;
cleanup:
  if (graceful != NULL)
    (void)rumqttc_client_close_now_timeout_ms(graceful, 5000, NULL);
  if (immediate != NULL)
    (void)rumqttc_client_close_now_timeout_ms(immediate, 5000, NULL);
  example_destroy_client(&graceful);
  example_destroy_client(&immediate);
  rumqttc_error_destroy(error);
  return result;
}
