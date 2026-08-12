#include "example_common.h"

#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>

static int demonstrate_shutdown(const char *host, uint16_t port,
                                const char *client_id, bool immediate) {
  rumqttc_client_t *client =
      example_connect(host, port, client_id, RUMQTTC_ACK_AUTOMATIC);
  rumqttc_error_t *error = NULL;
  rumqttc_status_t status;
  int failed;

  if (client == NULL)
    return 1;

  if (immediate) {
    status = rumqttc_client_close_now_timeout_ms(client, 5000, &error);
  } else {
    status = rumqttc_client_close_timeout_ms(client, 5000, &error);
  }
  failed = example_report(status, &error,
                          immediate ? "immediate close" : "graceful close");

  if (failed)
    (void)rumqttc_client_close_now_timeout_ms(client, 5000, NULL);
  example_destroy_client(&client);
  rumqttc_error_destroy(error);
  return failed;
}

int main(int argc, char **argv) {
  uint16_t port;

  if (argc != 3) {
    fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
    return 2;
  }

  port = (uint16_t)strtoul(argv[2], NULL, 10);
  if (demonstrate_shutdown(argv[1], port, "c-graceful", false) ||
      demonstrate_shutdown(argv[1], port, "c-immediate", true))
    return 1;

  puts("graceful and immediate shutdown completed");
  return 0;
}
