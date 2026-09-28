#include "native_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if !defined(_WIN32)
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#if defined(__linux__)
#include <unistd.h>
#endif
#endif

static uint16_t env_port(const char *name) {
  const char *text = getenv(name);
  char *end = NULL;
  REQUIRE(text != NULL);
  unsigned long value = strtoul(text, &end, 10);
  REQUIRE(end != text && *end == '\0' && value > 0 && value <= UINT16_MAX);
  return (uint16_t)value;
}

static void wait_connected(rumqttc_client_t *client, int reconnect) {
  unsigned disconnected = 0;
  for (unsigned attempt = 0; attempt < 10; ++attempt) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    native_check_event_accessors(event);
    rumqttc_event_destroy(event);
    if (kind == RUMQTTC_EVENT_CONNECTED) {
      REQUIRE(disconnected == (unsigned)reconnect);
      return;
    }
    REQUIRE(kind == RUMQTTC_EVENT_DISCONNECTED);
    ++disconnected;
  }
  REQUIRE(0);
}

static void close_client(rumqttc_client_t *client, int immediate) {
  if (immediate)
    CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  else
    CHECK(rumqttc_client_close_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
}

static void unix_options(rumqttc_protocol_t protocol, int immediate) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  const char *path = getenv("RUMQTTC_TEST_UNIX_PATH");
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  if (!(rumqttc_library_capabilities() & RUMQTTC_CAP_UNIX_SOCKET)) {
    REQUIRE(rumqttc_config_set_unix_broker(config, native_bytes((const uint8_t *)"mqtt.socket", 11), NULL) !=
            RUMQTTC_OK);
    rumqttc_config_destroy(config);
    return;
  }
  REQUIRE(path != NULL);
  CHECK(rumqttc_config_set_client_id(
      config, native_string(protocol == RUMQTTC_PROTOCOL_V4 ? "native-unix-reconnect-v4" : "native-unix-reconnect-v5"),
      NULL));
  CHECK(rumqttc_config_set_unix_broker(config, native_bytes((const uint8_t *)path, strlen(path)), NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  wait_connected(client, !immediate);
  close_client(client, immediate);
  CHECK(rumqttc_config_set_client_id(config, native_string("native-unix-missing"), NULL));
  char missing[160];
  REQUIRE(snprintf(missing, sizeof(missing), "%s.missing", path) > 0);
  CHECK(rumqttc_config_set_unix_broker(config, native_bytes((const uint8_t *)missing, strlen(missing)), NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
  rumqttc_error_t *error = NULL;
  rumqttc_error_kind_t kind = 0;
  CHECK(rumqttc_event_disconnected(event, NULL, &error));
  CHECK(rumqttc_error_kind(error, &kind));
  REQUIRE(kind == RUMQTTC_ERROR_NETWORK);
  rumqttc_error_destroy(error);
  rumqttc_event_destroy(event);
  close_client(client, 1);
  CHECK(rumqttc_config_set_unix_broker(config, native_bytes((const uint8_t *)path, strlen(path)), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-unix-timeout-case"), NULL));
  CHECK(rumqttc_config_set_connection_timeout_seconds(config, 1, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
  CHECK(rumqttc_event_disconnected(event, NULL, &error));
  CHECK(rumqttc_error_kind(error, &kind));
  REQUIRE(kind == RUMQTTC_ERROR_TIMEOUT);
  rumqttc_error_destroy(error);
  rumqttc_event_destroy(event);
  close_client(client, 1);
  rumqttc_config_destroy(config);
}

static void websocket_options(rumqttc_protocol_t protocol, int encrypted) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_websocket_header_edit_t edits[6] = {RUMQTTC_WEBSOCKET_HEADER_EDIT_INIT, RUMQTTC_WEBSOCKET_HEADER_EDIT_INIT,
                                              RUMQTTC_WEBSOCKET_HEADER_EDIT_INIT, RUMQTTC_WEBSOCKET_HEADER_EDIT_INIT,
                                              RUMQTTC_WEBSOCKET_HEADER_EDIT_INIT, RUMQTTC_WEBSOCKET_HEADER_EDIT_INIT};
  char url[120], value[] = "three";
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  if (!(rumqttc_library_capabilities() & RUMQTTC_CAP_WEBSOCKET)) {
    CHECK(rumqttc_config_set_transport_websocket(config, native_string("ws://localhost:1883/mqtt"), NULL));
    client = (rumqttc_client_t *)(uintptr_t)1;
    REQUIRE(rumqttc_client_start(config, &client, NULL) == RUMQTTC_CONFIG_ERROR);
    REQUIRE(client == NULL);
    rumqttc_config_destroy(config);
    return;
  }
  for (size_t index = 0; index < 6; ++index)
    edits[index].name = native_string("x-native");
  const char *protected_headers[] = {
      "Host", "Connection", "Upgrade", "Sec-WebSocket-Key", "Sec-WebSocket-Version", "Sec-WebSocket-Protocol"};
  for (size_t index = 0; index < sizeof(protected_headers) / sizeof(protected_headers[0]); ++index) {
    edits[0].name = native_string(protected_headers[index]);
    edits[0].value = native_string("private-header");
    REQUIRE(rumqttc_config_set_websocket_header_edits(config, edits, 1, NULL) == RUMQTTC_CONFIG_ERROR);
  }
  edits[0].name = native_string("x-native");
  edits[0].value = native_string("one");
  edits[1].value = native_string("two");
  edits[2].operation = RUMQTTC_WEBSOCKET_HEADER_REPLACE;
  edits[2].value = native_string(value);
  edits[3].name = native_string("x-remove");
  edits[3].value = native_string("remove");
  edits[4].name = native_string("x-remove");
  edits[4].operation = RUMQTTC_WEBSOCKET_HEADER_REMOVE;
  edits[5].value = native_string("");
  CHECK(rumqttc_config_set_websocket_header_edits(config, edits, 6, NULL));
  memset(value, 'x', strlen(value));
  uint16_t port = env_port(encrypted ? "RUMQTTC_TEST_WSS_PORT" : "RUMQTTC_TEST_WS_PORT");
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  REQUIRE(snprintf(url, sizeof(url), "%s://localhost:%u/native-headers", encrypted ? "wss" : "ws", port) > 0);
  if (encrypted) {
    const char *ca = getenv("RUMQTTC_TEST_CA_PEM");
    REQUIRE(ca != NULL);
    tls.backend =
        rumqttc_library_capabilities() & RUMQTTC_CAP_RUSTLS ? RUMQTTC_TLS_BACKEND_RUSTLS : RUMQTTC_TLS_BACKEND_NATIVE;
    tls.root_policy = RUMQTTC_TLS_ROOTS_PEM;
    tls.ca_pem = native_bytes((const uint8_t *)ca, strlen(ca));
    CHECK(rumqttc_config_set_transport_wss_with_options(config, native_string(url), &tls, NULL));
  } else {
    CHECK(rumqttc_config_set_transport_websocket(config, native_string(url), NULL));
  }
  CHECK(rumqttc_config_set_client_id(config,
                                     native_string(protocol == RUMQTTC_PROTOCOL_V4 ? "native-websocket-reconnect-v4"
                                                                                   : "native-websocket-reconnect-v5"),
                                     NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  wait_connected(client, 1);
  close_client(client, 0);
  CHECK(rumqttc_config_set_websocket_header_edits(config, NULL, 0, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-websocket-cleared"), NULL));
  REQUIRE(snprintf(url, sizeof(url), "%s://localhost:%u/native-headers-cleared", encrypted ? "wss" : "ws", port) > 0);
  if (encrypted)
    CHECK(rumqttc_config_set_transport_wss_with_options(config, native_string(url), &tls, NULL));
  else
    CHECK(rumqttc_config_set_transport_websocket(config, native_string(url), NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  wait_connected(client, 0);
  close_client(client, 0);
  rumqttc_config_destroy(config);
}

static void socket_options(rumqttc_protocol_t protocol) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  char address[64];
  uint16_t bind_port =
      env_port(protocol == RUMQTTC_PROTOCOL_V4 ? "RUMQTTC_TEST_BIND_PORT_V4" : "RUMQTTC_TEST_BIND_PORT_V5");
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-socket-options"), NULL));
  REQUIRE(snprintf(address, sizeof(address), "127.0.0.1:%u", bind_port) > 0);
  CHECK(rumqttc_config_set_local_bind_address(config, native_string(address), NULL));
  CHECK(rumqttc_config_set_tcp_send_buffer_size_bytes(config, 65536, NULL));
  CHECK(rumqttc_config_set_tcp_receive_buffer_size_bytes(config, 65536, NULL));
  CHECK(rumqttc_config_set_tcp_nodelay(config, 1, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  wait_connected(client, 0);
#if !defined(_WIN32)
  unsigned observed = 0;
  for (int descriptor = 0; descriptor < 1024; ++descriptor) {
    struct sockaddr_in local;
    socklen_t length = sizeof(local);
    if (getsockname(descriptor, (struct sockaddr *)&local, &length) != 0 || local.sin_family != AF_INET ||
        ntohs(local.sin_port) != bind_port)
      continue;
    int setting = 0;
    length = sizeof(setting);
    REQUIRE(getsockopt(descriptor, IPPROTO_TCP, TCP_NODELAY, &setting, &length) == 0 && setting == 1);
    REQUIRE(getsockopt(descriptor, SOL_SOCKET, SO_SNDBUF, &setting, &length) == 0 && setting >= 65536);
    REQUIRE(getsockopt(descriptor, SOL_SOCKET, SO_RCVBUF, &setting, &length) == 0 && setting >= 65536);
    ++observed;
  }
  REQUIRE(observed == 1);
#endif
  close_client(client, 0);
  CHECK(rumqttc_config_clear_local_bind_address(config, NULL));
  CHECK(rumqttc_config_clear_tcp_buffer_sizes(config, NULL));
  CHECK(rumqttc_config_set_tcp_nodelay(config, 0, NULL));
  rumqttc_config_destroy(config);
}

static void check_network_redaction(const rumqttc_error_t *error) {
  rumqttc_string_view_t views[2] = {{NULL, 0}, {NULL, 0}};
  const char *secrets[] = {"private-device", "rumqttc-missing-device"};
  CHECK(rumqttc_error_message(error, &views[0]));
  CHECK(rumqttc_error_source_chain(error, &views[1]));
  for (size_t index = 0; index < 2; ++index) {
    for (size_t secret = 0; secret < 2; ++secret) {
      size_t length = strlen(secrets[secret]);
      for (size_t offset = 0; offset + length <= views[index].len; ++offset)
        REQUIRE(memcmp(views[index].data + offset, secrets[secret], length) != 0);
    }
  }
}

static void platform_controls(rumqttc_protocol_t protocol) {
  rumqttc_config_t *config = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_error_kind_t kind = 0;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  REQUIRE(rumqttc_config_set_mptcp(config, 2, NULL) == RUMQTTC_INVALID_ARGUMENT);
#if defined(__linux__)
  CHECK(rumqttc_config_set_mptcp(config, 1, NULL));
#else
  REQUIRE(rumqttc_config_set_mptcp(config, 1, &error) == RUMQTTC_CONFIG_ERROR);
  CHECK(rumqttc_error_kind(error, &kind));
  REQUIRE(kind == RUMQTTC_ERROR_CONFIGURATION);
  check_network_redaction(error);
  rumqttc_error_destroy(error);
  error = NULL;
#endif
  CHECK(rumqttc_config_set_mptcp(config, 0, NULL));
#if defined(__linux__) || defined(__ANDROID__) || defined(__Fuchsia__)
  CHECK(rumqttc_config_set_bind_device(config, native_string("rumqttc-missing-device"), NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
  CHECK(rumqttc_event_disconnected(event, NULL, &error));
  CHECK(rumqttc_error_kind(error, &kind));
  REQUIRE(kind == RUMQTTC_ERROR_NETWORK);
  check_network_redaction(error);
  rumqttc_error_destroy(error);
  rumqttc_event_destroy(event);
  close_client(client, 1);
#else
  REQUIRE(rumqttc_config_set_bind_device(config, native_string("private-device"), &error) == RUMQTTC_CONFIG_ERROR);
  CHECK(rumqttc_error_kind(error, &kind));
  REQUIRE(kind == RUMQTTC_ERROR_CONFIGURATION);
  check_network_redaction(error);
  rumqttc_error_destroy(error);
#endif
  CHECK(rumqttc_config_clear_bind_device(config, NULL));
#if defined(__linux__)
  CHECK(rumqttc_config_set_mptcp(config, 1, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-mptcp"), NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  wait_connected(client, 0);
  /* Probe protocol availability without changing host kernel configuration. */
  int probe = socket(AF_INET, SOCK_STREAM, 262 /* IPPROTO_MPTCP */);
  int expected_protocol = probe >= 0 ? 262 : IPPROTO_TCP;
  if (probe >= 0)
    REQUIRE(close(probe) == 0);
  unsigned observed = 0;
  for (int descriptor = 0; descriptor < 1024; ++descriptor) {
    struct sockaddr_in remote;
    socklen_t length = sizeof(remote);
    if (getpeername(descriptor, (struct sockaddr *)&remote, &length) != 0 || remote.sin_family != AF_INET ||
        ntohs(remote.sin_port) != native_test_port())
      continue;
    int socket_protocol = 0;
    length = sizeof(socket_protocol);
    REQUIRE(getsockopt(descriptor, SOL_SOCKET, SO_PROTOCOL, &socket_protocol, &length) == 0);
    REQUIRE(socket_protocol == expected_protocol);
    ++observed;
  }
  REQUIRE(observed == 1);
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  rumqttc_completion_t *completion = NULL;
  CHECK(rumqttc_client_publish_tracked(client, native_string("native/mptcp"), native_bytes(NULL, 0), &options,
                                       &completion, NULL));
  native_wait_completion(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
  rumqttc_completion_destroy(completion);
  close_client(client, 0);
#endif
  rumqttc_config_destroy(config);
}

int main(void) {
  uint64_t capabilities = rumqttc_library_capabilities();
  for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
    unix_options(protocol, 0);
    unix_options(protocol, 1);
    websocket_options(protocol, 0);
    if (capabilities & (RUMQTTC_CAP_RUSTLS | RUMQTTC_CAP_NATIVE_TLS))
      websocket_options(protocol, 1);
    socket_options(protocol);
    platform_controls(protocol);
  }
  return 0;
}
