#include "native_common.h"

#include <stdlib.h>
#include <string.h>

static void exercise_proxy(rumqttc_protocol_t version, uint32_t proxy_kind,
                           const char *client_id) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_proxy_options_t proxy = RUMQTTC_PROXY_OPTIONS_INIT;
  rumqttc_tls_options_t proxy_tls = RUMQTTC_TLS_OPTIONS_INIT;
  rumqttc_event_t *connected;
  const uint8_t username[] = {'u', 's', 'e', 'r'};
  const uint8_t password[] = {'p', 'a', 's', 's'};
  CHECK(rumqttc_config_new(version, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("broker.invalid"),
                                  1883, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  proxy.protocol = proxy_kind;
  proxy.dns_policy = RUMQTTC_PROXY_DNS_REMOTE;
  if (proxy_kind == RUMQTTC_PROXY_HTTPS) {
    const char *ca = getenv("RUMQTTC_TEST_CA_PEM");
    const char *port_text = getenv("RUMQTTC_TEST_HTTPS_PROXY_PORT");
    char *end = NULL;
    unsigned long port;
    REQUIRE(ca != NULL && port_text != NULL);
    port = strtoul(port_text, &end, 10);
    REQUIRE(*port_text != '\0' && *end == '\0' && port > 0 && port <= 65535);
    proxy.host = native_string("localhost");
    proxy.port = (uint32_t)port;
    proxy_tls.backend = (rumqttc_library_capabilities() & RUMQTTC_CAP_RUSTLS)
                            ? RUMQTTC_TLS_BACKEND_RUSTLS
                            : RUMQTTC_TLS_BACKEND_NATIVE;
    proxy_tls.root_policy = RUMQTTC_TLS_ROOTS_PEM;
    proxy_tls.ca_pem = native_bytes((const uint8_t *)ca, strlen(ca));
    proxy.tls = &proxy_tls;
  } else {
    proxy.host = native_string("127.0.0.1");
    proxy.port = native_test_port();
  }
  proxy.username = native_bytes(username, sizeof(username));
  proxy.password = native_bytes(password, sizeof(password));
  proxy.credentials_present = 1;
  CHECK(rumqttc_config_set_proxy(config, &proxy, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  connected = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(connected);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

int main(void) {
  const uint64_t capabilities = rumqttc_library_capabilities();
  if (capabilities & RUMQTTC_CAP_HTTP_PROXY) {
    exercise_proxy(RUMQTTC_PROTOCOL_V4, RUMQTTC_PROXY_HTTP,
                   "native-proxy-http-v4");
    exercise_proxy(RUMQTTC_PROTOCOL_V5, RUMQTTC_PROXY_HTTP,
                   "native-proxy-http-v5");
    exercise_proxy(RUMQTTC_PROTOCOL_V4, RUMQTTC_PROXY_HTTP,
                   "native-proxy-http-reconnect-v4");
    exercise_proxy(RUMQTTC_PROTOCOL_V5, RUMQTTC_PROXY_HTTP,
                   "native-proxy-http-reconnect-v5");
    if (capabilities & (RUMQTTC_CAP_RUSTLS | RUMQTTC_CAP_NATIVE_TLS)) {
      exercise_proxy(RUMQTTC_PROTOCOL_V4, RUMQTTC_PROXY_HTTPS,
                     "native-proxy-https-v4");
      exercise_proxy(RUMQTTC_PROTOCOL_V5, RUMQTTC_PROXY_HTTPS,
                     "native-proxy-https-v5");
    }
  }
  if (capabilities & RUMQTTC_CAP_SOCKS5_PROXY) {
    exercise_proxy(RUMQTTC_PROTOCOL_V4, RUMQTTC_PROXY_SOCKS5,
                   "native-proxy-socks5-v4");
    exercise_proxy(RUMQTTC_PROTOCOL_V5, RUMQTTC_PROXY_SOCKS5,
                   "native-proxy-socks5-v5");
    exercise_proxy(RUMQTTC_PROTOCOL_V4, RUMQTTC_PROXY_SOCKS5,
                   "native-proxy-socks5-reconnect-v4");
    exercise_proxy(RUMQTTC_PROTOCOL_V5, RUMQTTC_PROXY_SOCKS5,
                   "native-proxy-socks5-reconnect-v5");
  }
  return 0;
}
