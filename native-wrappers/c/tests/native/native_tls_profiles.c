#include "native_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { NO_PIN = -1, CERT_PIN = 0, SPKI_PIN = 1, WRONG_PIN = 2, BACKUP_PIN = 3 };
enum { DIRECT_TLS, DIRECT_WSS, HTTPS_PROXY, REDIRECT_TLS, REDIRECT_WSS };

static uint16_t port_env(const char *name) {
  const char *value = getenv(name);
  REQUIRE(value != NULL);
  unsigned long port = strtoul(value, NULL, 10);
  REQUIRE(port > 0 && port <= UINT16_MAX);
  return (uint16_t)port;
}

static void read_pin(rumqttc_tls_pin_t *pin, const char *name, uint32_t target) {
  const char *value = getenv(name);
  REQUIRE(value != NULL && strlen(value) == 64);
  pin->target = target;
  for (size_t i = 0; i < 32; ++i) {
    unsigned byte = 0;
    REQUIRE(sscanf(value + i * 2, "%2x", &byte) == 1);
    pin->sha256[i] = (uint8_t)byte;
  }
}

static rumqttc_tls_profile_t *profile(uint32_t backend, uint32_t version, uint32_t roots,
                                     const char *ca_name, int pin_mode, int proxy) {
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  rumqttc_tls_profile_options_t options = RUMQTTC_TLS_PROFILE_OPTIONS_INIT;
  rumqttc_tls_pin_t pins[2] = {RUMQTTC_TLS_PIN_INIT, RUMQTTC_TLS_PIN_INIT};
  rumqttc_tls_profile_t *result = NULL;
  tls.backend = backend;
  tls.root_policy = roots;
  char *ca = NULL;
  if (roots != RUMQTTC_TLS_ROOTS_PLATFORM) {
    const char *source = getenv(ca_name);
    REQUIRE(source != NULL);
    ca = malloc(strlen(source) + 1);
    REQUIRE(ca != NULL);
    memcpy(ca, source, strlen(source) + 1);
    tls.ca_pem = native_bytes((const uint8_t *)ca, strlen(ca));
  }
  options.tls = &tls;
  options.version_policy = version;
  if (pin_mode != NO_PIN) {
    read_pin(&pins[0], proxy ? "RUMQTTC_TEST_PROXY_CERT_PIN" : "RUMQTTC_TEST_CERT_PIN",
             RUMQTTC_TLS_PIN_LEAF_CERTIFICATE);
    if (pin_mode == SPKI_PIN)
      read_pin(&pins[0], proxy ? "RUMQTTC_TEST_PROXY_SPKI_PIN" : "RUMQTTC_TEST_SPKI_PIN",
               RUMQTTC_TLS_PIN_LEAF_SPKI);
    options.pins = pins;
    options.pin_count = 1;
    if (pin_mode == WRONG_PIN || pin_mode == BACKUP_PIN)
      memset(pins[0].sha256, 0, sizeof(pins[0].sha256));
    if (pin_mode == BACKUP_PIN) {
      read_pin(&pins[1], proxy ? "RUMQTTC_TEST_PROXY_SPKI_PIN" : "RUMQTTC_TEST_SPKI_PIN",
               RUMQTTC_TLS_PIN_LEAF_SPKI);
      options.pin_count = 2;
    }
  }
  CHECK(rumqttc_tls_profile_new(&options, &result, NULL));
  REQUIRE(result != NULL);
  // Caller buffers and records are no longer needed after successful creation.
  if (ca != NULL) {
    memset(ca, 'x', strlen(ca));
    free(ca);
  }
  memset(pins, 0xff, sizeof(pins));
  return result;
}

static void run_case(uint32_t protocol, uint32_t backend, uint32_t version, uint32_t roots,
                     const char *ca_name, int pin_mode, int transport, const char *port_name,
                     int wrong_name, int succeeds) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_tls_profile_t *tls = profile(backend, version, roots, ca_name, pin_mode, 0);
  const char *host = wrong_name ? "127.0.0.1" : "localhost";
  uint16_t port = port_env(port_name);
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string(host), port, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-tls-profile"), NULL));
  if (transport == DIRECT_WSS) {
    char url[128];
    REQUIRE(snprintf(url, sizeof(url), "wss://%s:%u/mqtt", host, port) > 0);
    CHECK(rumqttc_config_set_transport_wss_with_profile(config, native_string(url), tls, NULL));
  } else if (transport == REDIRECT_TLS || transport == REDIRECT_WSS) {
    const char *id = transport == REDIRECT_TLS ? "native-redirect-matrix-connack-mqtts"
                                              : "native-redirect-matrix-connack-wss";
    CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
    CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
    CHECK(rumqttc_config_set_v5_redirect_policy_with_tls_profile(
        config, 2, transport == REDIRECT_TLS ? RUMQTTC_REDIRECT_TRANSPORT_TLS : RUMQTTC_REDIRECT_TRANSPORT_WSS,
        tls, NULL));
  } else {
    CHECK(rumqttc_config_set_transport_tls_with_profile(config, tls, NULL));
  }
  if (transport == HTTPS_PROXY) {
    rumqttc_tls_profile_t *proxy_tls = profile(backend, version, RUMQTTC_TLS_ROOTS_PEM,
                                              "RUMQTTC_TEST_PROXY_CA_PEM", pin_mode, 1);
    rumqttc_proxy_options_t proxy = RUMQTTC_PROXY_OPTIONS_INIT;
    proxy.protocol = RUMQTTC_PROXY_HTTPS;
    proxy.host = native_string("localhost");
    proxy.port = port_env("RUMQTTC_TEST_TLS_TUNNEL_PORT");
    proxy.credentials_present = 1;
    proxy.username = native_bytes((const uint8_t *)"proxy-private-user", 18);
    proxy.password = native_bytes((const uint8_t *)"proxy-private-password", 22);
    CHECK(rumqttc_config_set_proxy_with_tls_profile(config, &proxy, proxy_tls, NULL));
    rumqttc_tls_profile_destroy(proxy_tls);
  }
  rumqttc_tls_profile_destroy(tls);
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_config_destroy(config);
  rumqttc_event_t *event = native_wait_event(client, succeeds ? RUMQTTC_EVENT_CONNECTED : RUMQTTC_EVENT_DISCONNECTED);
  if (!succeeds) {
    rumqttc_error_t *error = NULL;
    rumqttc_error_kind_t kind = 0;
    CHECK(rumqttc_event_disconnected(event, NULL, &error));
    CHECK(rumqttc_error_kind(error, &kind));
    REQUIRE(kind == RUMQTTC_ERROR_TLS || kind == RUMQTTC_ERROR_NETWORK);
    rumqttc_error_destroy(error);
  }
  rumqttc_event_destroy(event);
  native_close_destroy(client);
}

static void check_invalid(uint32_t backend, const rumqttc_tls_backend_capabilities_t *caps) {
  rumqttc_tls_profile_options_t options = RUMQTTC_TLS_PROFILE_OPTIONS_INIT;
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  rumqttc_tls_profile_t *out = (rumqttc_tls_profile_t *)(uintptr_t)1;
  tls.backend = backend;
  options.tls = &tls;
  REQUIRE(rumqttc_tls_profile_new(NULL, &out, NULL) != RUMQTTC_OK && out == NULL);
  REQUIRE(rumqttc_tls_profile_new(&options, NULL, NULL) != RUMQTTC_OK);
  options.struct_size = 0;
  REQUIRE(rumqttc_tls_profile_new(&options, &out, NULL) != RUMQTTC_OK && out == NULL);
  options.struct_size = sizeof(options);
  options.version_policy = UINT32_MAX;
  REQUIRE(rumqttc_tls_profile_new(&options, &out, NULL) != RUMQTTC_OK && out == NULL);
  options.version_policy = RUMQTTC_TLS_VERSION_DEFAULT;
  options.pin_count = RUMQTTC_TLS_MAX_PINS + 1;
  REQUIRE(rumqttc_tls_profile_new(&options, &out, NULL) != RUMQTTC_OK && out == NULL);
  options.pin_count = 1;
  REQUIRE(rumqttc_tls_profile_new(&options, &out, NULL) != RUMQTTC_OK && out == NULL);
  options.pin_count = 0;
  options.reserved[0] = 1;
  REQUIRE(rumqttc_tls_profile_new(&options, &out, NULL) != RUMQTTC_OK && out == NULL);
  options.reserved[0] = 0;
  rumqttc_tls_pin_t invalid_pin = RUMQTTC_TLS_PIN_INIT;
  options.pins = &invalid_pin;
  options.pin_count = 1;
  invalid_pin.target = UINT32_MAX;
  REQUIRE(rumqttc_tls_profile_new(&options, &out, NULL) != RUMQTTC_OK && out == NULL);
  invalid_pin.target = RUMQTTC_TLS_PIN_LEAF_CERTIFICATE;
  invalid_pin.reserved[0] = 1;
  REQUIRE(rumqttc_tls_profile_new(&options, &out, NULL) != RUMQTTC_OK && out == NULL);
  invalid_pin.reserved[0] = 0;
  invalid_pin.struct_size = 0;
  REQUIRE(rumqttc_tls_profile_new(&options, &out, NULL) != RUMQTTC_OK && out == NULL);
  options.pin_count = 0;
  if (caps->pin_target_mask == 0) {
    rumqttc_tls_pin_t pin = RUMQTTC_TLS_PIN_INIT;
    options.pins = &pin;
    options.pin_count = 1;
    REQUIRE(rumqttc_tls_profile_new(&options, &out, NULL) != RUMQTTC_OK && out == NULL);
    options.pin_count = 0;
  }
  for (uint32_t version = 0; version <= RUMQTTC_TLS_VERSION_12_OR_13; ++version) {
    if (caps->version_policy_mask & (1u << version)) continue;
    options.version_policy = version;
    REQUIRE(rumqttc_tls_profile_new(&options, &out, NULL) != RUMQTTC_OK && out == NULL);
  }
  rumqttc_tls_profile_destroy(NULL);
}

static void check_pinned_reconnect(uint32_t protocol, int mode) {
  const char *name = mode == CERT_PIN ? "cert" : mode == SPKI_PIN ? "spki" : "key";
  char port_name[80], barrier[80];
  REQUIRE(snprintf(port_name, sizeof(port_name), "RUMQTTC_TEST_RECONNECT_%s_%s_PORT",
                   protocol == RUMQTTC_PROTOCOL_V4 ? "V4" : "V5",
                   mode == CERT_PIN ? "CERT" : mode == SPKI_PIN ? "SPKI" : "KEY") > 0);
  REQUIRE(snprintf(barrier, sizeof(barrier), "profile-reconnect-%s-%s-release",
                   protocol == RUMQTTC_PROTOCOL_V4 ? "v4" : "v5", name) > 0);
  rumqttc_tls_profile_t *tls = profile(RUMQTTC_TLS_BACKEND_RUSTLS, RUMQTTC_TLS_VERSION_13_ONLY,
                                      RUMQTTC_TLS_ROOTS_PEM, "RUMQTTC_TEST_CA_PEM",
                                      mode == CERT_PIN ? CERT_PIN : SPKI_PIN, 0);
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("localhost"), port_env(port_name), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("profile-pinned-reconnect"), NULL));
  CHECK(rumqttc_config_set_transport_tls_with_profile(config, tls, NULL));
  rumqttc_tls_profile_destroy(tls);
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_config_destroy(config);
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
  rumqttc_event_destroy(event);
  native_fixture_write(barrier, 1);
  event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
  rumqttc_event_destroy(event);
  event = native_wait_event(client, mode == SPKI_PIN ? RUMQTTC_EVENT_CONNECTED : RUMQTTC_EVENT_DISCONNECTED);
  if (mode != SPKI_PIN) {
    rumqttc_error_t *error = NULL;
    rumqttc_error_kind_t kind = 0;
    CHECK(rumqttc_event_disconnected(event, NULL, &error));
    CHECK(rumqttc_error_kind(error, &kind));
    REQUIRE(kind == RUMQTTC_ERROR_TLS);
    rumqttc_error_destroy(error);
  }
  rumqttc_event_destroy(event);
  native_close_destroy(client);
}

static void check_transactional_setters_and_reuse(uint32_t backend) {
  rumqttc_tls_profile_t *tls = profile(backend, 0, RUMQTTC_TLS_ROOTS_PEM,
                                      "RUMQTTC_TEST_CA_PEM", NO_PIN, 0);
  rumqttc_config_t *configs[2] = {NULL, NULL};
  for (size_t i = 0; i < 2; ++i) {
    CHECK(rumqttc_config_new(i == 0 ? RUMQTTC_PROTOCOL_V4 : RUMQTTC_PROTOCOL_V5, &configs[i], NULL));
    CHECK(rumqttc_config_set_broker(configs[i], native_string("localhost"), port_env("RUMQTTC_TEST_TLS_PORT"), NULL));
    CHECK(rumqttc_config_set_client_id(configs[i], native_string(i == 0 ? "profile-reuse-v4" : "profile-reuse-v5"), NULL));
    CHECK(rumqttc_config_set_transport_tls_with_profile(configs[i], tls, NULL));
    REQUIRE(rumqttc_config_set_transport_tls_with_profile(configs[i], NULL, NULL) != RUMQTTC_OK);
    REQUIRE(rumqttc_config_set_transport_wss_with_profile(configs[i], native_string("wss://localhost/mqtt"), NULL, NULL) != RUMQTTC_OK);
    REQUIRE(rumqttc_config_set_v5_redirect_policy_with_tls_profile(configs[i], 0, RUMQTTC_REDIRECT_TRANSPORT_TLS, tls, NULL) != RUMQTTC_OK);
    REQUIRE(rumqttc_config_set_v5_redirect_policy_with_tls_profile(configs[i], 2, RUMQTTC_REDIRECT_TRANSPORT_TCP, tls, NULL) != RUMQTTC_OK);
    rumqttc_proxy_options_t proxy = RUMQTTC_PROXY_OPTIONS_INIT;
    proxy.protocol = RUMQTTC_PROXY_HTTP;
    proxy.host = native_string("localhost");
    proxy.port = 1;
    REQUIRE(rumqttc_config_set_proxy_with_tls_profile(configs[i], &proxy, tls, NULL) != RUMQTTC_OK);
    rumqttc_tls_options_t legacy = RUMQTTC_TLS_OPTIONS_INIT;
    proxy.protocol = RUMQTTC_PROXY_HTTPS;
    proxy.tls = &legacy;
    REQUIRE(rumqttc_config_set_proxy_with_tls_profile(configs[i], &proxy, tls, NULL) != RUMQTTC_OK);
  }
  rumqttc_tls_profile_destroy(tls);
  for (size_t i = 0; i < 2; ++i) {
    rumqttc_client_t *client = NULL;
    CHECK(rumqttc_client_start(configs[i], &client, NULL));
    rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    rumqttc_event_destroy(event);
    native_close_destroy(client);
    CHECK(rumqttc_config_set_transport_tcp(configs[i], NULL));
    CHECK(rumqttc_config_set_broker(configs[i], native_string("127.0.0.1"), native_test_port(), NULL));
    CHECK(rumqttc_client_start(configs[i], &client, NULL));
    event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    rumqttc_event_destroy(event);
    native_close_destroy(client);
    rumqttc_config_destroy(configs[i]);
  }
}

int main(void) {
  for (uint32_t backend = 0; backend <= 1; ++backend) {
    rumqttc_tls_backend_capabilities_t caps = RUMQTTC_TLS_BACKEND_CAPABILITIES_INIT;
    CHECK(rumqttc_tls_backend_capabilities(backend, &caps, NULL));
    check_invalid(backend, &caps);
    if (caps.version_policy_mask == 0) continue;
    REQUIRE(caps.root_policy_mask == 7);
    check_transactional_setters_and_reuse(backend);
    for (uint32_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
      for (uint32_t version = 1; version <= 3; ++version) {
        if (!(caps.version_policy_mask & (1u << version))) continue;
        int allows12 = version != RUMQTTC_TLS_VERSION_13_ONLY && (caps.version_policy_mask & (1u << RUMQTTC_TLS_VERSION_12_ONLY));
        int allows13 = version != RUMQTTC_TLS_VERSION_12_ONLY && (caps.version_policy_mask & (1u << RUMQTTC_TLS_VERSION_13_ONLY));
        run_case(protocol, backend, version, RUMQTTC_TLS_ROOTS_PEM, "RUMQTTC_TEST_CA_PEM", NO_PIN,
                 DIRECT_TLS, "RUMQTTC_TEST_TLS_12_PORT", 0, allows12);
        run_case(protocol, backend, version, RUMQTTC_TLS_ROOTS_PEM, "RUMQTTC_TEST_CA_PEM", NO_PIN,
                 DIRECT_TLS, "RUMQTTC_TEST_TLS_13_PORT", 0, allows13);
      }
      if (getenv("RUMQTTC_TEST_PLATFORM_TRUST") != NULL) {
        run_case(protocol, backend, 0, RUMQTTC_TLS_ROOTS_PLATFORM_AND_PEM, "RUMQTTC_TEST_UNTRUSTED_CA_PEM", NO_PIN,
                 DIRECT_TLS, "RUMQTTC_TEST_TLS_PORT", 0, 1);
        run_case(protocol, backend, 0, RUMQTTC_TLS_ROOTS_PLATFORM_AND_PEM, "RUMQTTC_TEST_UNTRUSTED_CA_PEM", NO_PIN,
                 DIRECT_TLS, "RUMQTTC_TEST_UNTRUSTED_TLS_PORT", 0, 1);
        run_case(protocol, backend, 0, RUMQTTC_TLS_ROOTS_PEM, "RUMQTTC_TEST_UNTRUSTED_CA_PEM", NO_PIN,
                 DIRECT_TLS, "RUMQTTC_TEST_TLS_PORT", 0, 0);
        run_case(protocol, backend, 0, RUMQTTC_TLS_ROOTS_PLATFORM, NULL, NO_PIN,
                 DIRECT_TLS, "RUMQTTC_TEST_UNTRUSTED_TLS_PORT", 0, 0);
      }
      if (caps.pin_target_mask != 0) {
        check_pinned_reconnect(protocol, CERT_PIN);
        check_pinned_reconnect(protocol, SPKI_PIN);
        check_pinned_reconnect(protocol, BACKUP_PIN);
        for (int pin = CERT_PIN; pin <= BACKUP_PIN; ++pin) {
          int succeeds = pin != WRONG_PIN;
          run_case(protocol, backend, 0, RUMQTTC_TLS_ROOTS_PEM, "RUMQTTC_TEST_CA_PEM", pin,
                   DIRECT_TLS, "RUMQTTC_TEST_TLS_PORT", 0, succeeds);
          if (rumqttc_library_capabilities() & RUMQTTC_CAP_WEBSOCKET)
            run_case(protocol, backend, 0, RUMQTTC_TLS_ROOTS_PEM, "RUMQTTC_TEST_CA_PEM", pin,
                     DIRECT_WSS, "RUMQTTC_TEST_WSS_PORT", 0, succeeds);
        }
        run_case(protocol, backend, 0, RUMQTTC_TLS_ROOTS_PEM, "RUMQTTC_TEST_CA_PEM", CERT_PIN,
                 DIRECT_TLS, "RUMQTTC_TEST_TLS_PORT", 1, 0);
        run_case(protocol, backend, 0, RUMQTTC_TLS_ROOTS_PEM, "RUMQTTC_TEST_WRONG_CA_PEM", CERT_PIN,
                 DIRECT_TLS, "RUMQTTC_TEST_TLS_PORT", 0, 0);
      }
      if (rumqttc_library_capabilities() & RUMQTTC_CAP_HTTP_PROXY)
        run_case(protocol, backend, 0, RUMQTTC_TLS_ROOTS_PEM, "RUMQTTC_TEST_CA_PEM",
                 caps.pin_target_mask != 0 ? SPKI_PIN : NO_PIN, HTTPS_PROXY, "RUMQTTC_TEST_TLS_PORT", 0, 1);
    }
    run_case(RUMQTTC_PROTOCOL_V5, backend, 0, RUMQTTC_TLS_ROOTS_PEM, "RUMQTTC_TEST_CA_PEM",
             caps.pin_target_mask != 0 ? CERT_PIN : NO_PIN, REDIRECT_TLS, "RUMQTTC_TEST_TLS_PORT", 0, 1);
    if (rumqttc_library_capabilities() & RUMQTTC_CAP_WEBSOCKET)
      run_case(RUMQTTC_PROTOCOL_V5, backend, 0, RUMQTTC_TLS_ROOTS_PEM, "RUMQTTC_TEST_CA_PEM",
               caps.pin_target_mask != 0 ? SPKI_PIN : NO_PIN, REDIRECT_WSS, "RUMQTTC_TEST_WSS_PORT", 0, 1);
  }
  rumqttc_tls_backend_capabilities_t caps = RUMQTTC_TLS_BACKEND_CAPABILITIES_INIT;
  caps.version_policy_mask = UINT32_MAX;
  REQUIRE(rumqttc_tls_backend_capabilities(UINT32_MAX, &caps, NULL) != RUMQTTC_OK);
  REQUIRE(caps.version_policy_mask == 0 && caps.root_policy_mask == 0 && caps.pin_target_mask == 0);
  return 0;
}
