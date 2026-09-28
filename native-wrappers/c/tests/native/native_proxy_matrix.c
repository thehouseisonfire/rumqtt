#include "native_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum scenario { CONNECT, WRONG_PROXY_ROOT, WRONG_BROKER_ROOT, TIMEOUT, RECOVER };

static uint16_t port_from_env(const char *name) {
  const char *text = getenv(name);
  char *end = NULL;
  REQUIRE(text != NULL);
  unsigned long port = strtoul(text, &end, 10);
  REQUIRE(end != text && *end == '\0' && port > 0 && port <= UINT16_MAX);
  return (uint16_t)port;
}

static rumqttc_tls_options_t tls_options(const char *roots, uint32_t backend) {
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  REQUIRE(roots != NULL);
  tls.backend = backend;
  tls.root_policy = RUMQTTC_TLS_ROOTS_PEM;
  tls.ca_pem = native_bytes((const uint8_t *)roots, strlen(roots));
  return tls;
}

static void run_case(rumqttc_protocol_t protocol, uint32_t proxy_kind, uint32_t backend, int websocket,
                     enum scenario scenario) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_completion_t *completion = NULL;
  rumqttc_publish_options_t publish = native_publish_options(RUMQTTC_QOS_0);
  rumqttc_proxy_options_t proxy = RUMQTTC_PROXY_OPTIONS_INIT;
  const char *broker_ca = getenv("RUMQTTC_TEST_CA_PEM");
  const char *proxy_ca = getenv("RUMQTTC_TEST_PROXY_CA_PEM");
  rumqttc_tls_options_t broker_tls = tls_options(scenario == WRONG_BROKER_ROOT ? proxy_ca : broker_ca, backend);
  rumqttc_tls_options_t proxy_tls = tls_options(scenario == WRONG_PROXY_ROOT ? broker_ca : proxy_ca, backend);
  uint16_t broker_port = port_from_env(websocket ? "RUMQTTC_TEST_WSS_PORT" : "RUMQTTC_TEST_TLS_PORT");
  char client_id[100], url[100];
  unsigned disconnected = 0, connected = 0;
  REQUIRE(snprintf(client_id, sizeof(client_id), "native-tunnel-%u-%u-%u-%d-%d", protocol, proxy_kind, backend,
                   websocket, scenario) > 0);
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("localhost"), broker_port, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  CHECK(rumqttc_config_set_connection_timeout_seconds(config, scenario == TIMEOUT ? 1 : 5, NULL));
  if (websocket) {
    REQUIRE(snprintf(url, sizeof(url), "wss://localhost:%u/mqtt", broker_port) > 0);
    CHECK(rumqttc_config_set_transport_wss_with_options(config, native_string(url), &broker_tls, NULL));
  } else {
    CHECK(rumqttc_config_set_transport_tls_with_options(config, &broker_tls, NULL));
  }
  proxy.protocol = proxy_kind;
  proxy.dns_policy = RUMQTTC_PROXY_DNS_REMOTE;
  proxy.host = native_string("localhost");
  proxy.port = port_from_env(scenario == TIMEOUT                 ? "RUMQTTC_TEST_TIMEOUT_TUNNEL_PORT"
                             : scenario == RECOVER               ? "RUMQTTC_TEST_RECOVER_TUNNEL_PORT"
                             : proxy_kind == RUMQTTC_PROXY_HTTPS ? "RUMQTTC_TEST_TLS_TUNNEL_PORT"
                                                                 : "RUMQTTC_TEST_TUNNEL_PORT");
  proxy.credentials_present = 1;
  proxy.username = native_bytes((const uint8_t *)"proxy-private-user", strlen("proxy-private-user"));
  proxy.password = native_bytes((const uint8_t *)"proxy-private-password", strlen("proxy-private-password"));
  if (proxy_kind == RUMQTTC_PROXY_HTTPS)
    proxy.tls = &proxy_tls;
  CHECK(rumqttc_config_set_proxy(config, &proxy, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  CHECK(rumqttc_client_publish_tracked(client, native_string("native/tunnel"), native_bytes(NULL, 0), &publish,
                                       &completion, NULL));
  for (unsigned attempt = 0; attempt < 8; ++attempt) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    if (kind == RUMQTTC_EVENT_CONNECTED) {
      REQUIRE(scenario == CONNECT || scenario == RECOVER);
      connected = 1;
    } else if (kind == RUMQTTC_EVENT_DISCONNECTED) {
      rumqttc_error_t *error = NULL;
      rumqttc_error_kind_t error_kind = 0;
      CHECK(rumqttc_event_disconnected(event, NULL, &error));
      CHECK(rumqttc_error_kind(error, &error_kind));
      if (scenario == TIMEOUT)
        REQUIRE(error_kind == RUMQTTC_ERROR_TIMEOUT);
      else if (scenario == WRONG_PROXY_ROOT)
        REQUIRE(error_kind == RUMQTTC_ERROR_NETWORK);
      else if (scenario != RECOVER)
        REQUIRE(error_kind == RUMQTTC_ERROR_TLS);
      rumqttc_error_destroy(error);
      ++disconnected;
    } else {
      REQUIRE(0);
    }
    rumqttc_event_destroy(event);
    if (connected || (disconnected && scenario != RECOVER))
      break;
  }
  if (scenario == CONNECT || scenario == RECOVER)
    REQUIRE(connected);
  else
    REQUIRE(!connected && disconnected > 0);
  if (connected) {
    if (scenario == RECOVER)
      REQUIRE(disconnected > 0);
    native_wait_completion(completion, RUMQTTC_COMPLETION_QOS0_FLUSHED);
    rumqttc_completion_destroy(completion);
    publish.qos = RUMQTTC_QOS_1;
    CHECK(rumqttc_client_publish_tracked(client, native_string("native/tunnel"), native_bytes(NULL, 0), &publish,
                                         &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  }
  native_close_destroy(client);
  if (!connected) {
    rumqttc_error_t *error = NULL;
    REQUIRE(rumqttc_completion_wait_timeout_ms(completion, NATIVE_DEADLINE_MS, &error) != RUMQTTC_OK);
    REQUIRE(error != NULL);
    rumqttc_error_destroy(error);
  }
  rumqttc_completion_destroy(completion);
  rumqttc_config_destroy(config);
}

static void check_disabled_proxy(uint32_t kind) {
  rumqttc_config_t *config = NULL;
  rumqttc_proxy_options_t proxy = RUMQTTC_PROXY_OPTIONS_INIT;
  rumqttc_error_t *error = NULL;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  proxy.protocol = kind;
  proxy.host = native_string("localhost");
  proxy.port = native_test_port();
  REQUIRE(rumqttc_config_set_proxy(config, &proxy, &error) != RUMQTTC_OK);
  REQUIRE(error != NULL);
  rumqttc_error_destroy(error);
  rumqttc_config_destroy(config);
}

int main(void) {
  uint64_t capabilities = rumqttc_library_capabilities();
  const uint32_t backends[] = {RUMQTTC_TLS_BACKEND_RUSTLS, RUMQTTC_TLS_BACKEND_NATIVE};
  if (!(capabilities & RUMQTTC_CAP_HTTP_PROXY))
    check_disabled_proxy(RUMQTTC_PROXY_HTTP);
  if (!(capabilities & RUMQTTC_CAP_SOCKS5_PROXY))
    check_disabled_proxy(RUMQTTC_PROXY_SOCKS5);
  check_disabled_proxy(UINT32_MAX);
  for (size_t backend_index = 0; backend_index < 2; ++backend_index) {
    uint32_t backend = backends[backend_index];
    if (!(capabilities & (backend_index == 0 ? RUMQTTC_CAP_RUSTLS : RUMQTTC_CAP_NATIVE_TLS)))
      continue;
    for (uint32_t kind = RUMQTTC_PROXY_HTTP; kind <= RUMQTTC_PROXY_SOCKS5; ++kind) {
      if (!(capabilities & (kind == RUMQTTC_PROXY_SOCKS5 ? RUMQTTC_CAP_SOCKS5_PROXY : RUMQTTC_CAP_HTTP_PROXY)))
        continue;
      for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
        run_case(protocol, kind, backend, 0, CONNECT);
        if (capabilities & RUMQTTC_CAP_WEBSOCKET)
          run_case(protocol, kind, backend, 1, CONNECT);
        run_case(protocol, kind, backend, 0, WRONG_BROKER_ROOT);
        if (kind == RUMQTTC_PROXY_HTTPS)
          run_case(protocol, kind, backend, 0, WRONG_PROXY_ROOT);
      }
    }
    const uint32_t failure_kinds[] = {RUMQTTC_PROXY_HTTP, RUMQTTC_PROXY_SOCKS5};
    for (size_t index = 0; index < 2; ++index) {
      uint32_t kind = failure_kinds[index];
      if (!(capabilities & (kind == RUMQTTC_PROXY_HTTP ? RUMQTTC_CAP_HTTP_PROXY : RUMQTTC_CAP_SOCKS5_PROXY)))
        continue;
      for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
        run_case(protocol, kind, backend, 0, RECOVER);
        run_case(protocol, kind, backend, 0, TIMEOUT);
      }
    }
  }
  return 0;
}
