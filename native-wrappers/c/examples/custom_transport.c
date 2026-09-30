#include "example_common.h"
#include "transport_socket.h"
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc != 3)
        return 2;
    uint16_t port = (uint16_t)strtoul(argv[2], NULL, 10);
    const char *tunnel = getenv("RUMQTTC_TEST_BYTE_TUNNEL_PORT");
    uint16_t tunnel_port = tunnel ? (uint16_t)strtoul(tunnel, NULL, 10) : 0;
    for (rumqttc_protocol_t protocol = RUMQTTC_PROTOCOL_V4; protocol <= RUMQTTC_PROTOCOL_V5; ++protocol) {
        rumqttc_transport_registration_t *registration = NULL;
        socket_transport_t *host = socket_transport_new(tunnel_port, &registration);
        if (host == NULL)
            return 1;
        rumqttc_config_t *config = NULL;
        rumqttc_client_t *client = NULL;
        rumqttc_completion_t *completion = NULL;
        int failed = 1;
        rumqttc_event_t *event = NULL;
        rumqttc_publish_options_t publish = example_publish_options(RUMQTTC_QOS_1);
        if (rumqttc_config_new(protocol, &config, NULL) != RUMQTTC_OK ||
            rumqttc_config_set_broker(config, example_string(argv[1]), port, NULL) != RUMQTTC_OK ||
            rumqttc_config_set_client_id(config, example_string("custom-transport-example"), NULL) != RUMQTTC_OK ||
            rumqttc_config_set_transport_connector(config, registration, NULL) != RUMQTTC_OK)
            goto cleanup;
        rumqttc_transport_registration_destroy(registration);
        registration = NULL;
        if (rumqttc_client_start(config, &client, NULL) != RUMQTTC_OK)
            goto cleanup;
        rumqttc_config_destroy(config);
        config = NULL;
        event = example_next_event(client, RUMQTTC_EVENT_CONNECTED);
        if (event == NULL)
            goto cleanup;
        rumqttc_event_destroy(event);
        event = NULL;
        if (rumqttc_client_publish_tracked(client, example_string("example/custom-transport"),
                                           example_bytes("owned bytes", 11), &publish, &completion, NULL) != RUMQTTC_OK)
            goto cleanup;
        if (example_wait(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED) != 0)
            goto cleanup;
        failed = 0;
    cleanup:
        rumqttc_event_destroy(event);
        rumqttc_completion_destroy(completion);
        rumqttc_config_destroy(config);
        rumqttc_transport_registration_destroy(registration);
        /* A timed-out destroy leaves the driver alive: keep its host storage
         * intact and let the caller retry rather than joining/freeing it. */
        if (client != NULL && rumqttc_client_destroy_timeout_ms(client, 5000, NULL) != RUMQTTC_OK)
            return 1;
        if (!socket_transport_join_destroy(host) || failed)
            return 1;
    }
    return 0;
}
