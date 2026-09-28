#include "native_common.h"

#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct store_context {
  const char *client_id;
  atomic_uint calls;
  atomic_uint destroyed;
} store_context;

static void store_request(void *user_data, const rumqttc_store_request_t *request,
                          rumqttc_callback_completion_t *completion) {
  store_context *context = user_data;
  REQUIRE(request->scope.len == strlen("private-origin-scope"));
  REQUIRE(memcmp(request->scope.data, "private-origin-scope", request->scope.len) == 0);
  REQUIRE(request->client_id.len == strlen(context->client_id));
  REQUIRE(memcmp(request->client_id.data, context->client_id, request->client_id.len) == 0);
  atomic_fetch_add(&context->calls, 1);
  if (request->operation == RUMQTTC_STORE_LOAD)
    CHECK(rumqttc_callback_store_load_complete(completion, RUMQTTC_STORE_NOT_FOUND, native_bytes(NULL, 0)));
  else
    CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
}

static void destroy_store(void *user_data) {
  store_context *context = user_data;
  REQUIRE(atomic_fetch_add(&context->destroyed, 1) == 0);
}

static void run_case(int disconnect, const char *form, uint32_t transport, uint32_t expected_failure) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_store_registration_t *registration = NULL;
  rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  rumqttc_event_t *retained = NULL;
  rumqttc_string_view_t borrowed = {NULL, 0};
  char client_id[120], copied[160];
  unsigned followed = 0, finished = 0;
  REQUIRE(snprintf(client_id, sizeof(client_id), "native-redirect-matrix-%s-%s", disconnect ? "disconnect" : "connack",
                   form) > 0);
  store_context context = {client_id, 0, 0};
  vtable.load = store_request;
  vtable.save = store_request;
  vtable.clear = store_request;
  vtable.destroy = destroy_store;
  CHECK(rumqttc_store_registration_new(&vtable, &context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(client_id), NULL));
  CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_config_set_session_store(config, registration, native_string("private-origin-scope"), 5000,
                                         16u * 1024u * 1024u, NULL));
  if (transport == RUMQTTC_REDIRECT_TRANSPORT_TLS || transport == RUMQTTC_REDIRECT_TRANSPORT_WSS) {
    const char *ca = getenv("RUMQTTC_TEST_CA_PEM");
    REQUIRE(ca != NULL);
    tls.backend =
        (rumqttc_library_capabilities() & RUMQTTC_CAP_RUSTLS) ? RUMQTTC_TLS_BACKEND_RUSTLS : RUMQTTC_TLS_BACKEND_NATIVE;
    tls.root_policy = RUMQTTC_TLS_ROOTS_PEM;
    tls.ca_pem = native_bytes((const uint8_t *)ca, strlen(ca));
  }
  CHECK(rumqttc_config_set_v5_redirect_policy(
      config, RUMQTTC_REDIRECT_FOLLOW, 1, transport,
      transport == RUMQTTC_REDIRECT_TRANSPORT_TLS || transport == RUMQTTC_REDIRECT_TRANSPORT_WSS ? &tls : NULL, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  for (unsigned attempt = 0; attempt < 16 && !finished; ++attempt) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    native_check_event_accessors(event);
    if (kind == RUMQTTC_EVENT_REDIRECT) {
      uint32_t source = 0, reason = 0, failure = 0, decision = 0;
      uint8_t failure_present = 0, reference_present = 0, limit_present = 0;
      uint64_t attempts = 0, limit = 0;
      rumqttc_string_view_t reference = {NULL, 0};
      CHECK(
          rumqttc_event_redirect(event, &source, &reason, &failure_present, &failure, &reference_present, &reference));
      CHECK(rumqttc_event_redirect_diagnostics(event, &decision, &attempts, &limit_present, &limit, NULL, NULL, NULL,
                                               NULL, NULL));
      REQUIRE(reference_present == 1 && reference.len > 0);
      REQUIRE(limit_present == 1 && limit == 1 && attempts <= 1);
      if (!failure_present) {
        REQUIRE(source == (disconnect ? RUMQTTC_REDIRECT_SOURCE_DISCONNECT : RUMQTTC_REDIRECT_SOURCE_CONNACK));
        REQUIRE(reason ==
                (disconnect ? RUMQTTC_REDIRECT_REASON_SERVER_MOVED : RUMQTTC_REDIRECT_REASON_USE_ANOTHER_SERVER));
        REQUIRE(decision == RUMQTTC_REDIRECT_DECISION_FOLLOW);
        followed = 1;
      } else {
        REQUIRE(failure == expected_failure && decision == RUMQTTC_REDIRECT_DECISION_REJECT);
        if (expected_failure == RUMQTTC_REDIRECT_FAILURE_ATTEMPT_LIMIT)
          REQUIRE(attempts == 1 && followed);
      }
      if (retained == NULL) {
        size_t required = 0;
        CHECK(rumqttc_string_copy(reference, copied, sizeof(copied), &required));
        REQUIRE(required == reference.len && required < sizeof(copied));
        copied[required] = '\0';
        borrowed = reference;
        retained = event;
        event = NULL;
      }
    } else if (kind == RUMQTTC_EVENT_CONNECTED) {
      if (followed) {
        REQUIRE(expected_failure == 0);
        finished = 1;
      } else {
        REQUIRE(disconnect);
      }
    } else if (kind == RUMQTTC_EVENT_DRIVER_TERMINATED) {
      rumqttc_error_t *error = NULL;
      uint8_t present = 0;
      uint32_t failure = 0;
      REQUIRE(expected_failure != 0);
      CHECK(rumqttc_event_disconnected(event, NULL, &error));
      CHECK(rumqttc_error_redirect_failure(error, &present, &failure));
      REQUIRE(present == 1 && failure == expected_failure);
      rumqttc_error_destroy(error);
      finished = 1;
    }
    rumqttc_event_destroy(event);
  }
  REQUIRE(finished && retained != NULL && atomic_load(&context.calls) > 0);
  REQUIRE(strlen(copied) == borrowed.len && memcmp(copied, borrowed.data, borrowed.len) == 0);
  rumqttc_event_destroy(retained);
  REQUIRE(strlen(copied) > 0);
  unsigned previous_calls = atomic_load(&context.calls);
  if (expected_failure == 0) {
    rumqttc_publish_options_t publish = native_publish_options(RUMQTTC_QOS_1);
    rumqttc_completion_t *completion = NULL;
    CHECK(rumqttc_client_publish_tracked(client, native_string("native/redirect/barrier"), native_bytes(NULL, 0),
                                         &publish, &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    rumqttc_completion_destroy(completion);
  }
  native_close_destroy(client);
  REQUIRE(atomic_load(&context.calls) == previous_calls);
  rumqttc_config_destroy(config);
  rumqttc_store_registration_destroy(registration);
  REQUIRE(atomic_load(&context.destroyed) == 1);
}

static void disabled_transport(uint32_t transport) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = (rumqttc_client_t *)(uintptr_t)1;
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  tls.backend = RUMQTTC_TLS_BACKEND_RUSTLS;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  rumqttc_status_t status = rumqttc_config_set_v5_redirect_policy(
      config, RUMQTTC_REDIRECT_FOLLOW, 1, transport,
      transport == RUMQTTC_REDIRECT_TRANSPORT_TLS || transport == RUMQTTC_REDIRECT_TRANSPORT_WSS ? &tls : NULL, NULL);
  if (status == RUMQTTC_OK) {
    REQUIRE(rumqttc_client_start(config, &client, NULL) == RUMQTTC_CONFIG_ERROR);
    REQUIRE(client == NULL);
  } else {
    REQUIRE(status == RUMQTTC_CONFIG_ERROR);
  }
  rumqttc_config_destroy(config);
}

int main(void) {
  uint64_t capabilities = rumqttc_library_capabilities();
  if (!(capabilities & RUMQTTC_CAP_WEBSOCKET)) {
    disabled_transport(RUMQTTC_REDIRECT_TRANSPORT_WS);
    disabled_transport(RUMQTTC_REDIRECT_TRANSPORT_WSS);
  }
  if (!(capabilities & (RUMQTTC_CAP_RUSTLS | RUMQTTC_CAP_NATIVE_TLS)))
    disabled_transport(RUMQTTC_REDIRECT_TRANSPORT_TLS);
  for (int disconnect = 0; disconnect < 2; ++disconnect) {
    run_case(disconnect, "authority", RUMQTTC_REDIRECT_TRANSPORT_TCP, 0);
    run_case(disconnect, "mqtt", RUMQTTC_REDIRECT_TRANSPORT_TCP, 0);
    if (capabilities & (RUMQTTC_CAP_RUSTLS | RUMQTTC_CAP_NATIVE_TLS))
      run_case(disconnect, "mqtts", RUMQTTC_REDIRECT_TRANSPORT_TLS, 0);
    if (capabilities & RUMQTTC_CAP_WEBSOCKET) {
      run_case(disconnect, "ws", RUMQTTC_REDIRECT_TRANSPORT_WS, 0);
      if (capabilities & (RUMQTTC_CAP_RUSTLS | RUMQTTC_CAP_NATIVE_TLS))
        run_case(disconnect, "wss", RUMQTTC_REDIRECT_TRANSPORT_WSS, 0);
    }
    run_case(disconnect, "malformed", RUMQTTC_REDIRECT_TRANSPORT_TCP, RUMQTTC_REDIRECT_FAILURE_INVALID_REFERENCE);
    run_case(disconnect, "disallowed", RUMQTTC_REDIRECT_TRANSPORT_TCP, RUMQTTC_REDIRECT_FAILURE_INVALID_REFERENCE);
    run_case(disconnect, "exhausted", RUMQTTC_REDIRECT_TRANSPORT_TCP, RUMQTTC_REDIRECT_FAILURE_ATTEMPT_LIMIT);
  }
  return 0;
}
