/* Identical external C producer for pre-public and final shared libraries.
 * The runner supplies a TCP peer that accepts CONNECT but withholds CONNACK. */
#define _POSIX_C_SOURCE 200809L
#include "rumqttc.h"
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define REQUIRE(x)                                                                                                     \
    do {                                                                                                               \
        if (!(x)) {                                                                                                    \
            fprintf(stderr, "copy benchmark failed: %s\n", #x);                                                        \
            abort();                                                                                                   \
        }                                                                                                              \
    } while (0)
static uint64_t now_ns(void) {
    struct timespec t;
    REQUIRE(clock_gettime(CLOCK_MONOTONIC, &t) == 0);
    return (uint64_t)t.tv_sec * UINT64_C(1000000000) + (uint64_t)t.tv_nsec;
}
static rumqttc_string_view_t text(const char *s) { return (rumqttc_string_view_t){s, strlen(s)}; }
struct api {
    rumqttc_status_t (*config_new)(rumqttc_protocol_t, rumqttc_config_t **, rumqttc_error_t **);
    rumqttc_status_t (*config_set_broker)(rumqttc_config_t *, rumqttc_string_view_t, uint16_t, rumqttc_error_t **);
    rumqttc_status_t (*config_set_client_id)(rumqttc_config_t *, rumqttc_string_view_t, rumqttc_error_t **);
    rumqttc_status_t (*config_set_request_capacity)(rumqttc_config_t *, uint32_t, rumqttc_error_t **);
    rumqttc_status_t (*config_set_connection_timeout_seconds)(rumqttc_config_t *, uint64_t, rumqttc_error_t **);
    rumqttc_status_t (*client_start)(const rumqttc_config_t *, rumqttc_client_t **, rumqttc_error_t **);
    void (*config_destroy)(rumqttc_config_t *);
    rumqttc_status_t (*client_destroy_timeout_ms)(rumqttc_client_t *, uint64_t, rumqttc_error_t **);
    rumqttc_status_t (*client_try_publish)(rumqttc_client_t *, rumqttc_string_view_t, rumqttc_bytes_view_t,
                                           const rumqttc_publish_options_t *, uint64_t *, rumqttc_error_t **);
};
#define LOAD(n)                                                                                                        \
    do {                                                                                                               \
        void *symbol = dlsym(module, "rumqttc_" #n);                                                                   \
        REQUIRE(symbol && sizeof(symbol) == sizeof(api.n));                                                            \
        memcpy(&api.n, &symbol, sizeof(symbol));                                                                       \
    } while (0)
int main(int argc, char **argv) {
    REQUIRE(argc == 8);
    void *module = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    REQUIRE(module != NULL);
    struct api api;
    LOAD(config_new);
    LOAD(config_set_broker);
    LOAD(config_set_client_id);
    LOAD(config_set_request_capacity);
    LOAD(config_set_connection_timeout_seconds);
    LOAD(client_start);
    LOAD(config_destroy);
    LOAD(client_destroy_timeout_ms);
    LOAD(client_try_publish);
    unsigned protocol = (unsigned)strtoul(argv[3], NULL, 10);
    size_t len = (size_t)strtoull(argv[4], NULL, 10);
    unsigned iterations = (unsigned)strtoul(argv[6], NULL, 10);
    int lifecycle = strcmp(argv[5], "lifecycle") == 0;
    rumqttc_config_t *config = NULL;
    rumqttc_client_t *client = NULL;
    REQUIRE(api.config_new(protocol, &config, NULL) == 0);
    REQUIRE(api.config_set_broker(config, text("127.0.0.1"), (uint16_t)strtoul(argv[2], NULL, 10), NULL) == 0);
    REQUIRE(api.config_set_client_id(config, text("buffer-copy-benchmark"), NULL) == 0);
    REQUIRE(api.config_set_request_capacity(config, 1, NULL) == 0);
    REQUIRE(api.config_set_connection_timeout_seconds(config, 3600, NULL) == 0);
    REQUIRE(api.client_start(config, &client, NULL) == 0);
    api.config_destroy(config);
    uint8_t *input = malloc(len);
    REQUIRE(input != NULL);
    memset(input, 42, len);
    uint64_t id = 0;
    REQUIRE(api.client_try_publish(client, text("a"), (rumqttc_bytes_view_t){input, len}, NULL, &id, NULL) == 0);
    for (unsigned i = 0; i < 128; ++i)
        REQUIRE(api.client_try_publish(client, text("a"), (rumqttc_bytes_view_t){input, len}, NULL, &id, NULL) == 4);
    uint64_t start = now_ns();
    for (unsigned i = 0; i < iterations; ++i) {
        uint8_t *data = input;
        if (lifecycle) {
            data = malloc(len);
            REQUIRE(data != NULL);
            memset(data, 42, len);
        }
        REQUIRE(api.client_try_publish(client, text("a"), (rumqttc_bytes_view_t){data, len}, NULL, &id, NULL) == 4);
        if (lifecycle)
            free(data);
    }
    uint64_t elapsed = now_ns() - start;
    REQUIRE(api.client_destroy_timeout_ms(client, 5000, NULL) == 0);
    free(input);
    REQUIRE(dlclose(module) == 0);
    printf("{\"protocol\":%u,\"size\":%zu,\"phase\":\"%s\",\"sample\":%s,\"iterations\":%u,\"ns\":%llu}\n", protocol,
           len, argv[5], argv[7], iterations, (unsigned long long)elapsed);
    return 0;
}
