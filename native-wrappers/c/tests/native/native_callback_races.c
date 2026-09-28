#include "native_common.h"

#include <stdatomic.h>
#include <string.h>

enum callback_kind { STORE, AUTH, RESOLVER };

typedef struct callback_context {
  enum callback_kind kind;
  atomic_uintptr_t pending;
  atomic_uint active;
  atomic_uint destroyed;
} callback_context;

typedef struct completion_race {
  callback_context *context;
  rumqttc_callback_completion_t *completion;
  atomic_uint ready;
  atomic_uint release;
  rumqttc_status_t result;
} completion_race;

static void retain_pending(callback_context *context, rumqttc_callback_completion_t *completion) {
  rumqttc_callback_completion_t *retained = NULL;
  REQUIRE(atomic_fetch_add(&context->active, 1) == 0);
  CHECK(rumqttc_callback_completion_retain(completion, &retained));
  REQUIRE(atomic_exchange(&context->pending, (uintptr_t)retained) == 0);
  REQUIRE(atomic_fetch_sub(&context->active, 1) == 1);
}

static void store_load(void *user_data, const rumqttc_store_request_t *request,
                       rumqttc_callback_completion_t *completion) {
  REQUIRE(request->operation == RUMQTTC_STORE_LOAD);
  retain_pending(user_data, completion);
}

static void store_write(void *user_data, const rumqttc_store_request_t *request,
                        rumqttc_callback_completion_t *completion) {
  callback_context *context = user_data;
  REQUIRE(request->operation == RUMQTTC_STORE_SAVE || request->operation == RUMQTTC_STORE_CLEAR);
  REQUIRE(atomic_fetch_add(&context->active, 1) == 0);
  CHECK(rumqttc_callback_store_write_complete(completion, RUMQTTC_STORE_FOUND));
  REQUIRE(atomic_fetch_sub(&context->active, 1) == 1);
}

static void auth_respond(void *user_data, const rumqttc_auth_request_t *request,
                         rumqttc_callback_completion_t *completion) {
  if (request->stage == RUMQTTC_AUTH_REQUEST_START) {
    retain_pending(user_data, completion);
  } else {
    REQUIRE(request->stage == RUMQTTC_AUTH_REQUEST_SUCCESS);
    rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
    CHECK(rumqttc_callback_auth_complete(completion, &response));
  }
}

static void resolve(void *user_data, const rumqttc_resolver_request_t *request,
                    rumqttc_callback_completion_t *completion) {
  REQUIRE(request->owner.len > 0);
  retain_pending(user_data, completion);
}

static void destroy_owner(void *user_data) {
  callback_context *context = user_data;
  REQUIRE(atomic_load(&context->active) == 0);
  REQUIRE(atomic_fetch_add(&context->destroyed, 1) == 0);
}

static rumqttc_status_t complete(completion_race *race) {
  if (race->context->kind == STORE)
    return rumqttc_callback_store_load_complete(race->completion, RUMQTTC_STORE_NOT_FOUND, native_bytes(NULL, 0));
  if (race->context->kind == AUTH) {
    rumqttc_auth_response_t response = RUMQTTC_AUTH_RESPONSE_INIT;
    response.action = RUMQTTC_AUTH_ACTION_SEND;
    response.method_present = 1;
    response.method = native_string("custom");
    return rumqttc_callback_auth_complete(race->completion, &response);
  }
  rumqttc_srv_record_t record = RUMQTTC_SRV_RECORD_INIT;
  record.target = native_string("localhost");
  record.port = native_test_port();
  return rumqttc_callback_srv_complete(race->completion, RUMQTTC_SRV_SUCCESS, &record, 1);
}

static int finish_completion(void *argument) {
  completion_race *race = argument;
  atomic_store(&race->ready, 1);
  while (!atomic_load(&race->release))
    native_sleep_ms(1);
  race->result = complete(race);
  REQUIRE(race->result == RUMQTTC_OK || race->result == RUMQTTC_INVALID_STATE);
  return 0;
}

static void attach_owner(rumqttc_config_t *config, callback_context *context) {
  if (context->kind == STORE) {
    rumqttc_store_vtable_t vtable = RUMQTTC_STORE_VTABLE_INIT;
    rumqttc_store_registration_t *registration = NULL;
    vtable.load = store_load;
    vtable.save = store_write;
    vtable.clear = store_write;
    vtable.destroy = destroy_owner;
    CHECK(rumqttc_store_registration_new(&vtable, context, &registration, NULL));
    CHECK(rumqttc_config_set_session_store(config, registration, native_string("race-scope"), 5000, 16u * 1024u * 1024u,
                                           NULL));
    rumqttc_store_registration_destroy(registration);
  } else if (context->kind == AUTH) {
    rumqttc_auth_vtable_t vtable = RUMQTTC_AUTH_VTABLE_INIT;
    rumqttc_auth_registration_t *registration = NULL;
    vtable.respond = auth_respond;
    vtable.destroy = destroy_owner;
    CHECK(rumqttc_auth_registration_new(&vtable, context, &registration, NULL));
    CHECK(rumqttc_config_set_v5_authenticator(config, registration, native_string("custom"), 5000, NULL));
    rumqttc_auth_registration_destroy(registration);
  } else {
    rumqttc_resolver_vtable_t vtable = RUMQTTC_RESOLVER_VTABLE_INIT;
    rumqttc_resolver_registration_t *registration = NULL;
    vtable.resolve = resolve;
    vtable.destroy = destroy_owner;
    CHECK(rumqttc_resolver_registration_new(&vtable, context, &registration, NULL));
    CHECK(rumqttc_config_set_v5_srv_resolver(config, registration, NULL));
    rumqttc_resolver_registration_destroy(registration);
  }
}

static void run_case(enum callback_kind kind, unsigned iteration) {
  callback_context original = {.kind = kind};
  callback_context replacement = {.kind = kind};
  completion_race race = {.context = &original};
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(
      config, native_string(kind == RESOLVER ? "native-v5-srv-cancel" : "native-callback-race"), NULL));
  if (kind == STORE)
    CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  if (kind == RESOLVER)
    CHECK(rumqttc_config_set_v5_redirect_policy(config, RUMQTTC_REDIRECT_FOLLOW, 2, RUMQTTC_REDIRECT_TRANSPORT_TCP,
                                                NULL, NULL));
  attach_owner(config, &original);
  CHECK(rumqttc_client_start(config, &client, NULL));
  for (unsigned attempt = 0; attempt < 500 && !atomic_load(&original.pending); ++attempt)
    native_sleep_ms(1);
  race.completion = (rumqttc_callback_completion_t *)atomic_exchange(&original.pending, 0);
  REQUIRE(race.completion != NULL);
  attach_owner(config, &replacement);
  REQUIRE(atomic_load(&original.destroyed) == 0);
  if (kind == STORE && iteration == 0) {
    // A replacement registration has its own store identity, even for the same key.
    rumqttc_client_t *replacement_client = NULL;
    CHECK(rumqttc_client_start(config, &replacement_client, NULL));
    for (unsigned attempt = 0; attempt < 500 && !atomic_load(&replacement.pending); ++attempt)
      native_sleep_ms(1);
    rumqttc_callback_completion_t *replacement_completion =
        (rumqttc_callback_completion_t *)atomic_exchange(&replacement.pending, 0);
    REQUIRE(replacement_completion != NULL);
    CHECK(rumqttc_client_destroy_timeout_ms(replacement_client, NATIVE_DEADLINE_MS, NULL));
    REQUIRE(rumqttc_callback_store_load_complete(replacement_completion, RUMQTTC_STORE_NOT_FOUND,
                                                 native_bytes(NULL, 0)) == RUMQTTC_INVALID_STATE);
    rumqttc_callback_completion_destroy(replacement_completion);
    REQUIRE(atomic_load(&replacement.destroyed) == 0);
  }
  native_thread_t *worker = native_thread_start(finish_completion, &race);
  for (unsigned attempt = 0; attempt < 500 && !atomic_load(&race.ready); ++attempt)
    native_sleep_ms(1);
  REQUIRE(atomic_load(&race.ready));
  if (iteration == 0) {
    // Force cancellation to win once; other iterations start both concurrently.
    CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
    atomic_store(&race.release, 1);
  } else {
    atomic_store(&race.release, 1);
    CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  }
  REQUIRE(native_thread_join(worker) == 0);
  if (iteration == 0)
    REQUIRE(race.result == RUMQTTC_INVALID_STATE);
  REQUIRE(complete(&race) == RUMQTTC_INVALID_STATE);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  rumqttc_config_destroy(config);
  REQUIRE(atomic_load(&replacement.destroyed) == 1);
  REQUIRE(atomic_load(&original.destroyed) == 0);
  rumqttc_callback_completion_destroy(race.completion);
  REQUIRE(atomic_load(&original.destroyed) == 1);
}

static void failed_start(enum callback_kind kind) {
  uint64_t capabilities = rumqttc_library_capabilities();
  if (!(capabilities & (RUMQTTC_CAP_RUSTLS | RUMQTTC_CAP_NATIVE_TLS)))
    return;
  callback_context context = {.kind = kind};
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = (rumqttc_client_t *)(uintptr_t)1;
  rumqttc_error_t *error = NULL;
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-callback-failed-start"), NULL));
  if (kind == STORE)
    CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  attach_owner(config, &context);
  tls.backend = capabilities & RUMQTTC_CAP_RUSTLS ? RUMQTTC_TLS_BACKEND_RUSTLS : RUMQTTC_TLS_BACKEND_NATIVE;
  tls.root_policy = RUMQTTC_TLS_ROOTS_PEM;
  tls.ca_pem = native_bytes((const uint8_t *)"invalid-root", 12);
  CHECK(rumqttc_config_set_transport_tls_with_options(config, &tls, NULL));
  REQUIRE(rumqttc_client_start(config, &client, &error) != RUMQTTC_OK);
  REQUIRE(client == NULL && error != NULL);
  REQUIRE(atomic_load(&context.pending) == 0 && atomic_load(&context.destroyed) == 0);
  rumqttc_error_destroy(error);
  rumqttc_config_destroy(config);
  REQUIRE(atomic_load(&context.destroyed) == 1);
}

int main(void) {
  for (enum callback_kind kind = STORE; kind <= RESOLVER; ++kind) {
    failed_start(kind);
    for (unsigned iteration = 0; iteration < 24; ++iteration)
      run_case(kind, iteration);
  }
  return 0;
}
