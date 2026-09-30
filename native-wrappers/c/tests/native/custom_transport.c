#include "native_common.h"
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
/* A host-owned in-memory byte pipe. It calls the public C API exclusively;
 * the private fixture is reused only as the simulated host transport. */
#include "../../../wrapper-core/tests/fixtures/transport.c"

typedef struct memory_stream memory_stream;
typedef struct host {
    rumqttc_transport_stream_t *handles[4];
    memory_stream *streams[4];
    atomic_uint count;
    atomic_uint destroyed;
    atomic_uint streams_destroyed;
    atomic_uint cancellations;
    atomic_uint stop;
    atomic_uintptr_t late;
    rumqttc_protocol_t protocol;
    uint32_t terminal_failure;
    atomic_uint redirect_attempts;
    atomic_uintptr_t redirect_completion;
} host;
struct memory_stream {
    host *host;
    fixture *pipe;
};
typedef struct work {
    rumqttc_callback_completion_t *completion;
    uint64_t id;
} work;

static void release_work(void *data) {
    work *operation = data;
    rumqttc_callback_completion_destroy(operation->completion);
    free(operation);
}
static uint32_t finish_bytes(void *data, const uint8_t *bytes, size_t length, uint32_t result) {
    work *operation = data;
    rumqttc_transport_response_t response = {0};
    response.struct_size = sizeof(response);
    response.result = result ? RUMQTTC_TRANSPORT_FAILURE_IO : 0;
    if (!result)
        response.bytes = native_bytes(bytes, length);
    return rumqttc_callback_transport_complete(operation->completion, &response);
}
static uint32_t finish_count(void *data, size_t count, uint32_t result) {
    work *operation = data;
    rumqttc_transport_response_t response = {0};
    response.struct_size = sizeof(response);
    response.result = result ? RUMQTTC_TRANSPORT_FAILURE_IO : 0;
    if (!result)
        response.count = count;
    return rumqttc_callback_transport_complete(operation->completion, &response);
}
static uint32_t finish_unit(void *data, uint32_t result) {
    work *operation = data;
    rumqttc_transport_response_t response = {0};
    response.struct_size = sizeof(response);
    response.result = result ? RUMQTTC_TRANSPORT_FAILURE_IO : 0;
    return rumqttc_callback_transport_complete(operation->completion, &response);
}
static void perform(void *data, const rumqttc_transport_io_request_t *request,
                    rumqttc_callback_completion_t *completion) {
    memory_stream *stream = data;
    REQUIRE(request->struct_size == sizeof(*request));
    REQUIRE(request->generation > 0 && request->operation_id > 0);
    work *operation = calloc(1, sizeof(*operation));
    REQUIRE(operation != NULL);
    operation->id = request->operation_id;
    CHECK(rumqttc_callback_completion_retain(completion, &operation->completion));
    switch (request->operation) {
    case RUMQTTC_TRANSPORT_READ:
        REQUIRE(request->read_limit <= 16384);
        proof_read(stream->pipe, operation, request->read_limit, finish_bytes, release_work);
        break;
    case RUMQTTC_TRANSPORT_WRITE:
        REQUIRE(request->input.len <= 16384);
        proof_write(stream->pipe, operation, request->input.data, request->input.len, finish_count, release_work);
        break;
    case RUMQTTC_TRANSPORT_FLUSH:
        proof_unit(stream->pipe, FLUSH, operation, finish_unit, release_work);
        break;
    case RUMQTTC_TRANSPORT_SHUTDOWN:
        proof_unit(stream->pipe, SHUTDOWN, operation, finish_unit, release_work);
        break;
    default:
        REQUIRE(0);
    }
}
static void cancel_stream(void *data, uint64_t id) {
    memory_stream *stream = data;
    rumqttc_callback_completion_t *late = NULL;
    acquire(stream->pipe);
    for (unsigned kind = 0; kind < KINDS; ++kind) {
        work *operation = stream->pipe->operations[kind].token;
        if (operation != NULL && operation->id == id) {
            CHECK(rumqttc_callback_completion_retain(operation->completion, &late));
            break;
        }
    }
    unlock(stream->pipe);
    if (late != NULL) {
        uintptr_t expected = 0;
        if (!atomic_compare_exchange_strong(&stream->host->late, &expected, (uintptr_t)late))
            rumqttc_callback_completion_destroy(late);
    }
    atomic_fetch_add(&stream->host->cancellations, 1);
    proof_release_pending(stream->pipe);
}
static void destroy_stream(void *data) {
    memory_stream *stream = data;
    proof_destroy(stream->pipe);
    atomic_fetch_add(&stream->host->streams_destroyed, 1);
    free(stream);
}
static void cancel_connect(void *data, uint64_t id) {
    (void)data;
    (void)id;
}
static void destroy_host(void *data) {
    host *context = data;
    atomic_fetch_add(&context->destroyed, 1);
}
static void connect_stream(void *data, const rumqttc_transport_connect_request_t *request,
                           rumqttc_callback_completion_t *completion) {
    host *context = data;
    unsigned index = atomic_load(&context->count);
    REQUIRE(index < 4);
    REQUIRE(request->protocol == context->protocol);
    REQUIRE(request->generation == index + 1);
    REQUIRE(request->operation_id > 0);
    REQUIRE(request->remaining_timeout_ns > 0 && request->remaining_timeout_ns <= UINT64_C(5000000000));
    REQUIRE(request->target.len == strlen("supplied.invalid:1883"));
    REQUIRE(memcmp(request->target.data, "supplied.invalid:1883", request->target.len) == 0);
    REQUIRE(!request->send_buffer_present && !request->receive_buffer_present && !request->tcp_nodelay);
    memory_stream *stream = calloc(1, sizeof(*stream));
    REQUIRE(stream != NULL);
    stream->host = context;
    stream->pipe = proof_new(2, 1u << WRITE);
    rumqttc_transport_stream_vtable_t table = {0};
    table.struct_size = sizeof(table);
    table.mode = RUMQTTC_TRANSPORT_BASE;
    table.perform = perform;
    table.cancel = cancel_stream;
    table.destroy = destroy_stream;
    /* ERROR_OUT_SUCCESS: rumqttc_transport_stream_new */
    CHECK(rumqttc_transport_stream_new(completion, &table, stream, &context->handles[index], NULL));
    context->streams[index] = stream;
    uint8_t connack[] = {0x20, 3, 0, 0, 0};
    if (context->protocol == RUMQTTC_PROTOCOL_V4)
        connack[1] = 2;
    proof_feed(stream->pipe, connack, context->protocol == RUMQTTC_PROTOCOL_V4 ? 4 : 5);
    rumqttc_transport_response_t response = {0};
    response.struct_size = sizeof(response);
    response.stream = context->handles[index];
    response.network_handling = RUMQTTC_TRANSPORT_NETWORK_NOT_APPLICABLE;
    CHECK(rumqttc_callback_transport_complete(completion, &response));
    REQUIRE(rumqttc_callback_transport_complete(completion, NULL) == RUMQTTC_INVALID_STATE);
    atomic_store(&context->count, index + 1);
}
static void pump(host *context) {
    unsigned count = atomic_load(&context->count);
    for (unsigned index = 0; index < count; ++index)
        proof_finish(context->streams[index]->pipe, WRITE, 0, SIZE_MAX, 0);
}
static int pump_worker(void *data) {
    host *context = data;
    while (!atomic_load(&context->stop)) {
        pump(context);
        native_sleep_ms(1);
    }
    return 0;
}
static rumqttc_event_t *wait_event(host *context, rumqttc_client_t *client, uint32_t expected) {
    uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
    for (;;) {
        pump(context);
        rumqttc_event_t *event = NULL;
        uint32_t status = rumqttc_client_event_recv_timeout_ms(client, 1, &event, NULL);
        if (status == RUMQTTC_OK) {
            rumqttc_event_kind_t kind = 0;
            CHECK(rumqttc_event_kind(event, &kind));
            if (kind == expected)
                return event;
            rumqttc_event_destroy(event);
        } else
            REQUIRE(status == RUMQTTC_TIMEOUT);
        REQUIRE(native_monotonic_ms() < deadline);
    }
}
static uint16_t published_id(fixture *pipe) {
    uint8_t wire[65536];
    size_t length = proof_outgoing(pipe, wire, sizeof(wire));
    size_t offset = 0;
    while (offset < length) {
        uint8_t header = wire[offset++];
        size_t remaining = 0, shift = 0;
        uint8_t byte;
        do {
            if (offset == length || shift > 21)
                return 0;
            byte = wire[offset++];
            remaining |= (size_t)(byte & 127) << shift;
            shift += 7;
        } while (byte & 128);
        if (remaining > length - offset)
            return 0;
        if ((header >> 4) == 3) {
            REQUIRE(((header >> 1) & 3) == 1 && remaining >= 4);
            size_t topic_len = ((size_t)wire[offset] << 8) | wire[offset + 1];
            REQUIRE(topic_len == strlen("native/custom") && topic_len + 4 <= remaining);
            REQUIRE(memcmp(wire + offset + 2, "native/custom", topic_len) == 0);
            return (uint16_t)(((uint16_t)wire[offset + topic_len + 2] << 8) | wire[offset + topic_len + 3]);
        }
        offset += remaining;
    }
    return 0;
}
static void publish_acknowledged(host *context, rumqttc_client_t *client, unsigned index) {
    rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
    rumqttc_completion_t *completion = NULL;
    CHECK(rumqttc_client_publish_tracked(client, native_string("native/custom"),
                                         native_bytes((const uint8_t *)"owned payload", 13), &options, &completion,
                                         NULL));
    uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
    uint16_t id = 0;
    while (!(id = published_id(context->streams[index]->pipe))) {
        REQUIRE(native_monotonic_ms() < deadline);
        native_sleep_ms(1);
    }
    uint8_t puback[] = {0x40, 2, (uint8_t)(id >> 8), (uint8_t)id};
    proof_feed(context->streams[index]->pipe, puback, sizeof(puback));
    native_wait_completion(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    rumqttc_completion_destroy(completion);
}
static void run(rumqttc_protocol_t protocol) {
    host context = {0};
    context.protocol = protocol;
    rumqttc_transport_vtable_t table = {0};
    table.struct_size = sizeof(table);
    table.mode = RUMQTTC_TRANSPORT_BASE;
    table.max_retained_operations = 16;
    table.connect = connect_stream;
    table.cancel = cancel_connect;
    table.destroy = destroy_host;
    rumqttc_transport_registration_t *registration = NULL;
    CHECK(rumqttc_transport_registration_new(&table, &context, &registration, NULL));
    rumqttc_config_t *config = NULL;
    CHECK(rumqttc_config_new(protocol, &config, NULL));
    CHECK(rumqttc_config_set_broker(config, native_string("supplied.invalid"), 1883, NULL));
    CHECK(rumqttc_config_set_client_id(config, native_string("native-custom"), NULL));
    CHECK(rumqttc_config_set_transport_connector(config, registration, NULL));
    rumqttc_transport_registration_destroy(registration);
    rumqttc_client_t *client = NULL;
    native_thread_t *worker = native_thread_start(pump_worker, &context);
    CHECK(rumqttc_client_start(config, &client, NULL));
    rumqttc_config_destroy(config);
    rumqttc_event_destroy(wait_event(&context, client, RUMQTTC_EVENT_CONNECTED));
    REQUIRE(atomic_load(&context.destroyed) == 0);
    publish_acknowledged(&context, client, 0);
    proof_eof(context.streams[0]->pipe);
    rumqttc_event_destroy(wait_event(&context, client, RUMQTTC_EVENT_DISCONNECTED));
    rumqttc_event_destroy(wait_event(&context, client, RUMQTTC_EVENT_CONNECTED));
    REQUIRE(atomic_load(&context.count) == 2);
    publish_acknowledged(&context, client, 1);
    uint64_t read_deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
    while (!proof_pending(context.streams[1]->pipe, READ)) {
        REQUIRE(native_monotonic_ms() < read_deadline);
        native_sleep_ms(1);
    }
    if (protocol == RUMQTTC_PROTOCOL_V4)
        CHECK(rumqttc_client_close_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    else
        CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    atomic_store(&context.stop, 1);
    REQUIRE(native_thread_join(worker) == 0);
    uintptr_t late = atomic_load(&context.late);
    REQUIRE(late != 0 && atomic_load(&context.cancellations) > 0);
    REQUIRE(rumqttc_callback_transport_complete((rumqttc_callback_completion_t *)late,
                                                (const rumqttc_transport_response_t *)(uintptr_t)1) ==
            RUMQTTC_INVALID_STATE);
    rumqttc_callback_completion_destroy((rumqttc_callback_completion_t *)late);
    for (unsigned i = 0; i < atomic_load(&context.count); ++i)
        rumqttc_transport_stream_destroy(context.handles[i]);
    REQUIRE(atomic_load(&context.streams_destroyed) == 2);
    REQUIRE(atomic_load(&context.destroyed) == 1);
}
static void timeout_connect(void *data, const rumqttc_transport_connect_request_t *request,
                            rumqttc_callback_completion_t *completion) {
    host *context = data;
    REQUIRE(request->remaining_timeout_ns > 0 && request->remaining_timeout_ns <= UINT64_C(1000000000));
    if (atomic_fetch_add(&context->count, 1) == 0) {
        rumqttc_callback_completion_t *retained = NULL;
        CHECK(rumqttc_callback_completion_retain(completion, &retained));
        atomic_store(&context->late, (uintptr_t)retained);
    } else {
        /* Stop retries after the timeout so this test retains exactly one
         * cancelled operation without relying on the driver's scheduling. */
        rumqttc_transport_response_t response = RUMQTTC_TRANSPORT_RESPONSE_INIT;
        response.result = RUMQTTC_TRANSPORT_FAILURE_RESOURCE_LIMIT;
        CHECK(rumqttc_callback_transport_complete(completion, &response));
    }
}
static void timeout_cancel(void *data, uint64_t id) {
    host *context = data;
    REQUIRE(id > 0);
    atomic_fetch_add(&context->cancellations, 1);
}
static void timeout_and_failed_construction(rumqttc_protocol_t protocol) {
    for (int timeout = 0; timeout <= 1; ++timeout) {
        host context = {0};
        rumqttc_transport_vtable_t table = RUMQTTC_TRANSPORT_VTABLE_INIT;
        table.connect = timeout_connect;
        table.cancel = timeout_cancel;
        table.destroy = destroy_host;
        rumqttc_transport_registration_t *registration = NULL;
        rumqttc_config_t *config = NULL;
        rumqttc_client_t *client = NULL;
        CHECK(rumqttc_transport_registration_new(&table, &context, &registration, NULL));
        CHECK(rumqttc_config_new(protocol, &config, NULL));
        CHECK(rumqttc_config_set_transport_connector(config, registration, NULL));
        rumqttc_transport_registration_destroy(registration);
        if (!timeout) {
            /* The empty default broker fails before any transport work. */
            REQUIRE(rumqttc_client_start(config, &client, NULL) == RUMQTTC_CONFIG_ERROR);
            REQUIRE(client == NULL && atomic_load(&context.count) == 0);
            REQUIRE(atomic_load(&context.destroyed) == 0);
            rumqttc_config_destroy(config);
            REQUIRE(atomic_load(&context.destroyed) == 1);
            continue;
        }
        CHECK(rumqttc_config_set_broker(config, native_string("supplied.invalid"), 1883, NULL));
        CHECK(rumqttc_config_set_connection_timeout_seconds(config, 1, NULL));
        CHECK(rumqttc_client_start(config, &client, NULL));
        rumqttc_config_destroy(config);
        rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
        rumqttc_error_t *error = NULL;
        uint32_t kind = 0;
        CHECK(rumqttc_event_disconnected(event, NULL, &error));
        CHECK(rumqttc_error_kind(error, &kind));
        REQUIRE(kind == RUMQTTC_ERROR_TIMEOUT);
        rumqttc_error_destroy(error);
        rumqttc_event_destroy(event);
        event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
        error = NULL;
        CHECK(rumqttc_event_disconnected(event, NULL, &error));
        uint8_t present = 0;
        uint32_t failure = 0;
        CHECK(rumqttc_error_transport_failure(error, &present, &failure));
        REQUIRE(present && failure == RUMQTTC_TRANSPORT_FAILURE_RESOURCE_LIMIT);
        CHECK(rumqttc_error_transport_failure(error, NULL, &failure));
        CHECK(rumqttc_error_transport_failure(error, &present, NULL));
        REQUIRE(rumqttc_error_transport_failure(error, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
        rumqttc_error_destroy(error);
        rumqttc_event_destroy(event);
        REQUIRE(atomic_load(&context.count) == 2);
        event = NULL;
        REQUIRE(rumqttc_client_event_try_recv(client, &event, NULL) == RUMQTTC_WOULD_BLOCK);
        REQUIRE(event == NULL);
        CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
        REQUIRE(atomic_load(&context.count) == 2);
        REQUIRE(atomic_load(&context.cancellations) == 1);
        uintptr_t late = atomic_load(&context.late);
        REQUIRE(late != 0);
        REQUIRE(atomic_load(&context.destroyed) == 0);
        REQUIRE(rumqttc_callback_transport_complete((rumqttc_callback_completion_t *)late,
                                                    (const rumqttc_transport_response_t *)(uintptr_t)1) ==
                RUMQTTC_INVALID_STATE);
        rumqttc_callback_completion_destroy((rumqttc_callback_completion_t *)late);
        REQUIRE(atomic_load(&context.destroyed) == 1);
    }
}
static void terminal_connect(void *data, const rumqttc_transport_connect_request_t *request,
                             rumqttc_callback_completion_t *completion) {
    host *context = data;
    REQUIRE(request->protocol == context->protocol);
    atomic_fetch_add(&context->count, 1);
    rumqttc_transport_response_t response = RUMQTTC_TRANSPORT_RESPONSE_INIT;
    response.result = context->terminal_failure;
    CHECK(rumqttc_callback_transport_complete(completion, &response));
}
static void terminal_connector_failures(rumqttc_protocol_t protocol) {
    const uint32_t failures[] = {
        RUMQTTC_TRANSPORT_FAILURE_NETWORK_OPTIONS,
        RUMQTTC_TRANSPORT_FAILURE_COMPOSITION,
        RUMQTTC_TRANSPORT_FAILURE_INVALID_RESULT,
        RUMQTTC_TRANSPORT_FAILURE_PANIC,
        RUMQTTC_TRANSPORT_FAILURE_RESOURCE_LIMIT,
    };
    for (size_t index = 0; index < sizeof(failures) / sizeof(failures[0]); ++index) {
        host context = {0};
        context.protocol = protocol;
        context.terminal_failure = failures[index];
        rumqttc_transport_vtable_t table = RUMQTTC_TRANSPORT_VTABLE_INIT;
        table.connect = terminal_connect;
        table.cancel = cancel_connect;
        table.destroy = destroy_host;
        rumqttc_transport_registration_t *registration = NULL;
        rumqttc_config_t *config = NULL;
        rumqttc_client_t *client = NULL;
        CHECK(rumqttc_transport_registration_new(&table, &context, &registration, NULL));
        CHECK(rumqttc_config_new(protocol, &config, NULL));
        CHECK(rumqttc_config_set_broker(config, native_string("supplied.invalid"), 1883, NULL));
        CHECK(rumqttc_config_set_transport_connector(config, registration, NULL));
        CHECK(rumqttc_client_start(config, &client, NULL));
        rumqttc_config_destroy(config);
        rumqttc_transport_registration_destroy(registration);
        rumqttc_event_t *event = NULL;
        CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
        uint32_t kind = 0;
        CHECK(rumqttc_event_kind(event, &kind));
        REQUIRE(kind == RUMQTTC_EVENT_DRIVER_TERMINATED);
        rumqttc_error_t *error = NULL;
        CHECK(rumqttc_event_disconnected(event, NULL, &error));
        uint8_t present = 0, retryable = 1;
        uint32_t failure = 0;
        CHECK(rumqttc_error_transport_failure(error, &present, &failure));
        REQUIRE(present && failure == context.terminal_failure);
        CHECK(rumqttc_error_flags(error, &retryable, NULL));
        REQUIRE(!retryable);
        rumqttc_error_destroy(error);
        rumqttc_event_destroy(event);
        event = NULL;
        REQUIRE(rumqttc_client_event_try_recv(client, &event, NULL) == RUMQTTC_WOULD_BLOCK);
        REQUIRE(event == NULL && atomic_load(&context.count) == 1);
        CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
        REQUIRE(atomic_load(&context.count) == 1 && atomic_load(&context.destroyed) == 1);
    }
}
static void redirect_connect(void *data, const rumqttc_transport_connect_request_t *request,
                             rumqttc_callback_completion_t *completion) {
    host *context = data;
    if (request->target.len == strlen("supplied.invalid:1883") &&
        memcmp(request->target.data, "supplied.invalid:1883", request->target.len) == 0) {
        connect_stream(data, request, completion);
        return;
    }
    if (atomic_fetch_add(&context->redirect_attempts, 1) == 0) {
        rumqttc_callback_completion_t *retained = NULL;
        CHECK(rumqttc_callback_completion_retain(completion, &retained));
        atomic_store(&context->redirect_completion, (uintptr_t)retained);
    } else {
        rumqttc_transport_response_t response = RUMQTTC_TRANSPORT_RESPONSE_INIT;
        response.result = context->terminal_failure;
        CHECK(rumqttc_callback_transport_complete(completion, &response));
    }
}
static void redirect_resolve(void *data, const rumqttc_resolver_request_t *request,
                             rumqttc_callback_completion_t *completion) {
    (void)data;
    REQUIRE(request->owner.len > 0);
    rumqttc_srv_record_t records[2] = {RUMQTTC_SRV_RECORD_INIT, RUMQTTC_SRV_RECORD_INIT};
    records[0].target = native_string("preferred.invalid");
    records[0].port = 1883;
    records[1].target = native_string("backup.invalid");
    records[1].port = 1883;
    records[1].priority = 1;
    CHECK(rumqttc_callback_srv_complete(completion, RUMQTTC_SRV_SUCCESS, records, 2));
}
static void redirect_resolver_destroy(void *data) { (void)data; }
static void check_redirect_error(rumqttc_error_t *error, uint32_t expected) {
    uint8_t present = 0, retryable = 1;
    uint32_t failure = 0;
    REQUIRE(error != NULL);
    CHECK(rumqttc_error_transport_failure(error, &present, &failure));
    REQUIRE(present && failure == expected);
    CHECK(rumqttc_error_redirect_failure(error, &present, &failure));
    REQUIRE(present && failure == RUMQTTC_REDIRECT_FAILURE_TRANSPORT);
    CHECK(rumqttc_error_flags(error, &retryable, NULL));
    REQUIRE(!retryable);
}
static void redirect_connector_failures(void) {
    const uint32_t failures[] = {
        RUMQTTC_TRANSPORT_FAILURE_NETWORK_OPTIONS, RUMQTTC_TRANSPORT_FAILURE_COMPOSITION,
        RUMQTTC_TRANSPORT_FAILURE_INVALID_RESULT, RUMQTTC_TRANSPORT_FAILURE_PANIC,
        RUMQTTC_TRANSPORT_FAILURE_RESOURCE_LIMIT, RUMQTTC_TRANSPORT_FAILURE_CONNECT,
        RUMQTTC_TRANSPORT_FAILURE_IO, RUMQTTC_TRANSPORT_FAILURE_TIMEOUT,
        RUMQTTC_TRANSPORT_FAILURE_ABANDONED,
    };
    for (unsigned srv = 0; srv < 2; ++srv) {
        for (size_t index = 0; index < sizeof(failures) / sizeof(failures[0]); ++index) {
            host context = {0};
            context.protocol = RUMQTTC_PROTOCOL_V5;
            context.terminal_failure = failures[index];
            rumqttc_transport_vtable_t table = RUMQTTC_TRANSPORT_VTABLE_INIT;
            table.connect = redirect_connect;
            table.cancel = cancel_connect;
            table.destroy = destroy_host;
            rumqttc_transport_registration_t *registration = NULL;
            rumqttc_config_t *config = NULL;
            rumqttc_client_t *client = NULL;
            CHECK(rumqttc_transport_registration_new(&table, &context, &registration, NULL));
            CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
            CHECK(rumqttc_config_set_broker(config, native_string("supplied.invalid"), 1883, NULL));
            CHECK(rumqttc_config_set_client_id(config, native_string("redirect-custom"), NULL));
            CHECK(rumqttc_config_set_transport_connector(config, registration, NULL));
            CHECK(rumqttc_config_set_v5_redirect_policy(config, RUMQTTC_REDIRECT_FOLLOW, 3,
                                                         RUMQTTC_REDIRECT_TRANSPORT_TCP, NULL, NULL));
            rumqttc_resolver_vtable_t resolver_table = RUMQTTC_RESOLVER_VTABLE_INIT;
            resolver_table.resolve = redirect_resolve;
            resolver_table.destroy = redirect_resolver_destroy;
            rumqttc_resolver_registration_t *resolver = NULL;
            CHECK(rumqttc_resolver_registration_new(&resolver_table, NULL, &resolver, NULL));
            CHECK(rumqttc_config_set_v5_srv_resolver(config, resolver, NULL));
            rumqttc_resolver_registration_destroy(resolver);
            CHECK(rumqttc_client_start(config, &client, NULL));
            rumqttc_config_destroy(config);
            rumqttc_transport_registration_destroy(registration);
            rumqttc_event_destroy(wait_event(&context, client, RUMQTTC_EVENT_CONNECTED));
            const char *reference = srv ? "_mqtt._tcp.service.invalid" : "preferred.invalid:1883";
            size_t length = strlen(reference);
            REQUIRE(length + 6 < 128);
            uint8_t packet[128] = {0xe0, (uint8_t)(length + 5), 0x9c, (uint8_t)(length + 3),
                                   0x1c, 0, (uint8_t)length};
            memcpy(packet + 7, reference, length);
            proof_feed(context.streams[0]->pipe, packet, length + 7);
            rumqttc_event_destroy(wait_event(&context, client, RUMQTTC_EVENT_REDIRECT));
            rumqttc_string_view_t filter = native_string("pending");
            rumqttc_unsubscribe_options_t options = RUMQTTC_UNSUBSCRIBE_OPTIONS_INIT;
            rumqttc_completion_t *pending = NULL;
            CHECK(rumqttc_client_unsubscribe_tracked(client, &filter, 1, &options, &pending, NULL));
            uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
            uintptr_t retained = 0;
            while (!(retained = atomic_load(&context.redirect_completion))) {
                REQUIRE(native_monotonic_ms() < deadline);
                native_sleep_ms(1);
            }
            rumqttc_transport_response_t response = RUMQTTC_TRANSPORT_RESPONSE_INIT;
            response.result = context.terminal_failure;
            CHECK(rumqttc_callback_transport_complete((rumqttc_callback_completion_t *)retained, &response));
            rumqttc_callback_completion_destroy((rumqttc_callback_completion_t *)retained);
            rumqttc_event_t *event = wait_event(&context, client, RUMQTTC_EVENT_REDIRECT);
            uint8_t failed = 0, candidate = 0;
            uint32_t failure = 0;
            uint64_t candidate_index = 0, count = 0;
            CHECK(rumqttc_event_redirect(event, NULL, NULL, &failed, &failure, NULL, NULL));
            REQUIRE(failed && failure == RUMQTTC_REDIRECT_FAILURE_TRANSPORT);
            uint32_t classified = context.terminal_failure;
            int retryable = classified == RUMQTTC_TRANSPORT_FAILURE_CONNECT ||
                            classified == RUMQTTC_TRANSPORT_FAILURE_IO ||
                            classified == RUMQTTC_TRANSPORT_FAILURE_TIMEOUT ||
                            classified == RUMQTTC_TRANSPORT_FAILURE_ABANDONED;
            unsigned attempts = srv && retryable ? 2 : 1;
            CHECK(rumqttc_event_redirect_diagnostics(event, NULL, NULL, NULL, NULL, NULL, NULL,
                                                       &candidate, &candidate_index, &count));
            REQUIRE(!srv || (candidate && candidate_index == attempts && count == 2));
            rumqttc_event_destroy(event);
            event = wait_event(&context, client, RUMQTTC_EVENT_DRIVER_TERMINATED);
            rumqttc_error_t *error = NULL;
            CHECK(rumqttc_event_disconnected(event, NULL, &error));
            check_redirect_error(error, context.terminal_failure);
            rumqttc_error_destroy(error);
            rumqttc_event_destroy(event);
            error = NULL;
            REQUIRE(rumqttc_completion_wait_timeout_ms(pending, NATIVE_DEADLINE_MS, &error) == RUMQTTC_AMBIGUOUS);
            check_redirect_error(error, context.terminal_failure);
            rumqttc_error_destroy(error);
            rumqttc_completion_destroy(pending);
            REQUIRE(atomic_load(&context.redirect_attempts) == attempts);
            CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
            uintptr_t late = atomic_load(&context.late);
            if (late != 0)
                rumqttc_callback_completion_destroy((rumqttc_callback_completion_t *)late);
            rumqttc_transport_stream_destroy(context.handles[0]);
            REQUIRE(atomic_load(&context.streams_destroyed) == 1 && atomic_load(&context.destroyed) == 1);
        }
    }
}
int main(void) {
    REQUIRE(rumqttc_library_capabilities() & RUMQTTC_CAP_TRANSPORT_CALLBACKS);
    run(RUMQTTC_PROTOCOL_V4);
    run(RUMQTTC_PROTOCOL_V5);
    timeout_and_failed_construction(RUMQTTC_PROTOCOL_V4);
    timeout_and_failed_construction(RUMQTTC_PROTOCOL_V5);
    terminal_connector_failures(RUMQTTC_PROTOCOL_V4);
    terminal_connector_failures(RUMQTTC_PROTOCOL_V5);
    redirect_connector_failures();
    return 0;
}
