#include "example_common.h"

#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    rumqttc_execution_options_t options = RUMQTTC_EXECUTION_OPTIONS_INIT;
    rumqttc_execution_context_t *context = NULL;
    rumqttc_client_t *clients[2] = {NULL, NULL};
    rumqttc_config_t *config = NULL;
    rumqttc_error_t *error = NULL;
    int failed = 0;
    if (argc != 3) {
        fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
        return 2;
    }
    options.client_capacity = 2;
    if (example_report(rumqttc_execution_context_new(&options, &context, &error), &error, "create context"))
        return 1;
    for (size_t i = 0; i < 2; ++i) {
        rumqttc_event_t *event;
        /* Both protocol drivers share the same explicit execution owner. */
        if (example_report(rumqttc_config_new(i == 0 ? RUMQTTC_PROTOCOL_V4 : RUMQTTC_PROTOCOL_V5, &config, &error),
                           &error, "create config") ||
            example_report(rumqttc_config_set_broker(config, example_string(argv[1]),
                                                     (uint16_t)strtoul(argv[2], NULL, 10), &error),
                           &error, "set broker") ||
            example_report(
                rumqttc_config_set_client_id(config, example_string(i == 0 ? "c-shared-v4" : "c-shared-v5"), &error),
                &error, "set ID") ||
            example_report(rumqttc_config_set_execution_context(config, context, &error), &error, "select execution") ||
            example_report(rumqttc_client_start(config, &clients[i], &error), &error, "start client")) {
            failed = 1;
            break;
        }
        rumqttc_config_destroy(config);
        config = NULL;
        event = example_next_event(clients[i], RUMQTTC_EVENT_CONNECTED);
        if (event == NULL) {
            failed = 1;
            break;
        }
        rumqttc_event_destroy(event);
    }
    rumqttc_config_destroy(config);
    /* For graceful MQTT disconnects, close each client first. Context shutdown
     * itself requests immediate cleanup and joins the shared execution resources. */
    for (size_t i = 0; i < 2; ++i) {
        if (clients[i] &&
            example_report(rumqttc_client_close_timeout_ms(clients[i], 5000, &error), &error, "graceful close"))
            failed = 1;
    }
    if (example_report(rumqttc_execution_context_request_shutdown(context, &error), &error, "request shutdown") ||
        example_report(rumqttc_execution_context_join_timeout_ms(context, 10000, &error), &error, "join execution"))
        failed = 1;
    for (size_t i = 0; i < 2; ++i)
        example_destroy_client(&clients[i]);
    rumqttc_execution_context_release(context);
    rumqttc_error_destroy(error);
    if (!failed)
        puts("both protocols stopped; shared execution joined");
    return failed;
}
