#include "native_common.h"
#include "../../examples/transport_socket.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint16_t port(const char *name) {
    const char *value = getenv(name);
    REQUIRE(value != NULL);
    unsigned long number = strtoul(value, NULL, 10);
    REQUIRE(number && number <= UINT16_MAX);
    return (uint16_t)number;
}
static rumqttc_tls_options_t tls(const char *name, uint32_t backend) {
    rumqttc_tls_options_t options = RUMQTTC_TLS_OPTIONS_INIT;
    const char *roots = getenv(name);
    REQUIRE(roots != NULL);
    options.backend = backend;
    options.root_policy = RUMQTTC_TLS_ROOTS_PEM;
    options.ca_pem = native_bytes((const uint8_t *)roots, strlen(roots));
    return options;
}
static void run(uint32_t protocol, uint32_t transport, uint32_t proxy_kind, uint32_t backend, int wrong_roots) {
    rumqttc_config_t *config = NULL;
    rumqttc_client_t *client = NULL;
    rumqttc_transport_registration_t *registration = NULL;
    socket_transport_t *host = socket_transport_new(port("RUMQTTC_TEST_BYTE_TUNNEL_PORT"), &registration);
    REQUIRE(host != NULL);
    uint16_t broker_port = port(transport == 0   ? "RUMQTTC_TEST_PORT"
                                : transport == 1 ? "RUMQTTC_TEST_TLS_PORT"
                                : transport == 2 ? "RUMQTTC_TEST_WS_PORT"
                                                 : "RUMQTTC_TEST_WSS_PORT");
    CHECK(rumqttc_config_new(protocol, &config, NULL));
    CHECK(rumqttc_config_set_broker(config, native_string("localhost"), broker_port, NULL));
    CHECK(rumqttc_config_set_client_id(config, native_string("native-custom-tunnel"), NULL));
    rumqttc_tls_options_t broker_tls = tls(wrong_roots ? "RUMQTTC_TEST_WRONG_CA_PEM" : "RUMQTTC_TEST_CA_PEM", backend);
    char url[128];
    if (transport >= 2) {
        REQUIRE(snprintf(url, sizeof(url), "%s://localhost:%u/mqtt", transport == 2 ? "ws" : "wss", broker_port) > 0);
        if (transport == 2)
            CHECK(rumqttc_config_set_transport_websocket(config, native_string(url), NULL));
        else
            CHECK(rumqttc_config_set_transport_wss_with_options(config, native_string(url), &broker_tls, NULL));
    } else if (transport == 1)
        CHECK(rumqttc_config_set_transport_tls_with_options(config, &broker_tls, NULL));
    rumqttc_proxy_options_t proxy = RUMQTTC_PROXY_OPTIONS_INIT;
    rumqttc_tls_options_t proxy_tls = tls("RUMQTTC_TEST_PROXY_CA_PEM", backend);
    if (proxy_kind) {
        proxy.protocol = proxy_kind;
        proxy.host = native_string("localhost");
        proxy.port =
            port(proxy_kind == RUMQTTC_PROXY_HTTPS ? "RUMQTTC_TEST_TLS_TUNNEL_PORT" : "RUMQTTC_TEST_TUNNEL_PORT");
        proxy.credentials_present = 1;
        proxy.username = native_bytes((const uint8_t *)"proxy-private-user", 18);
        proxy.password = native_bytes((const uint8_t *)"proxy-private-password", 22);
        if (proxy_kind == RUMQTTC_PROXY_HTTPS)
            proxy.tls = &proxy_tls;
        CHECK(rumqttc_config_set_proxy(config, &proxy, NULL));
    }
    CHECK(rumqttc_config_set_transport_connector(config, registration, NULL));
    rumqttc_transport_registration_destroy(registration);
    CHECK(rumqttc_client_start(config, &client, NULL));
    rumqttc_config_destroy(config);
    rumqttc_event_t *event =
        native_wait_event(client, wrong_roots ? RUMQTTC_EVENT_DISCONNECTED : RUMQTTC_EVENT_CONNECTED);
    if (wrong_roots) {
        rumqttc_error_t *error = NULL;
        uint32_t kind = 0;
        CHECK(rumqttc_event_disconnected(event, NULL, &error));
        CHECK(rumqttc_error_kind(error, &kind));
        REQUIRE(kind == RUMQTTC_ERROR_TLS || (transport == 3 && kind == RUMQTTC_ERROR_NETWORK));
        rumqttc_error_destroy(error);
    }
    rumqttc_event_destroy(event);
    if (!wrong_roots) {
        rumqttc_completion_t *completion = NULL;
        rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
        CHECK(rumqttc_client_publish_tracked(client, native_string("native/custom-tunnel"),
                                             native_bytes((const uint8_t *)"owned", 5), &options, &completion, NULL));
        native_wait_completion(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
        rumqttc_completion_destroy(completion);
        CHECK(rumqttc_client_close_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    } else
        CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    REQUIRE(socket_transport_join_destroy(host));
}
int main(void) {
    uint64_t capabilities = rumqttc_library_capabilities();
    for (uint32_t protocol = 1; protocol <= 2; ++protocol) {
        for (uint32_t transport = 0; transport <= 2; transport += 2) {
            if (transport == 2 && !(capabilities & RUMQTTC_CAP_WEBSOCKET))
                continue;
            run(protocol, transport, 0, RUMQTTC_TLS_BACKEND_RUSTLS, 0);
            if (capabilities & RUMQTTC_CAP_HTTP_PROXY)
                run(protocol, transport, RUMQTTC_PROXY_HTTP, RUMQTTC_TLS_BACKEND_RUSTLS, 0);
            if (capabilities & RUMQTTC_CAP_SOCKS5_PROXY)
                run(protocol, transport, RUMQTTC_PROXY_SOCKS5, RUMQTTC_TLS_BACKEND_RUSTLS, 0);
        }
        for (uint32_t backend = RUMQTTC_TLS_BACKEND_RUSTLS; backend <= RUMQTTC_TLS_BACKEND_NATIVE; ++backend) {
            if (!(capabilities & (backend == RUMQTTC_TLS_BACKEND_RUSTLS ? RUMQTTC_CAP_RUSTLS : RUMQTTC_CAP_NATIVE_TLS)))
                continue;
            for (uint32_t transport = 0; transport <= 3; ++transport) {
                if (transport >= 2 && !(capabilities & RUMQTTC_CAP_WEBSOCKET))
                    continue;
                if (capabilities & RUMQTTC_CAP_HTTP_PROXY)
                    run(protocol, transport, RUMQTTC_PROXY_HTTPS, backend, 0);
                if (transport == 0 || transport == 2)
                    continue;
                run(protocol, transport, 0, backend, 0);
                run(protocol, transport, 0, backend, 1);
                if (capabilities & RUMQTTC_CAP_HTTP_PROXY) {
                    run(protocol, transport, RUMQTTC_PROXY_HTTP, backend, 0);
                }
                if (capabilities & RUMQTTC_CAP_SOCKS5_PROXY)
                    run(protocol, transport, RUMQTTC_PROXY_SOCKS5, backend, 0);
            }
        }
    }
    return 0;
}
