/* Real C consumer. The external runner samples this process at each barrier. */
#if !defined(_WIN32)
#define _POSIX_C_SOURCE 200809L
#endif
#include "native_common.h"
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#if defined(_WIN32)
#include <windows.h>
#else
#include <time.h>
#endif

static uint64_t clock_ns(void) {
#if defined(_WIN32)
    LARGE_INTEGER value, frequency;
    REQUIRE(QueryPerformanceCounter(&value) && QueryPerformanceFrequency(&frequency));
    return (uint64_t)((double)value.QuadPart * 1000000000.0 / (double)frequency.QuadPart);
#else
    struct timespec value;
    REQUIRE(clock_gettime(CLOCK_MONOTONIC, &value) == 0);
    return (uint64_t)value.tv_sec * UINT64_C(1000000000) + (uint64_t)value.tv_nsec;
#endif
}

static void barrier(void) {
    int input;
    fflush(stdout);
    input = getchar();
    REQUIRE(input == '\n');
}

static void json_string(rumqttc_string_view_t text) {
    putchar('"');
    for (size_t i = 0; i < text.len; ++i) {
        unsigned char byte = (unsigned char)text.data[i];
        if (byte == '"' || byte == '\\')
            putchar('\\');
        if (byte < 32)
            printf("\\u%04x", byte);
        else
            putchar(byte);
    }
    putchar('"');
}

static void samples(const char *name, const uint64_t *values, size_t count, size_t stride) {
    printf("\"%s\":[", name);
    for (size_t i = 0; i < count; i += stride)
        printf("%s%llu", i ? "," : "", (unsigned long long)values[i]);
    printf("]");
}

typedef struct busy_producer {
    rumqttc_client_t *client;
    atomic_int stop;
    size_t admitted;
} busy_producer;

static int produce(void *argument) {
    busy_producer *producer = argument;
    rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_0);
    unsigned char payload[64] = {0};
    while (!atomic_load(&producer->stop)) {
        rumqttc_completion_t *pending[8] = {NULL};
        size_t admitted = 0;
        /* Bound offered work like the eight public asyncio producers. A continuous
         * untracked producer otherwise measures queue saturation, not peer scheduling. */
        for (size_t i = 0; i < 8 && !atomic_load(&producer->stop); ++i) {
            rumqttc_status_t status = rumqttc_client_publish_tracked(producer->client, native_string("execution/busy"),
                                                                     native_bytes(payload, sizeof(payload)), &options,
                                                                     &pending[admitted], NULL);
            if (status == RUMQTTC_BACKPRESSURE)
                break;
            CHECK(status);
            admitted++;
            producer->admitted++;
        }
        for (size_t i = 0; i < admitted; ++i) {
            CHECK(rumqttc_completion_wait_timeout_ms(pending[i], 5000, NULL));
            rumqttc_completion_destroy(pending[i]);
        }
        if (!admitted)
            native_sleep_ms(0);
    }
    return 0;
}

int main(int argc, char **argv) {
    const char *mode;
    const char *scenario;
    size_t count, rounds, sample_count;
    uint32_t protocol, qos;
    rumqttc_client_t **clients;
    rumqttc_completion_t **operations;
    rumqttc_execution_context_t *contexts[2] = {NULL, NULL};
    size_t context_count;
    rumqttc_execution_options_t execution = RUMQTTC_EXECUTION_OPTIONS_INIT;
    uint64_t *started, *admission, *completion, *events;
    uint64_t startup, elapsed, teardown;
    busy_producer producer = {NULL, 0, 0};
    native_thread_t *producer_thread = NULL;
    unsigned char payload[64] = {0};
    unsigned char *ca = NULL;
    size_t ca_size = 0;
    const char *transport = getenv("RUMQTTC_EXECUTION_TRANSPORT");
    REQUIRE(argc == 6);
    mode = argv[1];
    count = (size_t)strtoul(argv[2], NULL, 10);
    protocol = (uint32_t)strtoul(argv[3], NULL, 10);
    qos = (uint32_t)strtoul(argv[4], NULL, 10);
    scenario = argv[5];
    rounds = getenv("RUMQTTC_EXECUTION_ROUNDS") ? (size_t)strtoul(getenv("RUMQTTC_EXECUTION_ROUNDS"), NULL, 10) : 10;
    REQUIRE(count > 0 && count <= 1000 && rounds > 0 && rounds <= 1000 && qos <= 2);
    sample_count = count * rounds;
    clients = calloc(count, sizeof(*clients));
    operations = calloc(count, sizeof(*operations));
    started = calloc(sample_count, sizeof(*started));
    admission = calloc(sample_count, sizeof(*admission));
    completion = calloc(sample_count, sizeof(*completion));
    events = calloc(sample_count, sizeof(*events));
    REQUIRE(clients && operations && started && admission && completion && events);
    execution.client_capacity = count;
    if (getenv("RUMQTTC_EXECUTION_CA")) {
        FILE *file = fopen(getenv("RUMQTTC_EXECUTION_CA"), "rb");
        long length;
        REQUIRE(file != NULL && fseek(file, 0, SEEK_END) == 0);
        length = ftell(file);
        REQUIRE(length > 0 && length < 65536 && fseek(file, 0, SEEK_SET) == 0);
        ca_size = (size_t)length;
        ca = malloc(ca_size);
        REQUIRE(ca && fread(ca, 1, ca_size, file) == ca_size);
        REQUIRE(fclose(file) == 0);
    }
    context_count = strcmp(mode, "dedicated") == 0 ? 0 : strcmp(mode, "shards") == 0 ? 2 : 1;
    for (size_t i = 0; i < context_count; ++i)
        CHECK(rumqttc_execution_context_new(&execution, &contexts[i], NULL));
    startup = clock_ns();
    for (size_t i = 0; i < count; ++i) {
        rumqttc_config_t *config = NULL;
        rumqttc_status_t status;
        rumqttc_error_t *error = NULL;
        char identifier[64];
        snprintf(identifier, sizeof(identifier), "execution-%u-%zu", protocol, i);
        CHECK(rumqttc_config_new(protocol, &config, NULL));
        CHECK(rumqttc_config_set_broker(
            config,
            native_string(transport && (strcmp(transport, "tls") == 0 || strcmp(transport, "wss") == 0) ? "localhost"
                                                                                                        : "127.0.0.1"),
            native_test_port(), NULL));
        CHECK(rumqttc_config_set_client_id(config, native_string(identifier), NULL));
        CHECK(rumqttc_config_set_event_capacity(config, 1024, NULL));
        if (transport && strcmp(transport, "tls") == 0) {
            CHECK(rumqttc_config_set_transport_tls(config, native_bytes(ca, ca_size), native_bytes(NULL, 0),
                                                   native_bytes(NULL, 0), NULL));
        } else if (transport && (strcmp(transport, "ws") == 0 || strcmp(transport, "wss") == 0)) {
            char url[128];
            snprintf(url, sizeof(url), "%s://localhost:%u/mqtt", transport, native_test_port());
            if (strcmp(transport, "wss") == 0)
                CHECK(rumqttc_config_set_transport_wss(config, native_string(url), native_bytes(ca, ca_size),
                                                       native_bytes(NULL, 0), native_bytes(NULL, 0), NULL));
            else
                CHECK(rumqttc_config_set_transport_websocket(config, native_string(url), NULL));
        }
        if (context_count)
            CHECK(rumqttc_config_set_execution_context(config, contexts[i % context_count], NULL));
        status = rumqttc_client_start(config, &clients[i], &error);
        rumqttc_config_destroy(config);
        if (status != RUMQTTC_OK) {
            rumqttc_string_view_t message = {NULL, 0};
            if (error)
                CHECK(rumqttc_error_message(error, &message));
            printf("{\"phase\":\"failed_start\",\"status\":%u,\"started_clients\":%zu,\"error\":", status, i);
            json_string(message);
            printf("}\n");
            rumqttc_error_destroy(error);
            fflush(stdout);
            for (size_t j = 0; j < i; ++j)
                CHECK(rumqttc_client_destroy_timeout_ms(clients[j], 5000, NULL));
            for (size_t k = 0; k < context_count; ++k) {
                CHECK(rumqttc_execution_context_request_shutdown(contexts[k], NULL));
                CHECK(rumqttc_execution_context_join_timeout_ms(contexts[k], 30000, NULL));
                rumqttc_execution_context_release(contexts[k]);
            }
            free(clients);
            free(operations);
            free(started);
            free(admission);
            free(completion);
            free(events);
            free(ca);
            return 2;
        }
        rumqttc_event_destroy(native_wait_event(clients[i], RUMQTTC_EVENT_CONNECTED));
        if (strcmp(scenario, "incoming") == 0) {
            rumqttc_subscription_t subscription = native_subscription("execution/traffic", RUMQTTC_QOS_0);
            CHECK(rumqttc_client_subscribe_tracked(clients[i], &subscription, 1, NULL, &operations[i], NULL));
            CHECK(rumqttc_completion_wait_timeout_ms(operations[i], 5000, NULL));
            rumqttc_completion_destroy(operations[i]);
            operations[i] = NULL;
        }
    }
    startup = clock_ns() - startup;
    printf("{\"phase\":\"ready\",\"startup_ns\":%llu}\n", (unsigned long long)startup);
    barrier();
    if (strcmp(scenario, "hotspot") == 0) {
        producer.client = clients[0];
        producer_thread = native_thread_start(produce, &producer);
        REQUIRE(producer_thread != NULL);
    }
    elapsed = clock_ns();
    if (strcmp(scenario, "reconnect") == 0) {
        for (size_t i = 0; i < count; ++i)
            rumqttc_event_destroy(native_wait_event(clients[i], RUMQTTC_EVENT_CONNECTED));
        sample_count = 0;
    } else if (strcmp(scenario, "idle") == 0) {
        native_sleep_ms(200);
        sample_count = 0;
    } else {
        rumqttc_publish_options_t options = native_publish_options(qos);
        for (size_t round = 0; round < rounds; ++round) {
            if (strcmp(scenario, "periodic") == 0)
                native_sleep_ms(100);
            /* Incoming broadcasts use one publisher and all subscribers, avoiding N-squared traffic. */
            size_t publishers = strcmp(scenario, "incoming") == 0 ? 1 : count;
            for (size_t i = 0; i < publishers; ++i) {
                size_t sample = round * count + i;
                started[sample] = clock_ns();
                for (;;) {
                    rumqttc_status_t status = rumqttc_client_publish_tracked(
                        clients[i], native_string("execution/traffic"), native_bytes(payload, sizeof(payload)),
                        &options, &operations[i], NULL);
                    if (status != RUMQTTC_BACKPRESSURE) {
                        CHECK(status);
                        break;
                    }
                    REQUIRE(clock_ns() - started[sample] < UINT64_C(5000000000));
                    native_sleep_ms(0);
                }
                admission[sample] = clock_ns() - started[sample];
            }
            for (size_t i = 0; i < publishers; ++i) {
                CHECK(rumqttc_completion_wait_timeout_ms(operations[i], 5000, NULL));
                completion[round * count + i] = clock_ns() - started[round * count + i];
                rumqttc_completion_destroy(operations[i]);
                operations[i] = NULL;
            }
            if (strcmp(scenario, "incoming") == 0) {
                for (size_t i = 0; i < count; ++i) {
                    rumqttc_event_destroy(native_wait_event(clients[i], RUMQTTC_EVENT_INCOMING_PUBLISH));
                    events[round * count + i] = clock_ns() - started[round * count];
                }
            }
        }
    }
    elapsed = clock_ns() - elapsed;
    if (producer_thread) {
        atomic_store(&producer.stop, 1);
        REQUIRE(native_thread_join(producer_thread) == 0);
    }
    teardown = clock_ns();
    for (size_t k = 0; k < context_count; ++k)
        CHECK(rumqttc_execution_context_request_shutdown(contexts[k], NULL));
    for (size_t i = 0; i < count; ++i)
        CHECK(rumqttc_client_destroy_timeout_ms(clients[i], 30000, NULL));
    for (size_t k = 0; k < context_count; ++k) {
        CHECK(rumqttc_execution_context_join_timeout_ms(contexts[k], 30000, NULL));
        rumqttc_execution_context_release(contexts[k]);
    }
    teardown = clock_ns() - teardown;
    printf("{\"phase\":\"finished\",\"elapsed_ns\":%llu,\"teardown_ns\":%llu,", (unsigned long long)elapsed,
           (unsigned long long)teardown);
    printf("\"busy_admitted\":%zu,", producer.admitted);
    samples("admission_ns", admission, sample_count, strcmp(scenario, "incoming") == 0 ? count : 1);
    printf(",");
    samples("completion_ns", completion, sample_count, strcmp(scenario, "incoming") == 0 ? count : 1);
    printf(",");
    samples("event_ns", events, strcmp(scenario, "incoming") == 0 ? sample_count : 0, 1);
    printf("}\n");
    barrier();
    free(clients);
    free(operations);
    free(started);
    free(admission);
    free(completion);
    free(events);
    free(ca);
    return 0;
}
