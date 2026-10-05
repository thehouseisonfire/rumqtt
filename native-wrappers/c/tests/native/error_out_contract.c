#include "native_common.h"

#include <string.h>
#include <stdlib.h>

#define EXPECT_FAILURE(expression) REQUIRE((expression) != RUMQTTC_OK)

static void coverage_store_callback(void *user_data,
                                    const rumqttc_store_request_t *request,
                                    rumqttc_callback_completion_t *completion) {
  (void)user_data;
  (void)request;
  (void)completion;
}

static void coverage_resolver_callback(void *user_data,
                                       const rumqttc_resolver_request_t *request,
                                       rumqttc_callback_completion_t *completion) {
  (void)user_data;
  (void)request;
  (void)completion;
}

static void coverage_auth_callback(void *user_data,
                                   const rumqttc_auth_request_t *request,
                                   rumqttc_callback_completion_t *completion) {
  (void)user_data;
  (void)request;
  (void)completion;
}

static void coverage_websocket(void *data, const rumqttc_websocket_request_t *request, rumqttc_callback_completion_t *completion) {
  (void)data; (void)request; CHECK(rumqttc_callback_websocket_reject(completion));
}

static void coverage_destroy(void *user_data) { (void)user_data; }

static void coverage_transport_cancel(void *data, uint64_t id) { (void)data; (void)id; }
static void coverage_transport_io(void *data, const rumqttc_transport_io_request_t *request,
                                  rumqttc_callback_completion_t *completion) {
  (void)data; (void)request; (void)completion;
}
static void coverage_transport_connect(void *data, const rumqttc_transport_connect_request_t *request,
                                       rumqttc_callback_completion_t *completion) {
  (void)request;
  rumqttc_transport_stream_vtable_t table = RUMQTTC_TRANSPORT_STREAM_VTABLE_INIT;
  table.perform = coverage_transport_io; table.cancel = coverage_transport_cancel; table.destroy = coverage_destroy;
  rumqttc_transport_stream_t *stream = NULL;
  /* ERROR_OUT_SUCCESS: rumqttc_transport_stream_new */
  CHECK(rumqttc_transport_stream_new(completion, &table, NULL, &stream, NULL));
  rumqttc_transport_stream_destroy(stream);
  *(int *)data = 1;
  rumqttc_transport_response_t response = RUMQTTC_TRANSPORT_RESPONSE_INIT;
  response.result = RUMQTTC_TRANSPORT_FAILURE_NETWORK_OPTIONS;
  CHECK(rumqttc_callback_transport_complete(completion, &response));
}

static uint32_t coverage_tls_verify(void *data, const rumqttc_tls_verification_request_t *request) {
  (void)data; (void)request; return RUMQTTC_TLS_CALLBACK_OK;
}
static uint32_t coverage_tls_select(void *data, const rumqttc_tls_identity_request_t *request, size_t *index) {
  (void)data; (void)request; *index = RUMQTTC_TLS_IDENTITY_DECLINE; return RUMQTTC_TLS_CALLBACK_OK;
}
static uint32_t coverage_tls_sign(void *data, const rumqttc_tls_signing_request_t *request, uint8_t *buffer, size_t capacity, size_t *written) {
  (void)data; (void)request; (void)buffer; (void)capacity; *written = 0; return RUMQTTC_TLS_CALLBACK_FAILED;
}
static void coverage_tls_verify_async(void *data, uint64_t id, const rumqttc_tls_verification_request_t *request, rumqttc_callback_completion_t *token) {
  (void)data; (void)id; (void)request; CHECK(rumqttc_callback_tls_verify_complete(token, 0));
}
static void coverage_tls_select_async(void *data, uint64_t id, const rumqttc_tls_identity_request_t *request, rumqttc_callback_completion_t *token) {
  (void)data; (void)id; (void)request; CHECK(rumqttc_callback_tls_select_complete(token, 0, SIZE_MAX));
}
static void coverage_tls_sign_async(void *data, uint64_t id, const rumqttc_tls_signing_request_t *request, rumqttc_callback_completion_t *token) {
  (void)data; (void)id; (void)request; CHECK(rumqttc_callback_tls_sign_complete(token, 2, native_bytes(NULL, 0)));
}
static void coverage_tls_cancel(void *data, uint64_t id) { (void)data; (void)id; }
static void coverage_tls_profiles(void) {
  rumqttc_tls_backend_capabilities_t caps = RUMQTTC_TLS_BACKEND_CAPABILITIES_INIT;
  rumqttc_tls_advanced_capabilities_t advanced = RUMQTTC_TLS_ADVANCED_CAPABILITIES_INIT;
  /* ERROR_OUT_SUCCESS: rumqttc_tls_backend_capabilities */
  CHECK(rumqttc_tls_backend_capabilities(0, &caps, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_tls_backend_capabilities */
  EXPECT_FAILURE(rumqttc_tls_backend_capabilities(99, &caps, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_tls_advanced_capabilities */
  CHECK(rumqttc_tls_advanced_capabilities(0, &advanced, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_tls_advanced_capabilities */
  EXPECT_FAILURE(rumqttc_tls_advanced_capabilities(99, &advanced, NULL));
  uint64_t features = rumqttc_library_capabilities();
  if (!(features & (RUMQTTC_CAP_RUSTLS | RUMQTTC_CAP_NATIVE_TLS))) return;
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  tls.backend = features & RUMQTTC_CAP_RUSTLS ? RUMQTTC_TLS_BACKEND_RUSTLS : RUMQTTC_TLS_BACKEND_NATIVE;
  const char *ca = getenv("RUMQTTC_TEST_CA_PEM"); REQUIRE(ca);
  tls.root_policy = RUMQTTC_TLS_ROOTS_PEM; tls.ca_pem = native_bytes((const uint8_t *)ca, strlen(ca));
  rumqttc_tls_profile_options_t options = RUMQTTC_TLS_PROFILE_OPTIONS_INIT; options.tls = &tls;
  rumqttc_tls_profile_t *profile = NULL;
  /* ERROR_OUT_FAILURE: rumqttc_tls_profile_new */
  EXPECT_FAILURE(rumqttc_tls_profile_new(NULL, &profile, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_tls_profile_new */
  CHECK(rumqttc_tls_profile_new(&options, &profile, NULL));
  rumqttc_tls_profile_destroy(profile); profile = NULL;
  rumqttc_tls_profile_extensions_t extensions = RUMQTTC_TLS_PROFILE_EXTENSIONS_INIT;
  rumqttc_tls_verifier_registration_t *verifier = NULL;
  rumqttc_tls_identity_registration_t *identity = NULL;
  if (features & RUMQTTC_CAP_RUSTLS) {
    size_t count = 0;
    /* ERROR_OUT_SUCCESS: rumqttc_tls_supported_cipher_suites */
    CHECK(rumqttc_tls_supported_cipher_suites(0, NULL, 0, &count, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_tls_supported_cipher_suites */
    EXPECT_FAILURE(rumqttc_tls_supported_cipher_suites(99, NULL, 0, &count, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_tls_supported_signature_schemes */
    CHECK(rumqttc_tls_supported_signature_schemes(0, NULL, 0, &count, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_tls_supported_signature_schemes */
    EXPECT_FAILURE(rumqttc_tls_supported_signature_schemes(99, NULL, 0, &count, NULL));
    rumqttc_tls_verifier_vtable_t verify = RUMQTTC_TLS_VERIFIER_VTABLE_INIT;
    verify.verify = coverage_tls_verify; verify.destroy = coverage_destroy;
    /* ERROR_OUT_FAILURE: rumqttc_tls_verifier_registration_new */
    EXPECT_FAILURE(rumqttc_tls_verifier_registration_new(NULL, NULL, &verifier, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_tls_verifier_registration_new */
    CHECK(rumqttc_tls_verifier_registration_new(&verify, NULL, &verifier, NULL));
    rumqttc_tls_identity_vtable_t table = RUMQTTC_TLS_IDENTITY_VTABLE_INIT;
    table.select = coverage_tls_select; table.sign = coverage_tls_sign; table.destroy = coverage_destroy;
    const char *pem = getenv("RUMQTTC_TEST_CLIENT_CERT_PEM"); REQUIRE(pem);
    uint16_t scheme = 0x0804;
    rumqttc_tls_external_identity_t descriptor = RUMQTTC_TLS_EXTERNAL_IDENTITY_INIT;
    descriptor.certificate_pem = native_bytes((const uint8_t *)pem, strlen(pem)); descriptor.key_id = native_bytes((const uint8_t *)"key", 3);
    descriptor.signature_schemes = &scheme; descriptor.signature_scheme_count = 1;
    /* ERROR_OUT_FAILURE: rumqttc_tls_identity_registration_new */
    EXPECT_FAILURE(rumqttc_tls_identity_registration_new(NULL, NULL, &descriptor, 1, &identity, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_tls_identity_registration_new */
    CHECK(rumqttc_tls_identity_registration_new(&table, NULL, &descriptor, 1, &identity, NULL));
    rumqttc_tls_async_verifier_vtable_t deferred_verify = RUMQTTC_TLS_ASYNC_VERIFIER_VTABLE_INIT;
    deferred_verify.verify = coverage_tls_verify_async; deferred_verify.cancel = coverage_tls_cancel; deferred_verify.destroy = coverage_destroy;
    rumqttc_tls_verifier_registration_t *deferred_verifier = NULL;
    /* ERROR_OUT_FAILURE: rumqttc_tls_verifier_registration_new_async */
    EXPECT_FAILURE(rumqttc_tls_verifier_registration_new_async(NULL, NULL, &deferred_verifier, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_tls_verifier_registration_new_async */
    CHECK(rumqttc_tls_verifier_registration_new_async(&deferred_verify, NULL, &deferred_verifier, NULL));
    rumqttc_tls_async_identity_vtable_t deferred_identity = RUMQTTC_TLS_ASYNC_IDENTITY_VTABLE_INIT;
    deferred_identity.select = coverage_tls_select_async; deferred_identity.sign = coverage_tls_sign_async;
    deferred_identity.cancel = coverage_tls_cancel; deferred_identity.destroy = coverage_destroy;
    rumqttc_tls_identity_registration_t *deferred_registration = NULL;
    /* ERROR_OUT_FAILURE: rumqttc_tls_identity_registration_new_async */
    EXPECT_FAILURE(rumqttc_tls_identity_registration_new_async(NULL, NULL, &descriptor, 1, &deferred_registration, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_tls_identity_registration_new_async */
    CHECK(rumqttc_tls_identity_registration_new_async(&deferred_identity, NULL, &descriptor, 1, &deferred_registration, NULL));
    rumqttc_tls_verifier_registration_destroy(deferred_verifier); rumqttc_tls_identity_registration_destroy(deferred_registration);
    extensions.verifier = verifier; extensions.external_identity = identity;
  }
  /* ERROR_OUT_FAILURE: rumqttc_tls_profile_new_with_extensions */
  EXPECT_FAILURE(rumqttc_tls_profile_new_with_extensions(&options, NULL, &profile, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_tls_profile_new_with_extensions */
  CHECK(rumqttc_tls_profile_new_with_extensions(&options, &extensions, &profile, NULL));
  rumqttc_tls_verifier_registration_destroy(verifier); rumqttc_tls_identity_registration_destroy(identity);
  rumqttc_config_t *v4 = NULL, *v5 = NULL;
  CHECK(rumqttc_config_new(1, &v4, NULL)); CHECK(rumqttc_config_new(2, &v5, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_tls_with_profile */
  CHECK(rumqttc_config_set_transport_tls_with_profile(v4, profile, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_tls_with_profile */
  EXPECT_FAILURE(rumqttc_config_set_transport_tls_with_profile(NULL, profile, NULL));
  if (features & RUMQTTC_CAP_WEBSOCKET) {
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_wss_with_profile */
    CHECK(rumqttc_config_set_transport_wss_with_profile(v4, native_string("wss://localhost"), profile, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_wss_with_profile */
    EXPECT_FAILURE(rumqttc_config_set_transport_wss_with_profile(NULL, native_string("wss://localhost"), profile, NULL));
  }
  if (features & RUMQTTC_CAP_HTTP_PROXY) {
    rumqttc_proxy_options_t proxy = RUMQTTC_PROXY_OPTIONS_INIT;
    proxy.protocol = RUMQTTC_PROXY_HTTPS; proxy.host = native_string("localhost"); proxy.port = 443;
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_proxy_with_tls_profile */
    CHECK(rumqttc_config_set_proxy_with_tls_profile(v4, &proxy, profile, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_proxy_with_tls_profile */
    EXPECT_FAILURE(rumqttc_config_set_proxy_with_tls_profile(NULL, &proxy, profile, NULL));
  }
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_redirect_policy_with_tls_profile */
  CHECK(rumqttc_config_set_v5_redirect_policy_with_tls_profile(v5, 2, RUMQTTC_REDIRECT_TRANSPORT_TLS, profile, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_redirect_policy_with_tls_profile */
  EXPECT_FAILURE(rumqttc_config_set_v5_redirect_policy_with_tls_profile(NULL, 2, RUMQTTC_REDIRECT_TRANSPORT_TLS, profile, NULL));
  rumqttc_tls_profile_destroy(profile); rumqttc_config_destroy(v4); rumqttc_config_destroy(v5);
}

/*
 * Keep calls explicit: check_error_out_coverage.py derives the API list from
 * rumqttc.h and requires both markers whenever an optional error output is
 * added. Each marked function below is called with NULL on both paths.
 */
static uint32_t coverage_redirect(void *data, const rumqttc_redirect_request_t *request, rumqttc_redirect_response_t *response) {
  (void)data;
  rumqttc_redirect_request_t *retained = NULL;
  rumqttc_redirect_request_info_t info = RUMQTTC_REDIRECT_REQUEST_INFO_INIT;
  rumqttc_redirect_reference_t reference = RUMQTTC_REDIRECT_REFERENCE_INIT;
  /* ERROR_OUT_SUCCESS: rumqttc_redirect_request_retain */
  CHECK(rumqttc_redirect_request_retain(request, &retained, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_redirect_request_retain */
  EXPECT_FAILURE(rumqttc_redirect_request_retain(NULL, NULL, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_redirect_request_info */
  CHECK(rumqttc_redirect_request_info(request, &info, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_redirect_request_info */
  EXPECT_FAILURE(rumqttc_redirect_request_info(NULL, &info, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_redirect_request_reference */
  CHECK(rumqttc_redirect_request_reference(request, 1, &reference, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_redirect_request_reference */
  EXPECT_FAILURE(rumqttc_redirect_request_reference(request, SIZE_MAX, &reference, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_redirect_response_follow */
  CHECK(rumqttc_redirect_response_follow(response, request, 1, RUMQTTC_REDIRECT_TRANSPORT_TCP, NULL, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_redirect_response_set_client_id */
  CHECK(rumqttc_redirect_response_set_client_id(response, RUMQTTC_REDIRECT_CLIENT_ID_REPLACE, native_string("target"), NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_redirect_response_set_credentials */
  CHECK(rumqttc_redirect_response_set_credentials(response, 1, native_string("user"), 1, native_bytes(NULL, 0), NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_redirect_response_set_session */
  CHECK(rumqttc_redirect_response_set_session(response, RUMQTTC_REDIRECT_SESSION_REUSE, native_string("target"), NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_redirect_response_set_reuse */
  CHECK(rumqttc_redirect_response_set_reuse(response, 0, 0, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_redirect_response_follow */
  EXPECT_FAILURE(rumqttc_redirect_response_follow(response, request, SIZE_MAX, 0, NULL, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_redirect_response_set_client_id */
  EXPECT_FAILURE(rumqttc_redirect_response_set_client_id(response, 99, native_string(""), NULL));
  /* ERROR_OUT_FAILURE: rumqttc_redirect_response_set_credentials */
  EXPECT_FAILURE(rumqttc_redirect_response_set_credentials(response, 2, native_string(""), 0, native_bytes(NULL, 0), NULL));
  /* ERROR_OUT_FAILURE: rumqttc_redirect_response_set_session */
  EXPECT_FAILURE(rumqttc_redirect_response_set_session(response, 99, native_string(""), NULL));
  /* ERROR_OUT_FAILURE: rumqttc_redirect_response_set_reuse */
  EXPECT_FAILURE(rumqttc_redirect_response_set_reuse(response, 2, 0, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_redirect_response_reject */
  CHECK(rumqttc_redirect_response_reject(response, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_redirect_response_reject */
  EXPECT_FAILURE(rumqttc_redirect_response_reject(NULL, NULL));
  rumqttc_redirect_request_destroy(retained);
  return RUMQTTC_OK;
}
static void coverage_redirect_authority(void) {
  rumqttc_redirect_vtable_t table = RUMQTTC_REDIRECT_VTABLE_INIT;
  rumqttc_redirect_registration_t *registration = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  table.decide = coverage_redirect; table.destroy = coverage_destroy;
  /* ERROR_OUT_SUCCESS: rumqttc_redirect_registration_new */
  CHECK(rumqttc_redirect_registration_new(&table, NULL, 1, 5000, &registration, NULL));
  rumqttc_redirect_registration_t *invalid = NULL;
  /* ERROR_OUT_FAILURE: rumqttc_redirect_registration_new */
  EXPECT_FAILURE(rumqttc_redirect_registration_new(&table, NULL, 0, 5000, &invalid, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("native-redirect-authority-coverage"), NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_redirect_authority */
  CHECK(rumqttc_config_set_v5_redirect_authority(config, registration, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_redirect_authority */
  EXPECT_FAILURE(rumqttc_config_set_v5_redirect_authority(config, NULL, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_redirect_registration_destroy(registration); rumqttc_config_destroy(config);
  rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_REDIRECT);
  uint8_t present = 0; rumqttc_string_view_t reference = {NULL, 0};
  /* ERROR_OUT_SUCCESS: rumqttc_event_redirect_selected_reference */
  CHECK(rumqttc_event_redirect_selected_reference(event, &present, &reference, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_event_redirect_selected_reference */
  EXPECT_FAILURE(rumqttc_event_redirect_selected_reference(NULL, &present, &reference, NULL));
  rumqttc_event_destroy(event);
  event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED); rumqttc_event_destroy(event);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
}

void native_test_error_out_contract(void) {
  coverage_redirect_authority();
  coverage_tls_profiles();
  rumqttc_config_t *v4 = NULL;
  rumqttc_config_t *v5 = NULL;
  rumqttc_config_t *start_config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_client_t *failed_client = NULL;
  rumqttc_error_t *ignored_error = NULL;
  rumqttc_string_view_t valid = native_string("value");
  rumqttc_string_view_t invalid_string = {NULL, 1};
  rumqttc_bytes_view_t empty = {NULL, 0};
  rumqttc_bytes_view_t invalid_bytes = {NULL, 1};

  /* ERROR_OUT_SUCCESS: rumqttc_config_new */
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &v4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_new */
  EXPECT_FAILURE(rumqttc_config_new(99, &v5, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &v5, NULL));

  {
    rumqttc_tls_backend_capabilities_t capabilities = RUMQTTC_TLS_BACKEND_CAPABILITIES_INIT;
    rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
    rumqttc_tls_profile_options_t options = RUMQTTC_TLS_PROFILE_OPTIONS_INIT;
    rumqttc_tls_profile_t *profile = NULL;
    rumqttc_tls_profile_t *failed_profile = NULL;
    rumqttc_proxy_options_t proxy = RUMQTTC_PROXY_OPTIONS_INIT;
    uint64_t features = rumqttc_library_capabilities();
    if (!(features & RUMQTTC_CAP_RUSTLS))
      tls.backend = RUMQTTC_TLS_BACKEND_NATIVE;
    options.tls = &tls;
    proxy.protocol = RUMQTTC_PROXY_HTTPS;
    proxy.host = native_string("localhost");
    proxy.port = 443;

    /* ERROR_OUT_SUCCESS: rumqttc_tls_backend_capabilities */
    CHECK(rumqttc_tls_backend_capabilities(tls.backend, &capabilities, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_tls_backend_capabilities */
    REQUIRE(rumqttc_tls_backend_capabilities(UINT32_MAX, &capabilities, NULL) == RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(capabilities.version_policy_mask == 0 && capabilities.root_policy_mask == 0 &&
            capabilities.pin_target_mask == 0);
    /* ERROR_OUT_FAILURE: rumqttc_tls_profile_new */
    REQUIRE(rumqttc_tls_profile_new(NULL, &failed_profile, NULL) == RUMQTTC_INVALID_ARGUMENT);
    REQUIRE(failed_profile == NULL);
    /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_tls_with_profile */
    REQUIRE(rumqttc_config_set_transport_tls_with_profile(v4, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_wss_with_profile */
    EXPECT_FAILURE(rumqttc_config_set_transport_wss_with_profile(v4, native_string("wss://localhost"), NULL, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_proxy_with_tls_profile */
    REQUIRE(rumqttc_config_set_proxy_with_tls_profile(v4, &proxy, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_redirect_policy_with_tls_profile */
    REQUIRE(rumqttc_config_set_v5_redirect_policy_with_tls_profile(
                v4, 1, RUMQTTC_REDIRECT_TRANSPORT_TLS, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);

    if (features & (RUMQTTC_CAP_RUSTLS | RUMQTTC_CAP_NATIVE_TLS)) {
      /* ERROR_OUT_SUCCESS: rumqttc_tls_profile_new */
      CHECK(rumqttc_tls_profile_new(&options, &profile, NULL));
      REQUIRE(profile != NULL);
      /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_tls_with_profile */
      CHECK(rumqttc_config_set_transport_tls_with_profile(v4, profile, NULL));
      if (features & RUMQTTC_CAP_WEBSOCKET) {
        /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_wss_with_profile */
        CHECK(rumqttc_config_set_transport_wss_with_profile(v4, native_string("wss://localhost"), profile, NULL));
      }
      if (features & RUMQTTC_CAP_HTTP_PROXY) {
        /* ERROR_OUT_SUCCESS: rumqttc_config_set_proxy_with_tls_profile */
        CHECK(rumqttc_config_set_proxy_with_tls_profile(v4, &proxy, profile, NULL));
      }
      /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_redirect_policy_with_tls_profile */
      CHECK(rumqttc_config_set_v5_redirect_policy_with_tls_profile(
          v5, 1, RUMQTTC_REDIRECT_TRANSPORT_TLS, profile, NULL));
      rumqttc_tls_profile_destroy(profile);
    }
  }

  /* ERROR_OUT_SUCCESS: rumqttc_config_set_broker */
  CHECK(rumqttc_config_set_broker(v4, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_broker */
  EXPECT_FAILURE(rumqttc_config_set_broker(NULL, valid, 1, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_client_id */
  CHECK(rumqttc_config_set_client_id(v4, valid, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_client_id */
  EXPECT_FAILURE(rumqttc_config_set_client_id(v4, invalid_string, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_username */
  CHECK(rumqttc_config_set_username(v4, valid, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_username */
  EXPECT_FAILURE(rumqttc_config_set_username(NULL, valid, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_clear_username */
  CHECK(rumqttc_config_clear_username(v4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_clear_username */
  EXPECT_FAILURE(rumqttc_config_clear_username(NULL, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_password */
  CHECK(rumqttc_config_set_password(v4, empty, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_password */
  EXPECT_FAILURE(rumqttc_config_set_password(v4, invalid_bytes, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_clear_password */
  CHECK(rumqttc_config_clear_password(v4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_clear_password */
  EXPECT_FAILURE(rumqttc_config_clear_password(NULL, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_tcp */
  CHECK(rumqttc_config_set_transport_tcp(v4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_tcp */
  EXPECT_FAILURE(rumqttc_config_set_transport_tcp(NULL, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_tls */
  CHECK(rumqttc_config_set_transport_tls(v4, empty, empty, empty, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_tls */
  EXPECT_FAILURE(
      rumqttc_config_set_transport_tls(NULL, empty, empty, empty, NULL));
  {
    rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_tls_with_options */
    CHECK(rumqttc_config_set_transport_tls_with_options(v4, &tls, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_tls_with_options */
    EXPECT_FAILURE(rumqttc_config_set_transport_tls_with_options(NULL, &tls, NULL));
  }
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_websocket */
  CHECK(rumqttc_config_set_transport_websocket(
      v4, native_string("ws://localhost"), NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_websocket */
  EXPECT_FAILURE(rumqttc_config_set_transport_websocket(NULL, valid, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_wss */
  CHECK(rumqttc_config_set_transport_wss(v4, native_string("wss://localhost"),
                                         empty, empty, empty, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_wss */
  EXPECT_FAILURE(
      rumqttc_config_set_transport_wss(NULL, valid, empty, empty, empty, NULL));
  {
    rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_wss_with_options */
    CHECK(rumqttc_config_set_transport_wss_with_options(
        v4, native_string("wss://localhost"), &tls, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_wss_with_options */
    EXPECT_FAILURE(rumqttc_config_set_transport_wss_with_options(NULL, valid, &tls, NULL));
  }
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_keep_alive_seconds */
  CHECK(rumqttc_config_set_keep_alive_seconds(v4, 5, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_keep_alive_seconds */
  EXPECT_FAILURE(rumqttc_config_set_keep_alive_seconds(NULL, 5, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_connection_timeout_seconds */
  CHECK(rumqttc_config_set_connection_timeout_seconds(v4, 1, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_connection_timeout_seconds */
  EXPECT_FAILURE(rumqttc_config_set_connection_timeout_seconds(NULL, 1, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_request_capacity */
  CHECK(rumqttc_config_set_request_capacity(v4, 4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_request_capacity */
  EXPECT_FAILURE(rumqttc_config_set_request_capacity(NULL, 4, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_event_capacity */
  CHECK(rumqttc_config_set_event_capacity(v4, 4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_event_capacity */
  EXPECT_FAILURE(rumqttc_config_set_event_capacity(NULL, 4, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_event_delivery_timeout_ms */
  CHECK(rumqttc_config_set_event_delivery_timeout_ms(v4, 100, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_event_delivery_timeout_ms */
  EXPECT_FAILURE(rumqttc_config_set_event_delivery_timeout_ms(NULL, 100, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_ack_mode */
  CHECK(rumqttc_config_set_ack_mode(v4, RUMQTTC_ACK_AUTOMATIC, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_ack_mode */
  EXPECT_FAILURE(rumqttc_config_set_ack_mode(v4, 99, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_incoming_packet_limit */
  CHECK(rumqttc_config_set_incoming_packet_limit(v4, 1024, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_incoming_packet_limit */
  EXPECT_FAILURE(rumqttc_config_set_incoming_packet_limit(NULL, 1024, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_emit_outgoing_events */
  CHECK(rumqttc_config_set_emit_outgoing_events(v4, 0, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_emit_outgoing_events */
  EXPECT_FAILURE(rumqttc_config_set_emit_outgoing_events(v4, 2, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_v4_session_present_mismatch_policy */
  CHECK(rumqttc_config_set_v4_session_present_mismatch_policy(v4, RUMQTTC_SESSION_PRESENT_MISMATCH_ERROR, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_v4_session_present_mismatch_policy */
  EXPECT_FAILURE(rumqttc_config_set_v4_session_present_mismatch_policy(v5, RUMQTTC_SESSION_PRESENT_MISMATCH_ERROR, NULL));
  EXPECT_FAILURE(rumqttc_config_set_v4_session_present_mismatch_policy(v4, 99, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_v4_clean_session */
  CHECK(rumqttc_config_set_v4_clean_session(v4, 1, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_v4_clean_session */
  EXPECT_FAILURE(rumqttc_config_set_v4_clean_session(NULL, 1, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_session */
  CHECK(rumqttc_config_set_v5_session(v5, 1, 0, 0, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_session */
  EXPECT_FAILURE(rumqttc_config_set_v5_session(NULL, 1, 0, 0, NULL));
  {
    rumqttc_last_will_t will = RUMQTTC_LAST_WILL_INIT;
    rumqttc_v5_connect_properties_t connect = RUMQTTC_V5_CONNECT_PROPERTIES_INIT;
    will.topic = native_string("error/out/will");
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_last_will */
    CHECK(rumqttc_config_set_last_will(v4, &will, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_last_will */
    EXPECT_FAILURE(rumqttc_config_set_last_will(NULL, &will, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_last_will */
    CHECK(rumqttc_config_clear_last_will(v4, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_last_will */
    EXPECT_FAILURE(rumqttc_config_clear_last_will(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_max_request_batch */
    CHECK(rumqttc_config_set_max_request_batch(v4, 4, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_max_request_batch */
    EXPECT_FAILURE(rumqttc_config_set_max_request_batch(NULL, 4, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_read_batch_size */
    CHECK(rumqttc_config_set_read_batch_size(v4, 4, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_read_batch_size */
    EXPECT_FAILURE(rumqttc_config_set_read_batch_size(NULL, 4, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_pending_throttle_us */
    CHECK(rumqttc_config_set_pending_throttle_us(v4, 1, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_pending_throttle_us */
    EXPECT_FAILURE(rumqttc_config_set_pending_throttle_us(NULL, 1, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_local_incoming_packet_limit_bytes */
    CHECK(rumqttc_config_set_local_incoming_packet_limit_bytes(v4, 1024, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_local_incoming_packet_limit_bytes */
    EXPECT_FAILURE(rumqttc_config_set_local_incoming_packet_limit_bytes(NULL, 1024, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_local_incoming_packet_limit_mode */
    CHECK(rumqttc_config_set_local_incoming_packet_limit_mode(v4, RUMQTTC_INCOMING_PACKET_LIMIT_DEFAULT, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_local_incoming_packet_limit_mode */
    EXPECT_FAILURE(rumqttc_config_set_local_incoming_packet_limit_mode(NULL, 0, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_v4_outgoing_packet_limit_bytes */
    CHECK(rumqttc_config_set_v4_outgoing_packet_limit_bytes(v4, 1024, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v4_outgoing_packet_limit_bytes */
    EXPECT_FAILURE(rumqttc_config_set_v4_outgoing_packet_limit_bytes(NULL, 1024, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_reset_v4_outgoing_packet_limit */
    CHECK(rumqttc_config_reset_v4_outgoing_packet_limit(v4, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_reset_v4_outgoing_packet_limit */
    EXPECT_FAILURE(rumqttc_config_reset_v4_outgoing_packet_limit(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_v4_inflight_limit */
    CHECK(rumqttc_config_set_v4_inflight_limit(v4, 2, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v4_inflight_limit */
    EXPECT_FAILURE(rumqttc_config_set_v4_inflight_limit(NULL, 2, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_advertised_max_packet_size_bytes */
    CHECK(rumqttc_config_set_v5_advertised_max_packet_size_bytes(v5, 1024, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_advertised_max_packet_size_bytes */
    EXPECT_FAILURE(rumqttc_config_set_v5_advertised_max_packet_size_bytes(NULL, 1024, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_v5_advertised_max_packet_size */
    CHECK(rumqttc_config_clear_v5_advertised_max_packet_size(v5, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_v5_advertised_max_packet_size */
    EXPECT_FAILURE(rumqttc_config_clear_v5_advertised_max_packet_size(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_outgoing_inflight_upper_limit */
    CHECK(rumqttc_config_set_v5_outgoing_inflight_upper_limit(v5, 2, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_outgoing_inflight_upper_limit */
    EXPECT_FAILURE(rumqttc_config_set_v5_outgoing_inflight_upper_limit(NULL, 2, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_v5_outgoing_inflight_upper_limit */
    CHECK(rumqttc_config_clear_v5_outgoing_inflight_upper_limit(v5, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_v5_outgoing_inflight_upper_limit */
    EXPECT_FAILURE(rumqttc_config_clear_v5_outgoing_inflight_upper_limit(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_connect_properties */
    CHECK(rumqttc_config_set_v5_connect_properties(v5, &connect, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_connect_properties */
    EXPECT_FAILURE(rumqttc_config_set_v5_connect_properties(NULL, &connect, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_v5_connect_properties */
    CHECK(rumqttc_config_clear_v5_connect_properties(v5, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_v5_connect_properties */
    EXPECT_FAILURE(rumqttc_config_clear_v5_connect_properties(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_topic_alias_policy */
    CHECK(rumqttc_config_set_v5_topic_alias_policy(v5, 0, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_topic_alias_policy */
    EXPECT_FAILURE(rumqttc_config_set_v5_topic_alias_policy(NULL, 0, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_websocket_header_edits */
    CHECK(rumqttc_config_set_websocket_header_edits(v5, NULL, 0, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_websocket_header_edits */
    EXPECT_FAILURE(rumqttc_config_set_websocket_header_edits(NULL, NULL, 0, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_tcp_send_buffer_size_bytes */
    CHECK(rumqttc_config_set_tcp_send_buffer_size_bytes(v4, 1024, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_tcp_send_buffer_size_bytes */
    EXPECT_FAILURE(rumqttc_config_set_tcp_send_buffer_size_bytes(NULL, 1024, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_tcp_receive_buffer_size_bytes */
    CHECK(rumqttc_config_set_tcp_receive_buffer_size_bytes(v4, 1024, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_tcp_receive_buffer_size_bytes */
    EXPECT_FAILURE(rumqttc_config_set_tcp_receive_buffer_size_bytes(NULL, 1024, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_tcp_buffer_sizes */
    CHECK(rumqttc_config_clear_tcp_buffer_sizes(v4, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_tcp_buffer_sizes */
    EXPECT_FAILURE(rumqttc_config_clear_tcp_buffer_sizes(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_tcp_nodelay */
    CHECK(rumqttc_config_set_tcp_nodelay(v4, 1, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_tcp_nodelay */
    EXPECT_FAILURE(rumqttc_config_set_tcp_nodelay(NULL, 1, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_local_bind_address */
    CHECK(rumqttc_config_set_local_bind_address(v4, native_string("127.0.0.1:0"), NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_local_bind_address */
    EXPECT_FAILURE(rumqttc_config_set_local_bind_address(NULL, valid, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_local_bind_address */
    CHECK(rumqttc_config_clear_local_bind_address(v4, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_local_bind_address */
    EXPECT_FAILURE(rumqttc_config_clear_local_bind_address(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_mptcp */
    CHECK(rumqttc_config_set_mptcp(v4, 0, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_mptcp */
    EXPECT_FAILURE(rumqttc_config_set_mptcp(NULL, 0, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_bind_device */
    CHECK(rumqttc_config_clear_bind_device(v4, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_bind_device */
    EXPECT_FAILURE(rumqttc_config_clear_bind_device(NULL, NULL));
#if defined(__linux__)
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_bind_device */
    CHECK(rumqttc_config_set_bind_device(v4, native_string("lo"), NULL));
#else
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_bind_device */
    EXPECT_FAILURE(rumqttc_config_set_bind_device(v4, native_string("lo"), NULL));
#endif
    /* ERROR_OUT_FAILURE: rumqttc_config_set_bind_device */
    EXPECT_FAILURE(rumqttc_config_set_bind_device(NULL, valid, NULL));
#if defined(__unix__)
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_unix_broker */
    CHECK(rumqttc_config_set_unix_broker(v4, native_bytes((const uint8_t *)"/tmp/rumqttc", 12), NULL));
#else
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_unix_broker */
    EXPECT_FAILURE(rumqttc_config_set_unix_broker(v4, empty, NULL));
#endif
    /* ERROR_OUT_FAILURE: rumqttc_config_set_unix_broker */
    EXPECT_FAILURE(rumqttc_config_set_unix_broker(NULL, empty, NULL));
  }
  {
    rumqttc_store_vtable_t store_vtable = RUMQTTC_STORE_VTABLE_INIT;
    rumqttc_resolver_vtable_t resolver_vtable = RUMQTTC_RESOLVER_VTABLE_INIT;
    rumqttc_auth_vtable_t auth_vtable = RUMQTTC_AUTH_VTABLE_INIT;
    rumqttc_store_registration_t *store = NULL;
    rumqttc_store_registration_t *failed_store = NULL;
    rumqttc_resolver_registration_t *resolver = NULL;
    rumqttc_resolver_registration_t *failed_resolver = NULL;
    rumqttc_auth_registration_t *auth = NULL;
    rumqttc_auth_registration_t *failed_auth = NULL;
    rumqttc_proxy_options_t proxy = RUMQTTC_PROXY_OPTIONS_INIT;
    store_vtable.load = coverage_store_callback;
    store_vtable.save = coverage_store_callback;
    store_vtable.clear = coverage_store_callback;
    store_vtable.destroy = coverage_destroy;
    resolver_vtable.resolve = coverage_resolver_callback;
    resolver_vtable.destroy = coverage_destroy;
    auth_vtable.respond = coverage_auth_callback;
    auth_vtable.destroy = coverage_destroy;
    proxy.host = native_string("127.0.0.1");
    proxy.port = 1883;

    /* ERROR_OUT_SUCCESS: rumqttc_store_registration_new */
    CHECK(rumqttc_store_registration_new(&store_vtable, NULL, &store, NULL));
    store_vtable.struct_size = 0;
    /* ERROR_OUT_FAILURE: rumqttc_store_registration_new */
    EXPECT_FAILURE(rumqttc_store_registration_new(&store_vtable, NULL, &failed_store, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_session_store */
    CHECK(rumqttc_config_set_session_store(v4, store, valid, 1000, 1024, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_session_store */
    EXPECT_FAILURE(rumqttc_config_set_session_store(v4, store, invalid_string, 1000, 1024, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_session_store */
    CHECK(rumqttc_config_clear_session_store(v4, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_session_store */
    EXPECT_FAILURE(rumqttc_config_clear_session_store(NULL, NULL));
    rumqttc_store_registration_destroy(store);

    /* ERROR_OUT_SUCCESS: rumqttc_resolver_registration_new */
    CHECK(rumqttc_resolver_registration_new(&resolver_vtable, NULL, &resolver, NULL));
    resolver_vtable.struct_size = 0;
    /* ERROR_OUT_FAILURE: rumqttc_resolver_registration_new */
    EXPECT_FAILURE(rumqttc_resolver_registration_new(&resolver_vtable, NULL, &failed_resolver, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_srv_resolver */
    CHECK(rumqttc_config_set_v5_srv_resolver(v5, resolver, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_srv_resolver */
    EXPECT_FAILURE(rumqttc_config_set_v5_srv_resolver(v4, resolver, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_v5_srv_resolver */
    CHECK(rumqttc_config_clear_v5_srv_resolver(v5, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_v5_srv_resolver */
    EXPECT_FAILURE(rumqttc_config_clear_v5_srv_resolver(v4, NULL));
    rumqttc_resolver_registration_destroy(resolver);

    /* ERROR_OUT_SUCCESS: rumqttc_auth_registration_new */
    CHECK(rumqttc_auth_registration_new(&auth_vtable, NULL, &auth, NULL));
    auth_vtable.struct_size = 0;
    /* ERROR_OUT_FAILURE: rumqttc_auth_registration_new */
    EXPECT_FAILURE(rumqttc_auth_registration_new(&auth_vtable, NULL, &failed_auth, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_authenticator */
    CHECK(rumqttc_config_set_v5_authenticator(v5, auth, native_string("custom"), 1000, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_authenticator */
    EXPECT_FAILURE(rumqttc_config_set_v5_authenticator(v4, auth, native_string("custom"), 1000, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_v5_authenticator */
    CHECK(rumqttc_config_clear_v5_authenticator(v5, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_v5_authenticator */
    EXPECT_FAILURE(rumqttc_config_clear_v5_authenticator(v4, NULL));
    rumqttc_auth_registration_destroy(auth);

    /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_broker_session_resume_policy */
    CHECK(rumqttc_config_set_v5_broker_session_resume_policy(v5, RUMQTTC_BROKER_SESSION_STRICT, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_broker_session_resume_policy */
    EXPECT_FAILURE(rumqttc_config_set_v5_broker_session_resume_policy(v4, 0, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_redirect_policy */
    CHECK(rumqttc_config_set_v5_redirect_policy(v5, RUMQTTC_REDIRECT_REJECT, 0, 0, NULL, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_redirect_policy */
    EXPECT_FAILURE(rumqttc_config_set_v5_redirect_policy(v4, RUMQTTC_REDIRECT_REJECT, 0, 0, NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_proxy */
    CHECK(rumqttc_config_clear_proxy(v4, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_proxy */
    EXPECT_FAILURE(rumqttc_config_clear_proxy(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_proxy */
    if (rumqttc_library_capabilities() & RUMQTTC_CAP_HTTP_PROXY)
      CHECK(rumqttc_config_set_proxy(v4, &proxy, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_proxy */
    EXPECT_FAILURE(rumqttc_config_set_proxy(v4, NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_scram */
    if (rumqttc_library_capabilities() & RUMQTTC_CAP_SCRAM)
      CHECK(rumqttc_config_set_v5_scram(v5, valid, native_bytes((const uint8_t *)"password", 8), 1000, 4096, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_scram */
    EXPECT_FAILURE(rumqttc_config_set_v5_scram(v4, valid, empty, 0, 0, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_v5_scram */
    CHECK(rumqttc_config_clear_v5_scram(v5, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_v5_scram */
    EXPECT_FAILURE(rumqttc_config_clear_v5_scram(v4, NULL));
  }
  rumqttc_config_destroy(v4);
  rumqttc_config_destroy(v5);

  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &start_config, NULL));
  CHECK(rumqttc_config_set_broker(start_config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(start_config, native_string("error-out"),
                                     NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_client_destroy_timeout_ms */
  CHECK(rumqttc_client_destroy_timeout_ms(NULL, 0, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_client_start */
  CHECK(rumqttc_client_start(start_config, &client, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_client_start */
  EXPECT_FAILURE(rumqttc_client_start(NULL, &failed_client, NULL));
  rumqttc_config_destroy(start_config);
  {
    rumqttc_event_t *connected =
        native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    rumqttc_publish_options_t publish = native_publish_options(RUMQTTC_QOS_1);
    rumqttc_publish_options_t invalid_publish = native_publish_options(99);
    rumqttc_subscription_t subscription =
        native_subscription("rumqttc/native/incoming", RUMQTTC_QOS_1);
    rumqttc_completion_t *publish_completion = NULL;
    rumqttc_completion_t *subscribe_completion = NULL;
    rumqttc_completion_t *diagnostics_completion = NULL;
    rumqttc_completion_t *failed_completion = NULL;
    uint64_t operation = 0;
    size_t count = 0;
    rumqttc_diagnostics_t diagnostics = RUMQTTC_DIAGNOSTICS_INIT;

    /* ERROR_OUT_SUCCESS: rumqttc_client_try_publish */
    CHECK(rumqttc_client_try_publish(client, native_string("error/out"), empty,
                                     &publish, &operation, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_try_publish */
    EXPECT_FAILURE(
        rumqttc_client_try_publish(client, native_string("error/out"), empty,
                                   &invalid_publish, &operation, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_client_publish_tracked */
    CHECK(rumqttc_client_publish_tracked(client, native_string("error/out"),
                                         empty, &publish, &publish_completion,
                                         NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_publish_tracked */
    EXPECT_FAILURE(rumqttc_client_publish_tracked(NULL, valid, empty, &publish,
                                                  &failed_completion, NULL));
    native_wait_completion(publish_completion,
                           RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    /* ERROR_OUT_SUCCESS: rumqttc_completion_poll */
    CHECK(rumqttc_completion_poll(publish_completion, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_poll */
    EXPECT_FAILURE(rumqttc_completion_poll(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_completion_wait_timeout_ms */
    CHECK(rumqttc_completion_wait_timeout_ms(publish_completion, 0, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_wait_timeout_ms */
    EXPECT_FAILURE(rumqttc_completion_wait_timeout_ms(NULL, 0, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_completion_kind */
    CHECK(rumqttc_completion_kind(publish_completion, &kind, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_kind */
    EXPECT_FAILURE(rumqttc_completion_kind(NULL, &kind, NULL));
    rumqttc_completion_destroy(publish_completion);

    /* ERROR_OUT_SUCCESS: rumqttc_client_try_subscribe */
    CHECK(rumqttc_client_try_subscribe(client, &subscription, 1, NULL, &operation,
                                       NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_try_subscribe */
    EXPECT_FAILURE(
        rumqttc_client_try_subscribe(NULL, &subscription, 1, NULL, &operation, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_client_subscribe_tracked */
    CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, NULL,
                                           &subscribe_completion, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_subscribe_tracked */
    EXPECT_FAILURE(rumqttc_client_subscribe_tracked(NULL, &subscription, 1, NULL,
                                                    &failed_completion, NULL));
    native_wait_completion(subscribe_completion, RUMQTTC_COMPLETION_SUBSCRIBE);
    /* ERROR_OUT_SUCCESS: rumqttc_completion_result_count */
    CHECK(rumqttc_completion_result_count(subscribe_completion, &count, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_result_count */
    EXPECT_FAILURE(rumqttc_completion_result_count(NULL, &count, NULL));
    {
      uint8_t success = 0, reason_present = 0, reason = 0;
      rumqttc_qos_t qos = 0;
      /* ERROR_OUT_SUCCESS: rumqttc_completion_result_at */
      CHECK(rumqttc_completion_result_at(subscribe_completion, 0, &success,
                                         &qos, &reason_present, &reason, NULL));
      success = 0;
      CHECK(rumqttc_completion_result_at(subscribe_completion, 0, &success,
                                         NULL, NULL, NULL, NULL));
      REQUIRE(success == 1);
      CHECK(rumqttc_completion_result_at(subscribe_completion, 0, NULL, &qos,
                                         NULL, NULL, NULL));
      REQUIRE(qos == RUMQTTC_QOS_1);
      EXPECT_FAILURE(rumqttc_completion_result_at(subscribe_completion, 0, NULL,
                                                  NULL, NULL, NULL, NULL));
      /* ERROR_OUT_FAILURE: rumqttc_completion_result_at */
      EXPECT_FAILURE(rumqttc_completion_result_at(
          NULL, 0, &success, &qos, &reason_present, &reason, NULL));
    }
    rumqttc_completion_destroy(subscribe_completion);
    {
      rumqttc_string_view_t filter = subscription.filter;
      /* ERROR_OUT_SUCCESS: rumqttc_client_try_unsubscribe */
      CHECK(
          rumqttc_client_try_unsubscribe(client, &filter, 1, NULL, &operation, NULL));
      /* ERROR_OUT_FAILURE: rumqttc_client_try_unsubscribe */
      EXPECT_FAILURE(
          rumqttc_client_try_unsubscribe(NULL, &filter, 1, NULL, &operation, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_unsubscribe_tracked */
      CHECK(rumqttc_client_unsubscribe_tracked(client, &filter, 1, NULL,
                                               &subscribe_completion, NULL));
      /* ERROR_OUT_FAILURE: rumqttc_client_unsubscribe_tracked */
      EXPECT_FAILURE(rumqttc_client_unsubscribe_tracked(
          NULL, &filter, 1, NULL, &failed_completion, NULL));
      native_wait_completion(subscribe_completion,
                             RUMQTTC_COMPLETION_UNSUBSCRIBE);
      rumqttc_completion_destroy(subscribe_completion);
    }
    /* ERROR_OUT_SUCCESS: rumqttc_client_diagnostics_tracked */
    CHECK(rumqttc_client_diagnostics_tracked(client, &diagnostics_completion,
                                             NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_diagnostics_tracked */
    EXPECT_FAILURE(
        rumqttc_client_diagnostics_tracked(NULL, &failed_completion, NULL));
    native_wait_completion(diagnostics_completion,
                           RUMQTTC_COMPLETION_DIAGNOSTICS);
    /* ERROR_OUT_SUCCESS: rumqttc_completion_diagnostics */
    CHECK(rumqttc_completion_diagnostics(diagnostics_completion, &diagnostics,
                                         NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_diagnostics */
    EXPECT_FAILURE(rumqttc_completion_diagnostics(NULL, &diagnostics, NULL));
    {
      uint8_t present = 99, raw = 99, resumed = 99;
      uint32_t diagnostic = 99;
      /* ERROR_OUT_SUCCESS: rumqttc_completion_connack_session_diagnostics */
      CHECK(rumqttc_completion_connack_session_diagnostics(diagnostics_completion, &present, &raw, &resumed, &diagnostic, NULL));
      REQUIRE(present == 1 && raw == 0 && resumed == 0 && diagnostic == 0);
      /* ERROR_OUT_FAILURE: rumqttc_completion_connack_session_diagnostics */
      EXPECT_FAILURE(rumqttc_completion_connack_session_diagnostics(NULL, &present, &raw, &resumed, &diagnostic, NULL));
      REQUIRE(present == 0 && raw == 0 && resumed == 0 && diagnostic == 0);
    }
    rumqttc_completion_destroy(diagnostics_completion);

    /* ERROR_OUT_SUCCESS: rumqttc_client_event_try_recv */
    CHECK(rumqttc_client_event_try_recv(client, &event, NULL));
    rumqttc_event_destroy(event);
    /* ERROR_OUT_FAILURE: rumqttc_client_event_try_recv */
    EXPECT_FAILURE(rumqttc_client_event_try_recv(NULL, &event, NULL));
    subscribe_completion = NULL;
    CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, NULL,
                                           &subscribe_completion, NULL));
    native_wait_completion(subscribe_completion, RUMQTTC_COMPLETION_SUBSCRIBE);
    rumqttc_completion_destroy(subscribe_completion);
    /* ERROR_OUT_SUCCESS: rumqttc_client_event_recv_timeout_ms */
    CHECK(rumqttc_client_event_recv_timeout_ms(client, 5000, &event, NULL));
    rumqttc_event_destroy(event);
    /* ERROR_OUT_FAILURE: rumqttc_client_event_recv_timeout_ms */
    EXPECT_FAILURE(rumqttc_client_event_recv_timeout_ms(NULL, 0, &event, NULL));
    EXPECT_FAILURE(
        rumqttc_event_disconnected(connected, &kind, &ignored_error));
    rumqttc_error_destroy(ignored_error);
    ignored_error = NULL;
    rumqttc_event_destroy(connected);
  }
  /* Let automatic protocol acknowledgements queued by the event loop reach the
   * fixture. */
  native_sleep_ms(100);
  /* ERROR_OUT_SUCCESS: rumqttc_client_close_now_timeout_ms */
  CHECK(rumqttc_client_close_now_timeout_ms(client, 5000, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_client_close_now_timeout_ms */
  EXPECT_FAILURE(rumqttc_client_close_now_timeout_ms(NULL, 5000, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_client_close_now_with_options_timeout_ms */
  CHECK(rumqttc_client_close_now_with_options_timeout_ms(client, 5000, NULL, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_client_close_now_with_options_timeout_ms */
  EXPECT_FAILURE(rumqttc_client_close_now_with_options_timeout_ms(NULL, 0, NULL, NULL));
  CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));

  client = native_start_client(RUMQTTC_PROTOCOL_V4, "error-close-options",
                               RUMQTTC_ACK_AUTOMATIC, 8, 8, 1000);
  /* ERROR_OUT_SUCCESS: rumqttc_client_close_with_options_timeout_ms */
  CHECK(rumqttc_client_close_with_options_timeout_ms(client, 5000, NULL, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_client_close_with_options_timeout_ms */
  EXPECT_FAILURE(rumqttc_client_close_with_options_timeout_ms(NULL, 0, NULL, NULL));
  CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));

  client = native_start_client(RUMQTTC_PROTOCOL_V4, "error-disconnect",
                               RUMQTTC_ACK_AUTOMATIC, 8, 8, 1000);
  {
    rumqttc_publish_options_t interrupt = native_publish_options(RUMQTTC_QOS_0);
    rumqttc_event_t *disconnected;
    rumqttc_error_t *disconnect_error = NULL;
    uint64_t operation = 0;
    uint32_t phase = 0;
    CHECK(rumqttc_client_try_publish(client,
                                     native_string("rumqttc/native/interrupt"),
                                     empty, &interrupt, &operation, NULL));
    disconnected = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
    CHECK(rumqttc_event_disconnected(disconnected, &phase, &disconnect_error));
    rumqttc_error_destroy(disconnect_error);
    rumqttc_event_destroy(disconnected);
  }
  native_close_destroy(client);

  {
    unsigned tracked;
    for (tracked = 0; tracked < 4; ++tracked) {
      rumqttc_subscription_t subscription =
          native_subscription("rumqttc/native/incoming", RUMQTTC_QOS_1);
      rumqttc_completion_t *completion = NULL;
      rumqttc_event_t *incoming;
      uint64_t operation = 0;
      client =
          native_start_client(RUMQTTC_PROTOCOL_V4,
                              tracked % 2 ? "error-ack-tracked" : "error-ack-try",
                              RUMQTTC_ACK_MANUAL, 8, 8, 1000);
      CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, NULL,
                                             &completion, NULL));
      native_wait_completion(completion, RUMQTTC_COMPLETION_SUBSCRIBE);
      rumqttc_completion_destroy(completion);
      incoming = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
      if (tracked == 0) {
        /* ERROR_OUT_SUCCESS: rumqttc_client_try_acknowledge */
        CHECK(
            rumqttc_client_try_acknowledge(client, incoming, &operation, NULL));
        /* ERROR_OUT_FAILURE: rumqttc_client_try_acknowledge */
        EXPECT_FAILURE(
            rumqttc_client_try_acknowledge(client, incoming, &operation, NULL));
      } else if (tracked == 1) {
        completion = NULL;
        /* ERROR_OUT_SUCCESS: rumqttc_client_acknowledge_tracked */
        CHECK(rumqttc_client_acknowledge_tracked(client, incoming, &completion,
                                                 NULL));
        native_wait_completion(completion, RUMQTTC_COMPLETION_ACKNOWLEDGED);
        rumqttc_completion_destroy(completion);
        completion = NULL;
        /* ERROR_OUT_FAILURE: rumqttc_client_acknowledge_tracked */
        EXPECT_FAILURE(rumqttc_client_acknowledge_tracked(client, incoming,
                                                          &completion, NULL));
      }
      if (tracked == 2) {
        /* ERROR_OUT_SUCCESS: rumqttc_client_try_acknowledge_with_options */
        CHECK(rumqttc_client_try_acknowledge_with_options(client, incoming, NULL, &operation, NULL));
        /* ERROR_OUT_FAILURE: rumqttc_client_try_acknowledge_with_options */
        EXPECT_FAILURE(rumqttc_client_try_acknowledge_with_options(client, incoming, NULL, &operation, NULL));
        REQUIRE(operation == 0);
      } else if (tracked == 3) {
        completion = NULL;
        /* ERROR_OUT_SUCCESS: rumqttc_client_acknowledge_with_options_tracked */
        CHECK(rumqttc_client_acknowledge_with_options_tracked(client, incoming, NULL, &completion, NULL));
        native_wait_completion(completion, RUMQTTC_COMPLETION_ACKNOWLEDGED);
        rumqttc_completion_destroy(completion);
        completion = (rumqttc_completion_t *)(uintptr_t)1;
        /* ERROR_OUT_FAILURE: rumqttc_client_acknowledge_with_options_tracked */
        EXPECT_FAILURE(rumqttc_client_acknowledge_with_options_tracked(client, incoming, NULL, &completion, NULL));
        REQUIRE(completion == NULL);
      }
      rumqttc_event_destroy(incoming);
      if (tracked % 2 == 0) {
        /* The untracked API reports admission, so allow the admitted ACK to
         * flush. */
        native_sleep_ms(100);
      }
      native_close_destroy(client);
    }
  }

  client = native_start_client(RUMQTTC_PROTOCOL_V4, "error-close",
                               RUMQTTC_ACK_AUTOMATIC, 8, 8, 1000);
  /* ERROR_OUT_SUCCESS: rumqttc_client_close_timeout_ms */
  CHECK(rumqttc_client_close_timeout_ms(client, 5000, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_client_close_timeout_ms */
  EXPECT_FAILURE(rumqttc_client_close_timeout_ms(NULL, 0, NULL));
  CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));

  client = native_start_client(RUMQTTC_PROTOCOL_V4, "error-destroy",
                               RUMQTTC_ACK_AUTOMATIC, 8, 8, 1000);
  /* ERROR_OUT_FAILURE: rumqttc_client_destroy_timeout_ms */
  EXPECT_FAILURE(rumqttc_client_destroy_timeout_ms(client, 0, NULL));
  /* A failed destroy retained ownership, so the same handle can be retried. */
  CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));

  client = native_start_client(RUMQTTC_PROTOCOL_V4, "error-abandon",
                               RUMQTTC_ACK_AUTOMATIC, 8, 8, 1000);
  rumqttc_client_abandon(client);
  rumqttc_client_abandon(NULL);

  {
    uint64_t operation_id = 0;
    rumqttc_completion_t *auth_completion = NULL;
    /* ERROR_OUT_FAILURE: rumqttc_client_try_reauthenticate */
    EXPECT_FAILURE(rumqttc_client_try_reauthenticate(NULL, &operation_id, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_reauthenticate_tracked */
    EXPECT_FAILURE(rumqttc_client_reauthenticate_tracked(NULL, &auth_completion, NULL));
    if (rumqttc_library_capabilities() & RUMQTTC_CAP_SCRAM) {
      rumqttc_config_t *auth_config = NULL;
      rumqttc_client_t *auth_client = NULL;
      CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &auth_config, NULL));
      CHECK(rumqttc_config_set_broker(auth_config, native_string("127.0.0.1"),
                                      native_test_port(), NULL));
      CHECK(rumqttc_config_set_client_id(auth_config, native_string("error-auth"), NULL));
      CHECK(rumqttc_config_set_v5_scram(
          auth_config, native_string("user"),
          native_bytes((const uint8_t *)"password", 8), 1000, 4096, NULL));
      CHECK(rumqttc_client_start(auth_config, &auth_client, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_try_reauthenticate */
      CHECK(rumqttc_client_try_reauthenticate(auth_client, &operation_id, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_reauthenticate_tracked */
      CHECK(rumqttc_client_reauthenticate_tracked(auth_client, &auth_completion, NULL));
      rumqttc_completion_destroy(auth_completion);
      (void)rumqttc_client_close_now_timeout_ms(auth_client, 5000, NULL);
      CHECK(rumqttc_client_destroy_timeout_ms(auth_client, 5000, NULL));
      rumqttc_config_destroy(auth_config);
    }
  }

  {
    int invoked = 0;
    rumqttc_transport_vtable_t table = RUMQTTC_TRANSPORT_VTABLE_INIT;
    table.connect = coverage_transport_connect; table.cancel = coverage_transport_cancel; table.destroy = coverage_destroy;
    rumqttc_transport_registration_t *registration = NULL;
    rumqttc_config_t *transport_config = NULL;
    rumqttc_client_t *transport_client = NULL;
    /* ERROR_OUT_SUCCESS: rumqttc_transport_registration_new */
    CHECK(rumqttc_transport_registration_new(&table, &invoked, &registration, NULL));
    rumqttc_transport_registration_t *invalid_registration = NULL;
    /* ERROR_OUT_FAILURE: rumqttc_transport_registration_new */
    EXPECT_FAILURE(rumqttc_transport_registration_new(NULL, NULL, &invalid_registration, NULL));
    CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &transport_config, NULL));
    CHECK(rumqttc_config_set_broker(transport_config, native_string("supplied.invalid"), 1883, NULL));
    CHECK(rumqttc_config_set_client_id(transport_config, native_string("transport-error-coverage"), NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_connector */
    CHECK(rumqttc_config_set_transport_connector(transport_config, registration, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_connector */
    EXPECT_FAILURE(rumqttc_config_set_transport_connector(NULL, registration, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_transport_connector */
    CHECK(rumqttc_config_clear_transport_connector(transport_config, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_transport_connector */
    EXPECT_FAILURE(rumqttc_config_clear_transport_connector(NULL, NULL));
    CHECK(rumqttc_config_set_transport_connector(transport_config, registration, NULL));
    rumqttc_transport_registration_destroy(registration);
    rumqttc_transport_stream_t *invalid_stream = NULL;
    /* ERROR_OUT_FAILURE: rumqttc_transport_stream_new */
    EXPECT_FAILURE(rumqttc_transport_stream_new(NULL, NULL, NULL, &invalid_stream, NULL));
    CHECK(rumqttc_client_start(transport_config, &transport_client, NULL));
    rumqttc_event_t *event = native_wait_event(transport_client, RUMQTTC_EVENT_DRIVER_TERMINATED);
    rumqttc_error_t *error = NULL;
    uint8_t present = 0, retryable = 1;
    uint32_t failure = 0;
    CHECK(rumqttc_event_disconnected(event, NULL, &error));
    CHECK(rumqttc_error_transport_failure(error, &present, &failure));
    REQUIRE(present && failure == RUMQTTC_TRANSPORT_FAILURE_NETWORK_OPTIONS);
    CHECK(rumqttc_error_flags(error, &retryable, NULL));
    REQUIRE(!retryable);
    rumqttc_error_destroy(error);
    rumqttc_event_destroy(event);
    CHECK(rumqttc_client_close_now_timeout_ms(transport_client, 5000, NULL));
    CHECK(rumqttc_client_destroy_timeout_ms(transport_client, 5000, NULL));
    rumqttc_config_destroy(transport_config);
    REQUIRE(invoked == 1);
  }
  if (rumqttc_library_capabilities() & RUMQTTC_CAP_WEBSOCKET_CALLBACKS) {
    rumqttc_config_t *config = NULL;
    rumqttc_websocket_registration_t *registration = NULL;
    rumqttc_websocket_response_t *response = NULL;
    rumqttc_websocket_vtable_t table = RUMQTTC_WEBSOCKET_VTABLE_INIT;
    table.prepare = coverage_websocket; table.destroy = coverage_destroy;
    CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &config, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_websocket_registration_new */
    EXPECT_FAILURE(rumqttc_websocket_registration_new(NULL, NULL, &registration, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_websocket_registration_new */
    CHECK(rumqttc_websocket_registration_new(&table, NULL, &registration, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_set_websocket_handshake */
    EXPECT_FAILURE(rumqttc_config_set_websocket_handshake(NULL, registration, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_set_websocket_handshake */
    CHECK(rumqttc_config_set_websocket_handshake(config, registration, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_config_clear_websocket_handshake */
    EXPECT_FAILURE(rumqttc_config_clear_websocket_handshake(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_config_clear_websocket_handshake */
    CHECK(rumqttc_config_clear_websocket_handshake(config, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_websocket_response_new */
    EXPECT_FAILURE(rumqttc_websocket_response_new(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_websocket_response_new */
    CHECK(rumqttc_websocket_response_new(&response, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_websocket_response_set_authority */
    EXPECT_FAILURE(rumqttc_websocket_response_set_authority(response, native_string("user@host"), NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_websocket_response_set_authority */
    CHECK(rumqttc_websocket_response_set_authority(response, native_string("customer.example:443"), NULL));
    /* ERROR_OUT_FAILURE: rumqttc_websocket_response_set_path_and_query */
    EXPECT_FAILURE(rumqttc_websocket_response_set_path_and_query(response, native_string("https://invalid"), NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_websocket_response_set_path_and_query */
    CHECK(rumqttc_websocket_response_set_path_and_query(response, native_string("/mqtt?token=one"), NULL));
    /* ERROR_OUT_FAILURE: rumqttc_websocket_response_header_edit */
    EXPECT_FAILURE(rumqttc_websocket_response_header_edit(response, RUMQTTC_WEBSOCKET_HEADER_ADD, native_string("Host"), native_bytes(NULL, 0), NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_websocket_response_header_edit */
    CHECK(rumqttc_websocket_response_header_edit(response, RUMQTTC_WEBSOCKET_HEADER_ADD, native_string("x-token"), native_bytes(NULL, 0), NULL));
    rumqttc_websocket_response_destroy(response);
    rumqttc_websocket_registration_destroy(registration);
    rumqttc_config_destroy(config);
  }
  {
    rumqttc_client_t *ack_client =
        native_start_client(RUMQTTC_PROTOCOL_V5, "native-terminal-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
    rumqttc_subscription_t filters[3] = {native_subscription("a", RUMQTTC_QOS_2),
                                         native_subscription("b", RUMQTTC_QOS_2),
                                         native_subscription("c", RUMQTTC_QOS_2)};
    rumqttc_completion_t *ack_completion = NULL;
    CHECK(rumqttc_client_subscribe_tracked(ack_client, filters, 3, NULL, &ack_completion, NULL));
    native_wait_completion(ack_completion, RUMQTTC_COMPLETION_SUBSCRIBE);
    rumqttc_acknowledgement_details_t details = RUMQTTC_ACKNOWLEDGEMENT_DETAILS_INIT;
    uint8_t present = 0, reason = 0;
    size_t count = 0;
    rumqttc_string_view_t name = {NULL, 0}, value = {NULL, 0};
    /* ERROR_OUT_SUCCESS: rumqttc_completion_acknowledgement */
    CHECK(rumqttc_completion_acknowledgement(ack_completion, &details, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_acknowledgement */
    EXPECT_FAILURE(rumqttc_completion_acknowledgement(NULL, &details, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_completion_acknowledgement_result_count */
    CHECK(rumqttc_completion_acknowledgement_result_count(ack_completion, &present, &count, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_acknowledgement_result_count */
    EXPECT_FAILURE(rumqttc_completion_acknowledgement_result_count(NULL, &present, &count, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_completion_acknowledgement_result_at */
    CHECK(rumqttc_completion_acknowledgement_result_at(ack_completion, 0, &reason, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_acknowledgement_result_at */
    EXPECT_FAILURE(rumqttc_completion_acknowledgement_result_at(ack_completion, SIZE_MAX, &reason, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_completion_acknowledgement_reason_string */
    CHECK(rumqttc_completion_acknowledgement_reason_string(ack_completion, &present, &value, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_acknowledgement_reason_string */
    EXPECT_FAILURE(rumqttc_completion_acknowledgement_reason_string(NULL, &present, &value, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_completion_acknowledgement_user_property_count */
    CHECK(rumqttc_completion_acknowledgement_user_property_count(ack_completion, &count, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_acknowledgement_user_property_count */
    EXPECT_FAILURE(rumqttc_completion_acknowledgement_user_property_count(NULL, &count, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_completion_acknowledgement_user_property_at */
    CHECK(rumqttc_completion_acknowledgement_user_property_at(ack_completion, 0, &name, &value, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_acknowledgement_user_property_at */
    EXPECT_FAILURE(rumqttc_completion_acknowledgement_user_property_at(ack_completion, SIZE_MAX, &name, &value, NULL));
    rumqttc_completion_destroy(ack_completion);
    native_close_destroy(ack_client);
  }
  if (rumqttc_library_capabilities() & RUMQTTC_CAP_ORDERED_SHUTDOWN) {
    {
      rumqttc_client_t *ordered = native_start_client(RUMQTTC_PROTOCOL_V4, "native-ordered-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
      uint64_t id = 0;
      /* ERROR_OUT_FAILURE: rumqttc_client_try_disconnect_after_queued */
      EXPECT_FAILURE(rumqttc_client_try_disconnect_after_queued(NULL, &id, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_try_disconnect_after_queued */
      CHECK(rumqttc_client_try_disconnect_after_queued(ordered, &id, NULL));
      CHECK(rumqttc_client_close_after_queued_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
      CHECK(rumqttc_client_destroy_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
    }
    {
      rumqttc_client_t *ordered = native_start_client(RUMQTTC_PROTOCOL_V4, "native-ordered-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
      uint64_t id = 0;
      /* ERROR_OUT_FAILURE: rumqttc_client_try_disconnect_after_queued_timeout_ms */
      EXPECT_FAILURE(rumqttc_client_try_disconnect_after_queued_timeout_ms(NULL, NATIVE_DEADLINE_MS, &id, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_try_disconnect_after_queued_timeout_ms */
      CHECK(rumqttc_client_try_disconnect_after_queued_timeout_ms(ordered, NATIVE_DEADLINE_MS, &id, NULL));
      CHECK(rumqttc_client_close_after_queued_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
      CHECK(rumqttc_client_destroy_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
    }
    {
      rumqttc_client_t *ordered = native_start_client(RUMQTTC_PROTOCOL_V4, "native-ordered-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
      uint64_t id = 0;
      /* ERROR_OUT_FAILURE: rumqttc_client_try_disconnect_after_queued_with_options */
      EXPECT_FAILURE(rumqttc_client_try_disconnect_after_queued_with_options(NULL, NULL, &id, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_try_disconnect_after_queued_with_options */
      CHECK(rumqttc_client_try_disconnect_after_queued_with_options(ordered, NULL, &id, NULL));
      CHECK(rumqttc_client_close_after_queued_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
      CHECK(rumqttc_client_destroy_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
    }
    {
      rumqttc_client_t *ordered = native_start_client(RUMQTTC_PROTOCOL_V4, "native-ordered-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
      uint64_t id = 0;
      /* ERROR_OUT_FAILURE: rumqttc_client_try_disconnect_after_queued_with_options_timeout_ms */
      EXPECT_FAILURE(rumqttc_client_try_disconnect_after_queued_with_options_timeout_ms(NULL, NATIVE_DEADLINE_MS, NULL, &id, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_try_disconnect_after_queued_with_options_timeout_ms */
      CHECK(rumqttc_client_try_disconnect_after_queued_with_options_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL, &id, NULL));
      CHECK(rumqttc_client_close_after_queued_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
      CHECK(rumqttc_client_destroy_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
    }
    {
      rumqttc_client_t *ordered = native_start_client(RUMQTTC_PROTOCOL_V4, "native-ordered-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
      rumqttc_completion_t *completion = NULL;
      /* ERROR_OUT_FAILURE: rumqttc_client_disconnect_after_queued_tracked */
      EXPECT_FAILURE(rumqttc_client_disconnect_after_queued_tracked(NULL, &completion, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_disconnect_after_queued_tracked */
      CHECK(rumqttc_client_disconnect_after_queued_tracked(ordered, &completion, NULL));
      native_wait_completion(completion, RUMQTTC_COMPLETION_ORDERED_SHUTDOWN);
      rumqttc_completion_destroy(completion);
      CHECK(rumqttc_client_close_after_queued_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
      CHECK(rumqttc_client_destroy_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
    }
    {
      rumqttc_client_t *ordered = native_start_client(RUMQTTC_PROTOCOL_V4, "native-ordered-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
      rumqttc_completion_t *completion = NULL;
      /* ERROR_OUT_FAILURE: rumqttc_client_disconnect_after_queued_timeout_ms_tracked */
      EXPECT_FAILURE(rumqttc_client_disconnect_after_queued_timeout_ms_tracked(NULL, NATIVE_DEADLINE_MS, &completion, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_disconnect_after_queued_timeout_ms_tracked */
      CHECK(rumqttc_client_disconnect_after_queued_timeout_ms_tracked(ordered, NATIVE_DEADLINE_MS, &completion, NULL));
      native_wait_completion(completion, RUMQTTC_COMPLETION_ORDERED_SHUTDOWN);
      rumqttc_completion_destroy(completion);
      CHECK(rumqttc_client_close_after_queued_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
      CHECK(rumqttc_client_destroy_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
    }
    {
      rumqttc_client_t *ordered = native_start_client(RUMQTTC_PROTOCOL_V4, "native-ordered-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
      rumqttc_completion_t *completion = NULL;
      /* ERROR_OUT_FAILURE: rumqttc_client_disconnect_after_queued_with_options_tracked */
      EXPECT_FAILURE(rumqttc_client_disconnect_after_queued_with_options_tracked(NULL, NULL, &completion, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_disconnect_after_queued_with_options_tracked */
      CHECK(rumqttc_client_disconnect_after_queued_with_options_tracked(ordered, NULL, &completion, NULL));
      native_wait_completion(completion, RUMQTTC_COMPLETION_ORDERED_SHUTDOWN);
      rumqttc_completion_destroy(completion);
      CHECK(rumqttc_client_close_after_queued_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
      CHECK(rumqttc_client_destroy_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
    }
    {
      rumqttc_client_t *ordered = native_start_client(RUMQTTC_PROTOCOL_V4, "native-ordered-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
      rumqttc_completion_t *completion = NULL;
      /* ERROR_OUT_FAILURE: rumqttc_client_disconnect_after_queued_with_options_timeout_ms_tracked */
      EXPECT_FAILURE(rumqttc_client_disconnect_after_queued_with_options_timeout_ms_tracked(NULL, NATIVE_DEADLINE_MS, NULL, &completion, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_disconnect_after_queued_with_options_timeout_ms_tracked */
      CHECK(rumqttc_client_disconnect_after_queued_with_options_timeout_ms_tracked(ordered, NATIVE_DEADLINE_MS, NULL, &completion, NULL));
      native_wait_completion(completion, RUMQTTC_COMPLETION_ORDERED_SHUTDOWN);
      rumqttc_completion_destroy(completion);
      CHECK(rumqttc_client_close_after_queued_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
      CHECK(rumqttc_client_destroy_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
    }
    {
      rumqttc_client_t *ordered = native_start_client(RUMQTTC_PROTOCOL_V4, "native-ordered-close-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
      /* ERROR_OUT_FAILURE: rumqttc_client_close_after_queued_timeout_ms */
      EXPECT_FAILURE(rumqttc_client_close_after_queued_timeout_ms(NULL, NATIVE_DEADLINE_MS, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_close_after_queued_timeout_ms */
      CHECK(rumqttc_client_close_after_queued_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
      CHECK(rumqttc_client_destroy_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
    }
    {
      rumqttc_client_t *ordered = native_start_client(RUMQTTC_PROTOCOL_V4, "native-ordered-close-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
      /* ERROR_OUT_FAILURE: rumqttc_client_close_after_queued_with_options_timeout_ms */
      EXPECT_FAILURE(rumqttc_client_close_after_queued_with_options_timeout_ms(NULL, NATIVE_DEADLINE_MS, NULL, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_close_after_queued_with_options_timeout_ms */
      CHECK(rumqttc_client_close_after_queued_with_options_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL, NULL));
      CHECK(rumqttc_client_destroy_timeout_ms(ordered, NATIVE_DEADLINE_MS, NULL));
    }
    {
      rumqttc_client_t *ordered = native_start_client(RUMQTTC_PROTOCOL_V4, "native-ordered-diagnostics-error-out", RUMQTTC_ACK_AUTOMATIC, 16, 64, 5000);
      rumqttc_completion_t *diagnostics = NULL;
      CHECK(rumqttc_client_diagnostics_tracked(ordered, &diagnostics, NULL));
      native_wait_completion(diagnostics, RUMQTTC_COMPLETION_DIAGNOSTICS);
      rumqttc_ordered_shutdown_diagnostics_t snapshot = RUMQTTC_ORDERED_SHUTDOWN_DIAGNOSTICS_INIT;
      /* ERROR_OUT_FAILURE: rumqttc_completion_ordered_shutdown_diagnostics */
      EXPECT_FAILURE(rumqttc_completion_ordered_shutdown_diagnostics(NULL, &snapshot, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_completion_ordered_shutdown_diagnostics */
      CHECK(rumqttc_completion_ordered_shutdown_diagnostics(diagnostics, &snapshot, NULL));
      REQUIRE(!snapshot.present);
      rumqttc_completion_destroy(diagnostics);
      native_close_destroy(ordered);
    }
  }
  (void)ignored_error;
}
