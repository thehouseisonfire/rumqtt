#include "native_common.h"
#include "external_signer.h"
#include "transport_socket.h"
#include <openssl/pem.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
  EVP_PKEY *key;
  atomic_uint selects, signs, verifies, destroyed;
  uint32_t failure_stage, failure_reason, expected_layer, blocked_stage;
  int invalid_signature, reject_first;
  atomic_uintptr_t client;
  atomic_uint reentrant;
  atomic_uint blocked, entered, release, cancelled;
  atomic_uint worker_count;
  native_thread_t *workers[128];
} host_t;
static int deferred_hooks;
static uint32_t select_identity(void *data, const rumqttc_tls_identity_request_t *request, size_t *index) {
  host_t *host = data;
  REQUIRE(request->layer == host->expected_layer && request->remaining_ns != 0);
  atomic_fetch_add(&host->selects, 1);
  if (host->failure_reason && host->failure_stage == RUMQTTC_TLS_STAGE_SELECT) return host->failure_reason;
  *index = 0;
  return RUMQTTC_TLS_CALLBACK_OK;
}
static uint32_t sign_message(void *data, const rumqttc_tls_signing_request_t *request,
                             uint8_t *output, size_t capacity, size_t *written) {
  host_t *host = data;
  REQUIRE(request->layer == host->expected_layer && request->identity_index == 0);
  REQUIRE(request->key_id.len == 8 && memcmp(request->key_id.data, "host-key", 8) == 0);
  atomic_fetch_add(&host->signs, 1);
  if (host->failure_reason && host->failure_stage == RUMQTTC_TLS_STAGE_SIGN) return host->failure_reason;
  uint32_t result = rumqttc_example_evp_sign(host->key, request, output, capacity, written);
  if (host->invalid_signature && result == 0 && *written != 0) output[0] ^= 1;
  return result;
}
static uint32_t verify_peer(void *data, const rumqttc_tls_verification_request_t *request) {
  host_t *host = data;
  REQUIRE(request->layer == host->expected_layer && request->certificate_count > 0);
  REQUIRE(request->server_name.len == 9 && memcmp(request->server_name.data, "localhost", 9) == 0);
  unsigned previous = atomic_fetch_add(&host->verifies, 1);
  if (host->reject_first && previous == 0) return RUMQTTC_TLS_CALLBACK_REJECTED;
  if (atomic_load(&host->blocked) && (!deferred_hooks || host->blocked_stage == RUMQTTC_TLS_STAGE_VERIFY)) {
    atomic_store(&host->entered, 1);
    unsigned waited = 0;
    while (!atomic_load(&host->release) && waited++ < 5000) native_sleep_ms(1);
    REQUIRE(atomic_load(&host->release));
  }
  rumqttc_client_t *client = (rumqttc_client_t *)atomic_load(&host->client);
  if (client != NULL) {
    rumqttc_publish_options_t options = RUMQTTC_PUBLISH_OPTIONS_INIT;
    uint64_t operation = 0;
    uint32_t result = rumqttc_client_try_publish(client, native_string("external/reentrant"), native_bytes(NULL, 0), &options, &operation, NULL);
    REQUIRE(result == RUMQTTC_OK || result == RUMQTTC_BACKPRESSURE);
    atomic_fetch_add(&host->reentrant, 1);
  }
  if (host->failure_reason && host->failure_stage == RUMQTTC_TLS_STAGE_VERIFY) return host->failure_reason;
  return RUMQTTC_TLS_CALLBACK_OK;
}
static void destroy(void *data) { host_t *host = data; atomic_fetch_add(&host->destroyed, 1); }

typedef struct {
  host_t *host;
  uint32_t stage;
  uint64_t id;
  rumqttc_callback_completion_t *token;
  rumqttc_tls_verification_request_t verify;
  rumqttc_tls_identity_request_t select;
  rumqttc_tls_signing_request_t sign;
  rumqttc_bytes_view_t views[32];
  uint16_t schemes[64];
  void *copies[70];
  size_t copy_count;
} work_t;
static rumqttc_bytes_view_t copy_bytes(work_t *work, rumqttc_bytes_view_t input) {
  if (input.len == 0) return native_bytes(NULL, 0);
  REQUIRE(work->copy_count < 70);
  void *copy = malloc(input.len); REQUIRE(copy); memcpy(copy, input.data, input.len);
  work->copies[work->copy_count++] = copy;
  return native_bytes(copy, input.len);
}
static rumqttc_string_view_t copy_name(work_t *work, rumqttc_string_view_t input) {
  rumqttc_bytes_view_t copied = copy_bytes(work, native_bytes((const uint8_t *)input.data, input.len));
  rumqttc_string_view_t result = { (const char *)copied.data, copied.len }; return result;
}
static int complete_work(void *data) {
  work_t *work = data; uint32_t reason = 0, status = 0;
  native_sleep_ms(10);
  if (atomic_load(&work->host->blocked) && work->stage == work->host->blocked_stage) {
      atomic_store(&work->host->entered, 1);
      unsigned waited = 0;
      while (!atomic_load(&work->host->cancelled) && waited++ < 5000) native_sleep_ms(1);
      REQUIRE(atomic_load(&work->host->cancelled));
      /* Work has stopped. Retain its cancelled token until the host releases
       * this barrier, proving destruction cannot precede late-token cleanup. */
      waited = 0;
      while (!atomic_load(&work->host->release) && waited++ < 5000) native_sleep_ms(1);
      REQUIRE(atomic_load(&work->host->release));
  }
  if (work->stage == RUMQTTC_TLS_STAGE_VERIFY) {
    reason = atomic_load(&work->host->cancelled) ? 0 : verify_peer(work->host, &work->verify);
    status = rumqttc_callback_tls_verify_complete(work->token, reason);
    REQUIRE(rumqttc_callback_tls_verify_complete(work->token, reason) == RUMQTTC_INVALID_STATE);
  } else if (work->stage == RUMQTTC_TLS_STAGE_SELECT) {
    size_t index = SIZE_MAX; reason = select_identity(work->host, &work->select, &index);
    if (!atomic_load(&work->host->cancelled)) REQUIRE(rumqttc_callback_tls_verify_complete(work->token, 0) == RUMQTTC_INVALID_ARGUMENT);
    status = rumqttc_callback_tls_select_complete(work->token, reason, index);
    REQUIRE(rumqttc_callback_tls_select_complete(work->token, reason, index) == RUMQTTC_INVALID_STATE);
  } else {
    uint8_t signature[4096]; size_t written = 0;
    reason = sign_message(work->host, &work->sign, signature, sizeof(signature), &written);
    if (!atomic_load(&work->host->cancelled)) REQUIRE(rumqttc_callback_tls_sign_complete(work->token, 0, native_bytes(NULL, 0)) == RUMQTTC_INVALID_ARGUMENT);
    status = rumqttc_callback_tls_sign_complete(work->token, reason, native_bytes(signature, written));
    memset(signature, 0, sizeof(signature));
    REQUIRE(rumqttc_callback_tls_sign_complete(work->token, 0, native_bytes((const uint8_t *)(uintptr_t)1, 1)) == RUMQTTC_INVALID_STATE);
  }
  REQUIRE(status == RUMQTTC_OK || (atomic_load(&work->host->cancelled) && status == RUMQTTC_INVALID_STATE));
  rumqttc_callback_completion_destroy(work->token);
  for (size_t i = 0; i < work->copy_count; ++i) free(work->copies[i]);
  free(work); return 0;
}
static work_t *new_work(host_t *host, uint64_t id, uint32_t stage, rumqttc_callback_completion_t *token) {
  work_t *work = calloc(1, sizeof(*work)); REQUIRE(work); work->host = host; work->id = id; work->stage = stage;
  CHECK(rumqttc_callback_completion_retain(token, &work->token)); return work;
}
static void start_work(work_t *work) {
  host_t *host = work->host;
  unsigned slot = atomic_fetch_add(&host->worker_count, 1); REQUIRE(slot < 128);
  host->workers[slot] = native_thread_start(complete_work, work); REQUIRE(host->workers[slot]);
}
static void verify_async(void *data, uint64_t id, const rumqttc_tls_verification_request_t *request, rumqttc_callback_completion_t *token) {
  work_t *work = new_work(data, id, RUMQTTC_TLS_STAGE_VERIFY, token); work->verify = *request;
  work->verify.server_name = copy_name(work, request->server_name); REQUIRE(request->certificate_count <= 32);
  for (size_t i = 0; i < request->certificate_count; ++i) work->views[i] = copy_bytes(work, request->certificates[i]);
  work->verify.certificates = work->views; work->verify.ocsp_response = copy_bytes(work, request->ocsp_response);
  start_work(work);
}
static void select_async(void *data, uint64_t id, const rumqttc_tls_identity_request_t *request, rumqttc_callback_completion_t *token) {
  work_t *work = new_work(data, id, RUMQTTC_TLS_STAGE_SELECT, token); work->select = *request;
  work->select.server_name = copy_name(work, request->server_name); REQUIRE(request->issuer_hint_count <= 32 && request->signature_scheme_count <= 64);
  for (size_t i = 0; i < request->issuer_hint_count; ++i) work->views[i] = copy_bytes(work, request->issuer_hints[i]);
  work->select.issuer_hints = work->views;
  memcpy(work->schemes, request->signature_schemes, request->signature_scheme_count * sizeof(uint16_t)); work->select.signature_schemes = work->schemes;
  start_work(work);
}
static void sign_async(void *data, uint64_t id, const rumqttc_tls_signing_request_t *request, rumqttc_callback_completion_t *token) {
  work_t *work = new_work(data, id, RUMQTTC_TLS_STAGE_SIGN, token); work->sign = *request;
  work->sign.server_name = copy_name(work, request->server_name); work->sign.key_id = copy_bytes(work, request->key_id);
  work->sign.message = copy_bytes(work, request->message); start_work(work);
}
static void cancel_async(void *data, uint64_t id) { host_t *host = data; REQUIRE(id > 0); atomic_fetch_add(&host->cancelled, 1); }
static void join_work(host_t *host) {
  for (unsigned i = 0; i < atomic_load(&host->worker_count); ++i) REQUIRE(native_thread_join(host->workers[i]) == 0);
}
static uint16_t port_env(const char *name) { const char *value = getenv(name); REQUIRE(value); return (uint16_t)strtoul(value, NULL, 10); }
static rumqttc_tls_profile_t *profile(host_t *host, int ec, int proxy) {
  rumqttc_tls_verifier_vtable_t verify = RUMQTTC_TLS_VERIFIER_VTABLE_INIT;
  rumqttc_tls_identity_vtable_t identity = RUMQTTC_TLS_IDENTITY_VTABLE_INIT;
  rumqttc_tls_verifier_registration_t *verifier = NULL;
  rumqttc_tls_identity_registration_t *registration = NULL;
  verify.verify = verify_peer; verify.destroy = destroy;
  identity.select = select_identity; identity.sign = sign_message; identity.destroy = destroy;
  if (deferred_hooks) {
    rumqttc_tls_async_verifier_vtable_t async_verify = RUMQTTC_TLS_ASYNC_VERIFIER_VTABLE_INIT;
    async_verify.verify = verify_async; async_verify.cancel = cancel_async; async_verify.destroy = destroy;
    CHECK(rumqttc_tls_verifier_registration_new_async(&async_verify, host, &verifier, NULL));
  } else CHECK(rumqttc_tls_verifier_registration_new(&verify, host, &verifier, NULL));
  if (!proxy) {
    const char *pem = getenv(ec ? "RUMQTTC_TEST_EC_CLIENT_CERT_PEM" : "RUMQTTC_TEST_CLIENT_CERT_PEM"); REQUIRE(pem);
    char *copy = malloc(strlen(pem) + 1); REQUIRE(copy); memcpy(copy, pem, strlen(pem) + 1);
    uint16_t schemes[] = { ec ? 0x0403 : 0x0804 };
    rumqttc_tls_external_identity_t descriptor = RUMQTTC_TLS_EXTERNAL_IDENTITY_INIT;
    descriptor.certificate_pem = native_bytes((const uint8_t *)copy, strlen(copy));
    descriptor.key_id = native_bytes((const uint8_t *)"host-key", 8);
    descriptor.signature_schemes = schemes; descriptor.signature_scheme_count = 1;
    if (deferred_hooks) {
      rumqttc_tls_async_identity_vtable_t async_identity = RUMQTTC_TLS_ASYNC_IDENTITY_VTABLE_INIT;
      async_identity.select = select_async; async_identity.sign = sign_async; async_identity.cancel = cancel_async; async_identity.destroy = destroy;
      CHECK(rumqttc_tls_identity_registration_new_async(&async_identity, host, &descriptor, 1, &registration, NULL));
    } else CHECK(rumqttc_tls_identity_registration_new(&identity, host, &descriptor, 1, &registration, NULL));
    memset(copy, 'x', strlen(copy)); free(copy); schemes[0] = 0xffff;
  }
  const char *ca = getenv(proxy ? "RUMQTTC_TEST_PROXY_CA_PEM" : "RUMQTTC_TEST_CA_PEM"); REQUIRE(ca);
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  tls.root_policy = RUMQTTC_TLS_ROOTS_PEM; tls.ca_pem = native_bytes((const uint8_t *)ca, strlen(ca));
  rumqttc_tls_profile_options_t options = RUMQTTC_TLS_PROFILE_OPTIONS_INIT;
  options.tls = &tls; options.version_policy = RUMQTTC_TLS_VERSION_13_ONLY;
  rumqttc_tls_profile_extensions_t extensions = RUMQTTC_TLS_PROFILE_EXTENSIONS_INIT;
  extensions.verifier = verifier; extensions.external_identity = registration;
  extensions.resumption_policy = RUMQTTC_TLS_RESUMPTION_DISABLED;
  uint16_t cipher = 0x1301; extensions.cipher_suites = &cipher; extensions.cipher_suite_count = 1;
  rumqttc_tls_profile_t *result = NULL;
  CHECK(rumqttc_tls_profile_new_with_extensions(&options, &extensions, &result, NULL));
  rumqttc_tls_verifier_registration_destroy(verifier); rumqttc_tls_identity_registration_destroy(registration);
  REQUIRE(atomic_load(&host->destroyed) == 0);
  return result;
}
static void run_case(uint32_t protocol, int ec, int wss, int failure, int via) {
  host_t host = {0}; host_t proxy_host = {0};
  host.expected_layer = via == 2 ? RUMQTTC_TLS_LAYER_REDIRECT : RUMQTTC_TLS_LAYER_BROKER;
  proxy_host.expected_layer = RUMQTTC_TLS_LAYER_PROXY;
  if (via == 4) { atomic_store(&host.blocked, 1); host.blocked_stage = deferred_hooks ? (uint32_t)failure : RUMQTTC_TLS_STAGE_VERIFY; }
  if (failure == 1 && via != 4) { host.failure_stage = RUMQTTC_TLS_STAGE_VERIFY; host.failure_reason = RUMQTTC_TLS_CALLBACK_REJECTED; }
  if (failure == 2 && via != 4) { host.failure_stage = RUMQTTC_TLS_STAGE_SELECT; host.failure_reason = RUMQTTC_TLS_CALLBACK_FAILED; }
  if (failure == 3 && via != 4) host.invalid_signature = 1;
  if (failure == 4) { proxy_host.failure_stage = RUMQTTC_TLS_STAGE_VERIFY; proxy_host.failure_reason = RUMQTTC_TLS_CALLBACK_REJECTED; }
  const char *key_pem = getenv(ec ? "RUMQTTC_TEST_EC_CLIENT_KEY_PEM" : "RUMQTTC_TEST_CLIENT_KEY_PEM"); REQUIRE(key_pem);
  BIO *input = BIO_new_mem_buf(key_pem, -1); REQUIRE(input);
  host.key = PEM_read_bio_PrivateKey(input, NULL, NULL, NULL); BIO_free(input); REQUIRE(host.key);
  rumqttc_tls_profile_t *tls = profile(&host, ec, 0);
  rumqttc_config_t *config = NULL; rumqttc_client_t *client = NULL;
  CHECK(rumqttc_config_new(protocol, &config, NULL));
  uint16_t port = port_env(wss ? "RUMQTTC_TEST_MTLS_WSS_PORT" : "RUMQTTC_TEST_MTLS_PORT");
  CHECK(rumqttc_config_set_broker(config, native_string("localhost"), port, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-external-identity"), NULL));
  if (via == 4) {
    CHECK(rumqttc_config_set_client_id(config, native_string("native-external-cancel-pending"), NULL));
  }
  if (via == 3) {
    char id[128]; REQUIRE(snprintf(id, sizeof(id), "native-external-reconnect-%u-%d-%d", protocol, ec, wss) > 0);
    CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
  }
  if (via == 2) {
    CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
    CHECK(rumqttc_config_set_client_id(config, native_string(wss ? "native-redirect-matrix-connack-wss" : "native-redirect-matrix-connack-mqtts"), NULL));
    CHECK(rumqttc_config_set_v5_redirect_policy_with_tls_profile(config, 2, wss ? RUMQTTC_REDIRECT_TRANSPORT_WSS : RUMQTTC_REDIRECT_TRANSPORT_TLS, tls, NULL));
  } else if (wss) {
    char url[128]; REQUIRE(snprintf(url, sizeof(url), "wss://localhost:%u/mqtt", port) > 0);
    CHECK(rumqttc_config_set_transport_wss_with_profile(config, native_string(url), tls, NULL));
  } else CHECK(rumqttc_config_set_transport_tls_with_profile(config, tls, NULL));
  if (via == 1) {
    rumqttc_tls_profile_t *proxy_tls = profile(&proxy_host, ec, 1);
    rumqttc_proxy_options_t proxy = RUMQTTC_PROXY_OPTIONS_INIT;
    proxy.protocol = RUMQTTC_PROXY_HTTPS; proxy.host = native_string("localhost"); proxy.port = port_env("RUMQTTC_TEST_TLS_TUNNEL_PORT");
    proxy.credentials_present = 1; proxy.username = native_bytes((const uint8_t *)"proxy-private-user", 18); proxy.password = native_bytes((const uint8_t *)"proxy-private-password", 22);
    CHECK(rumqttc_config_set_proxy_with_tls_profile(config, &proxy, proxy_tls, NULL)); rumqttc_tls_profile_destroy(proxy_tls);
  }
  socket_transport_t *socket_host = NULL;
  if (via == 5) {
    rumqttc_transport_registration_t *socket_registration = NULL;
    socket_host = socket_transport_new(0, &socket_registration); REQUIRE(socket_host && socket_registration);
    CHECK(rumqttc_config_set_transport_connector(config, socket_registration, NULL));
    rumqttc_transport_registration_destroy(socket_registration);
  }
  rumqttc_tls_profile_destroy(tls);
  CHECK(rumqttc_client_start(config, &client, NULL)); rumqttc_config_destroy(config);
  if (via == 3) atomic_store(&host.client, (uintptr_t)client);
  if (via == 4) {
    unsigned waited = 0;
    while (!atomic_load(&host.entered) && waited++ < 5000) native_sleep_ms(1);
    REQUIRE(atomic_load(&host.entered));
    if (deferred_hooks) {
      CHECK(rumqttc_client_close_now_timeout_ms(client, 1000, NULL));
      CHECK(rumqttc_client_destroy_timeout_ms(client, 1000, NULL)); client = NULL;
      REQUIRE(atomic_load(&host.cancelled) == 1);
    } else {
      REQUIRE(rumqttc_client_close_now_timeout_ms(client, 10, NULL) == RUMQTTC_TIMEOUT);
      REQUIRE(rumqttc_client_destroy_timeout_ms(client, 10, NULL) == RUMQTTC_TIMEOUT);
    }
    REQUIRE(atomic_load(&host.destroyed) == 0);
    atomic_store(&host.release, 1);
  } else if (!failure) {
    rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED); rumqttc_event_destroy(event);
    if (via == 3) {
      event = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED); rumqttc_event_destroy(event);
      event = native_wait_event(client, RUMQTTC_EVENT_CONNECTED); rumqttc_event_destroy(event);
      REQUIRE(atomic_load(&host.selects) == 2 && atomic_load(&host.signs) == 2 && atomic_load(&host.verifies) == 2);
      REQUIRE(atomic_load(&host.reentrant) >= 1);
    }
  } else {
    rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
    rumqttc_error_t *error = NULL; CHECK(rumqttc_event_disconnected(event, NULL, &error));
    uint8_t present = 0; uint32_t stage = 99, reason = 0, layer = 99;
    CHECK(rumqttc_error_tls_callback_failure(error, &present, &stage, &reason, &layer));
    host_t *failed_host = failure == 4 ? &proxy_host : &host;
    REQUIRE(present && layer == failed_host->expected_layer);
    REQUIRE(stage == (failure == 3 ? RUMQTTC_TLS_STAGE_SIGN : failed_host->failure_stage));
    REQUIRE(reason == (failure == 3 ? RUMQTTC_TLS_CALLBACK_INVALID_SIGNATURE : failed_host->failure_reason));
    rumqttc_error_destroy(error); rumqttc_event_destroy(event);
  }
  if (client != NULL) native_close_destroy(client);
  join_work(&host); join_work(&proxy_host);
  if (socket_host != NULL) REQUIRE(socket_transport_join_destroy(socket_host));
  REQUIRE(atomic_load(&host.destroyed) == 2);
  if (!failure && via != 4) REQUIRE(atomic_load(&host.selects) >= 1 && atomic_load(&host.signs) >= 1);
  if (via == 1) REQUIRE(atomic_load(&proxy_host.destroyed) == 1 && atomic_load(&proxy_host.verifies) >= 1);
  EVP_PKEY_free(host.key);
}
static void shared_profile_concurrency(void) {
  host_t host = {0}; host.expected_layer = RUMQTTC_TLS_LAYER_BROKER; host.reject_first = 1;
  BIO *input = BIO_new_mem_buf(getenv("RUMQTTC_TEST_CLIENT_KEY_PEM"), -1); REQUIRE(input);
  host.key = PEM_read_bio_PrivateKey(input, NULL, NULL, NULL); BIO_free(input); REQUIRE(host.key);
  rumqttc_tls_profile_t *tls = profile(&host, 0, 0);
  rumqttc_config_t *config = NULL; rumqttc_client_t *clients[2] = {NULL, NULL};
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("localhost"), port_env("RUMQTTC_TEST_MTLS_PORT"), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-shared-tls-profile"), NULL));
  CHECK(rumqttc_config_set_transport_tls_with_profile(config, tls, NULL));
  CHECK(rumqttc_client_start(config, &clients[0], NULL)); CHECK(rumqttc_client_start(config, &clients[1], NULL));
  rumqttc_config_destroy(config); rumqttc_tls_profile_destroy(tls);
  unsigned failures = 0; size_t failed_index = SIZE_MAX;
  for (size_t i = 0; i < 2; ++i) {
    for (;;) {
      rumqttc_event_t *event = NULL; uint32_t kind = 0;
      CHECK(rumqttc_client_event_recv_timeout_ms(clients[i], 5000, &event, NULL)); CHECK(rumqttc_event_kind(event, &kind));
      if (kind == RUMQTTC_EVENT_DRIVER_TERMINATED) {
        rumqttc_error_t *error = NULL; uint8_t present = 0; uint32_t reason = 0;
        CHECK(rumqttc_event_disconnected(event, NULL, &error));
        CHECK(rumqttc_error_tls_callback_failure(error, &present, NULL, &reason, NULL));
        REQUIRE(present && reason == RUMQTTC_TLS_CALLBACK_REJECTED); rumqttc_error_destroy(error);
        failures++; failed_index = i;
      }
      rumqttc_event_destroy(event);
      if (kind == RUMQTTC_EVENT_CONNECTED || kind == RUMQTTC_EVENT_DRIVER_TERMINATED) break;
    }
  }
  REQUIRE(failures == 1 && atomic_load(&host.verifies) == 2);
  REQUIRE(atomic_load(&host.selects) == 2 && atomic_load(&host.signs) == 1);
  native_close_destroy(clients[failed_index]); REQUIRE(atomic_load(&host.destroyed) == 0);
  native_close_destroy(clients[1 - failed_index]); join_work(&host); REQUIRE(atomic_load(&host.destroyed) == 2);
  EVP_PKEY_free(host.key);
}
int main(int argc, char **argv) {
  deferred_hooks = argc == 2 && strcmp(argv[1], "deferred") == 0;
  const uint64_t capabilities = rumqttc_library_capabilities();
  if (!(capabilities & RUMQTTC_CAP_RUSTLS)) return 77;
  rumqttc_tls_advanced_capabilities_t caps = RUMQTTC_TLS_ADVANCED_CAPABILITIES_INIT;
  CHECK(rumqttc_tls_advanced_capabilities(0, &caps, NULL)); REQUIRE(caps.feature_mask == 31 && caps.max_signature_bytes == 4096);
  uint16_t suites[64]; size_t count = 0; CHECK(rumqttc_tls_supported_cipher_suites(0, suites, 64, &count, NULL)); REQUIRE(count > 0);
  for (uint32_t protocol = 1; protocol <= 2; ++protocol)
    for (int ec = 0; ec <= 1; ++ec)
      for (int wss = 0; wss <= !!(capabilities & RUMQTTC_CAP_WEBSOCKET); ++wss) {
        for (int failure = 0; failure <= 3; ++failure) run_case(protocol, ec, wss, failure, 0);
        if (capabilities & RUMQTTC_CAP_HTTP_PROXY) {
          run_case(protocol, ec, wss, 0, 1); run_case(protocol, ec, wss, 4, 1);
        }
        run_case(protocol, ec, wss, 0, 3);
        if (deferred_hooks) {
          for (int stage = 0; stage <= 2; ++stage) run_case(protocol, ec, wss, stage, 4);
          run_case(protocol, ec, wss, 0, 5);
        } else run_case(protocol, ec, wss, 0, 4);
        if (protocol == 2) for (int failure = 0; failure <= 3; ++failure) run_case(protocol, ec, wss, failure, 2);
      }
  shared_profile_concurrency();
  puts(capabilities & RUMQTTC_CAP_WEBSOCKET
       ? "native external RSA-PSS/ECDSA TLS and WSS passed"
       : "native external RSA-PSS/ECDSA TLS passed"); return 0;
}
