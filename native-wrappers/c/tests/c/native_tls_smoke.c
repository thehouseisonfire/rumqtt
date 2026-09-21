#include "rumqttc.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int check_transport(int websocket) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_event_t *event = NULL;
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  rumqttc_string_view_t host = {"localhost", 9};
  rumqttc_string_view_t client_id = {"native-c-smoke", 14};
  rumqttc_string_view_t url;
  const char *ca = getenv("RUMQTTC_TEST_CA_PEM");
  const char *port_text = getenv(websocket ? "RUMQTTC_TEST_WSS_PORT"
                                           : "RUMQTTC_TEST_TLS_PORT");
  char url_buffer[128];
  unsigned long port = 9;
  int url_length;
  int result = 1;

  if (ca != NULL && port_text != NULL) {
    char *end = NULL;
    port = strtoul(port_text, &end, 10);
    if (*port_text == '\0' || *end != '\0' || port == 0 || port > 65535)
      return 1;
    tls.root_policy = RUMQTTC_TLS_ROOTS_PEM;
    tls.ca_pem.data = (const uint8_t *)ca;
    tls.ca_pem.len = strlen(ca);
  } else if (ca != NULL || port_text != NULL) {
    return 1;
  }
  url_length = snprintf(url_buffer, sizeof(url_buffer), "wss://localhost:%lu/",
                        port);
  if (url_length < 0 || (size_t)url_length >= sizeof(url_buffer))
    return 1;
  url.data = url_buffer;
  url.len = (size_t)url_length;
  tls.backend = RUMQTTC_TLS_BACKEND_NATIVE;
  if (rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL) != RUMQTTC_OK ||
      rumqttc_config_set_client_id(config, client_id, NULL) != RUMQTTC_OK)
    goto cleanup;
  if (websocket) {
    if (rumqttc_config_set_transport_wss_with_options(config, url, &tls,
                                                      NULL) != RUMQTTC_OK)
      goto cleanup;
  } else {
    if (rumqttc_config_set_broker(config, host, (uint16_t)port, NULL) !=
            RUMQTTC_OK ||
        rumqttc_config_set_transport_tls_with_options(config, &tls, NULL) !=
            RUMQTTC_OK)
      goto cleanup;
  }
  if (rumqttc_client_start(config, &client, NULL) != RUMQTTC_OK)
    goto cleanup;
  if (ca != NULL) {
    rumqttc_event_kind_t kind = 0;
    if (rumqttc_client_event_recv_timeout_ms(client, 5000, &event, NULL) !=
            RUMQTTC_OK ||
        rumqttc_event_kind(event, &kind) != RUMQTTC_OK ||
        kind != RUMQTTC_EVENT_CONNECTED)
      goto cleanup;
  }
  if (rumqttc_client_close_now_timeout_ms(client, 5000, NULL) != RUMQTTC_OK)
    goto cleanup;
  result = 0;

cleanup:
  rumqttc_event_destroy(event);
  rumqttc_config_destroy(config);
  if (client != NULL &&
      rumqttc_client_destroy_timeout_ms(client, 5000, NULL) != RUMQTTC_OK)
    return 1;
  return result;
}

int main(void) {
  uint64_t capabilities = rumqttc_library_capabilities();
  if ((capabilities & RUMQTTC_CAP_NATIVE_TLS) == 0 ||
      (capabilities & RUMQTTC_CAP_RUSTLS) != 0 ||
      (capabilities & RUMQTTC_CAP_WEBSOCKET) == 0)
    return 1;
  return check_transport(0) || check_transport(1);
}
