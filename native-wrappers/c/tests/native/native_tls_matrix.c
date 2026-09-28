#include "native_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum tls_scenario { POSITIVE, WRONG_ROOT, WRONG_HOST, MALFORMED_IDENTITY, WRONG_PASSWORD, PLATFORM_ROOTS };

static uint16_t fixture_port(const char *name) {
  const char *text = getenv(name);
  char *end = NULL;
  REQUIRE(text != NULL);
  unsigned long port = strtoul(text, &end, 10);
  REQUIRE(end != text && *end == '\0' && port > 0 && port <= UINT16_MAX);
  return (uint16_t)port;
}

static rumqttc_bytes_view_t copy_env(const char *name) {
  const char *text = getenv(name);
  REQUIRE(text != NULL);
  size_t length = strlen(text);
  uint8_t *copy = malloc(length);
  REQUIRE(copy != NULL);
  memcpy(copy, text, length);
  return native_bytes(copy, length);
}

static rumqttc_bytes_view_t read_archive(void) {
  const char *path = getenv("RUMQTTC_TEST_PKCS12_FILE");
  REQUIRE(path != NULL);
  FILE *file = fopen(path, "rb");
  REQUIRE(file != NULL && fseek(file, 0, SEEK_END) == 0);
  long length = ftell(file);
  REQUIRE(length > 0 && length < 1024 * 1024 && fseek(file, 0, SEEK_SET) == 0);
  uint8_t *bytes = malloc((size_t)length);
  REQUIRE(bytes != NULL && fread(bytes, 1, (size_t)length, file) == (size_t)length);
  REQUIRE(fclose(file) == 0);
  return native_bytes(bytes, (size_t)length);
}

static void release_bytes(rumqttc_bytes_view_t bytes) {
  if (bytes.len)
    memset((void *)bytes.data, 0, bytes.len);
  free((void *)bytes.data);
}

static void run_case(rumqttc_protocol_t protocol, uint32_t backend, int websocket, int mutual,
                     enum tls_scenario scenario) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  rumqttc_tls_pem_identity_t pem = RUMQTTC_TLS_PEM_IDENTITY_INIT;
  rumqttc_tls_pkcs12_identity_t pkcs12 = RUMQTTC_TLS_PKCS12_IDENTITY_INIT;
  rumqttc_bytes_view_t certificate = {NULL, 0}, key = {NULL, 0}, archive = {NULL, 0};
  uint8_t alpn_data[] = {'m', 'q', 't', 't'};
  rumqttc_bytes_view_t alpn = native_bytes(alpn_data, sizeof(alpn_data));
  rumqttc_bytes_view_t ca = copy_env(scenario == WRONG_ROOT ? "RUMQTTC_TEST_WRONG_CA_PEM" : "RUMQTTC_TEST_CA_PEM");
  char client_id[120], url[120];
  const char *host = scenario == WRONG_HOST ? "127.0.0.1" : "localhost";
  uint16_t port = fixture_port(mutual ? (websocket ? "RUMQTTC_TEST_MTLS_WSS_PORT" : "RUMQTTC_TEST_MTLS_PORT")
                                      : (websocket ? "RUMQTTC_TEST_WSS_PORT" : "RUMQTTC_TEST_TLS_PORT"));
  REQUIRE(snprintf(client_id, sizeof(client_id), "native-tls-matrix-%s-%u-%u-%d-%d-%d",
                   scenario == POSITIVE || scenario == PLATFORM_ROOTS ? "positive" : "failure", protocol, backend,
                   websocket, mutual, scenario) > 0);
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string(host), port, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  tls.backend = backend;
  tls.root_policy = scenario == PLATFORM_ROOTS ? RUMQTTC_TLS_ROOTS_PLATFORM : RUMQTTC_TLS_ROOTS_PEM;
  if (scenario != PLATFORM_ROOTS)
    tls.ca_pem = ca;
  tls.alpn_protocols = &alpn;
  tls.alpn_protocol_count = 1;
  if (mutual) {
    if (backend == RUMQTTC_TLS_BACKEND_RUSTLS) {
      certificate = copy_env("RUMQTTC_TEST_CLIENT_CERT_PEM");
      key = copy_env("RUMQTTC_TEST_CLIENT_KEY_PEM");
      pem.certificate = certificate;
      pem.private_key =
          scenario == MALFORMED_IDENTITY ? native_bytes((const uint8_t *)"private-invalid-identity", 24) : key;
      tls.pem_identity = &pem;
    } else {
      archive = read_archive();
      pkcs12.identity =
          scenario == MALFORMED_IDENTITY ? native_bytes((const uint8_t *)"private-invalid-identity", 24) : archive;
      pkcs12.password = native_bytes(
          (const uint8_t *)(scenario == WRONG_PASSWORD ? "private-wrong-password" : "private-pkcs12-password"),
          scenario == WRONG_PASSWORD ? 22 : 23);
      tls.pkcs12_identity = &pkcs12;
    }
  }
  if (websocket) {
    REQUIRE(snprintf(url, sizeof(url), "wss://%s:%u/mqtt", host, port) > 0);
    CHECK(rumqttc_config_set_transport_wss_with_options(config, native_string(url), &tls, NULL));
  } else {
    CHECK(rumqttc_config_set_transport_tls_with_options(config, &tls, NULL));
  }
  memset(alpn_data, 'x', sizeof(alpn_data));
  release_bytes(ca);
  release_bytes(certificate);
  release_bytes(key);
  release_bytes(archive);
  rumqttc_error_t *error = NULL;
  if (scenario == MALFORMED_IDENTITY || scenario == WRONG_PASSWORD) {
    client = (rumqttc_client_t *)(uintptr_t)1;
    REQUIRE(rumqttc_client_start(config, &client, &error) != RUMQTTC_OK);
    REQUIRE(client == NULL && error != NULL);
    rumqttc_error_kind_t kind = 0;
    CHECK(rumqttc_error_kind(error, &kind));
    REQUIRE(kind == RUMQTTC_ERROR_TLS);
    rumqttc_error_destroy(error);
  } else {
    CHECK(rumqttc_client_start(config, &client, NULL));
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    if (scenario == WRONG_ROOT || scenario == WRONG_HOST) {
      rumqttc_error_kind_t error_kind = 0;
      REQUIRE(kind == RUMQTTC_EVENT_DISCONNECTED);
      CHECK(rumqttc_event_disconnected(event, NULL, &error));
      CHECK(rumqttc_error_kind(error, &error_kind));
      REQUIRE(error_kind == RUMQTTC_ERROR_TLS || (websocket && error_kind == RUMQTTC_ERROR_NETWORK));
      rumqttc_error_destroy(error);
    } else {
      REQUIRE(kind == RUMQTTC_EVENT_CONNECTED);
    }
    rumqttc_event_destroy(event);
    native_close_destroy(client);
  }
  // Replacing the transport discards the previous TLS identity and settings.
  CHECK(rumqttc_config_set_transport_tcp(config, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-tls-matrix-cleared"), NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(event);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
}

static void check_disabled_backend(uint32_t backend) {
  rumqttc_config_t *config = NULL;
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  tls.backend = backend;
  REQUIRE(rumqttc_config_set_transport_tls_with_options(config, &tls, NULL) != RUMQTTC_OK);
  rumqttc_config_destroy(config);
}

int main(void) {
  const uint64_t capabilities = rumqttc_library_capabilities();
  for (uint32_t backend = RUMQTTC_TLS_BACKEND_RUSTLS; backend <= RUMQTTC_TLS_BACKEND_NATIVE; ++backend) {
    if (!(capabilities & (backend == RUMQTTC_TLS_BACKEND_RUSTLS ? RUMQTTC_CAP_RUSTLS : RUMQTTC_CAP_NATIVE_TLS))) {
      check_disabled_backend(backend);
      continue;
    }
    for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
      for (int websocket = 0; websocket <= !!(capabilities & RUMQTTC_CAP_WEBSOCKET); ++websocket) {
        run_case(protocol, backend, websocket, 0, POSITIVE);
        run_case(protocol, backend, websocket, 1, POSITIVE);
        run_case(protocol, backend, websocket, 0, WRONG_ROOT);
        run_case(protocol, backend, websocket, 0, WRONG_HOST);
      }
      run_case(protocol, backend, 0, 1, MALFORMED_IDENTITY);
      if (backend == RUMQTTC_TLS_BACKEND_NATIVE)
        run_case(protocol, backend, 0, 1, WRONG_PASSWORD);
#if defined(__linux__)
      run_case(protocol, backend, 0, 0, PLATFORM_ROOTS);
#endif
    }
  }
  return 0;
}
