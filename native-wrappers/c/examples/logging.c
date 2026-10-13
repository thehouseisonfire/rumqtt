#include "example_common.h"
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>

/* This owner is retained until its logging-thread destructor runs. */
typedef struct log_output { FILE *stream; } log_output;
static uint32_t write_record(void *data, const rumqttc_log_record_t *record) {
    log_output *output = data;
    rumqttc_log_record_info_t info = RUMQTTC_LOG_RECORD_INFO_INIT;
    if (rumqttc_log_record_info(record, &info, NULL) != RUMQTTC_OK) return 1;
    /* Views borrow the callback record. Use record_copy to keep one afterward. */
    if (fprintf(output->stream, "client=%" PRIu64 " level=%" PRIu32 " %.*s [%.*s]\n",
                info.client_id, info.level, (int)info.name.len, info.name.data,
                (int)info.target.len, info.target.data) < 0) return 1;
    return RUMQTTC_OK;
}
static void destroy_output(void *data) { free(data); }
int main(int argc, char **argv) {
    rumqttc_logging_context_t *logging = NULL;
    rumqttc_logging_registration_t *sink = NULL;
    rumqttc_logging_flush_t *flush = NULL;
    rumqttc_logging_vtable_t vtable = RUMQTTC_LOGGING_VTABLE_INIT;
    rumqttc_logging_filter_t filter = RUMQTTC_LOGGING_FILTER_INIT;
    rumqttc_logging_statistics_t stats = RUMQTTC_LOGGING_STATISTICS_INIT;
    rumqttc_config_t *config = NULL;
    rumqttc_client_t *client = NULL;
    rumqttc_error_t *error = NULL;
    log_output *output = NULL;
    int result = 1;
    if (argc != 3) { fprintf(stderr, "usage: %s HOST PORT\n", argv[0]); return 2; }
    if (!(rumqttc_library_capabilities() & RUMQTTC_CAP_LOGGING_SINK)) {
        fprintf(stderr, "build the C library with --features tracing\n"); return 77;
    }
    if (example_report(rumqttc_logging_context_new(NULL, &logging, &error), &error, "logging context")) goto cleanup;
    output = malloc(sizeof(*output)); if (output == NULL) goto cleanup;
    output->stream = stdout; vtable.record = write_record; vtable.destroy = destroy_output;
    filter.level = RUMQTTC_LOG_DEBUG;
    if (example_report(rumqttc_logging_registration_new(logging, &filter, &vtable, output, &sink, &error), &error, "logging sink")) goto cleanup;
    output = NULL; /* Successful registration transfers ownership. */
    if (example_report(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, &error), &error, "configuration") ||
        example_report(rumqttc_config_set_broker(config, example_string(argv[1]), (uint16_t)strtoul(argv[2], NULL, 10), &error), &error, "broker") ||
        example_report(rumqttc_config_set_client_id(config, example_string("c-logging-example"), &error), &error, "client ID") ||
        example_report(rumqttc_config_set_logging(config, sink, &error), &error, "attach logging") ||
        example_report(rumqttc_client_start(config, &client, &error), &error, "start")) goto cleanup;
    {
        rumqttc_event_t *connected = example_next_event(client, RUMQTTC_EVENT_CONNECTED);
        if (connected == NULL) goto cleanup;
        rumqttc_event_destroy(connected);
    }
    if (example_report(rumqttc_client_close_timeout_ms(client, 5000, &error), &error, "MQTT close")) goto cleanup;
    /* MQTT close does not wait for logging delivery. Flush captures its own prefix. */
    if (example_report(rumqttc_logging_context_flush(logging, &flush, &error), &error, "logging flush") ||
        example_report(rumqttc_logging_flush_wait_timeout_ms(flush, 5000, &error), &error, "flush wait") ||
        example_report(rumqttc_logging_context_statistics(logging, &stats, &error), &error, "logging statistics")) goto cleanup;
    printf("delivered=%" PRIu64 ", overload=%" PRIu64 ", failures=%" PRIu64 "\n", stats.delivered, stats.overload, stats.failures);
    result = 0;
cleanup:
    free(output);
    example_destroy_client(&client);
    rumqttc_config_destroy(config);
    rumqttc_logging_registration_release(sink);
    rumqttc_logging_flush_release(flush);
    if (logging != NULL) {
        (void)rumqttc_logging_context_request_shutdown(logging, NULL);
        if (example_report(rumqttc_logging_context_join_timeout_ms(logging, 5000, &error), &error, "logging join")) {
            /* Timeout retains active owners; retry join before unloading callback/library code. */
            result = 1;
        }
        rumqttc_logging_context_release(logging);
    }
    rumqttc_error_destroy(error);
    return result;
}
