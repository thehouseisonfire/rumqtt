#include "example_common.h"
#include "external_signer.h"
#include <openssl/pem.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef RUMQTTC_EXAMPLE_DEFERRED
#include <stdatomic.h>
#endif

#ifndef RUMQTTC_EXAMPLE_DEFERRED
static uint32_t select_identity(void *data, const rumqttc_tls_identity_request_t *request, size_t *index) {
  (void)data; (void)request; *index = 0; return RUMQTTC_TLS_CALLBACK_OK;
}
static uint32_t sign_message(void *data, const rumqttc_tls_signing_request_t *request,
                             uint8_t *output, size_t capacity, size_t *written) {
  return rumqttc_example_evp_sign(data, request, output, capacity, written);
}
static void destroy_key(void *data) { EVP_PKEY_free(data); }
#else
/* A short-lived worker demonstrates deferral. A production host can submit
 * this copied message to its existing reactor or hardware-key service. */
typedef struct {
  EVP_PKEY *key;
  _Atomic unsigned count;
  _Atomic uint64_t cancelled_id;
  example_thread_t *threads[64];
} signer_host_t;
typedef struct {
  signer_host_t *host;
  uint64_t id;
  rumqttc_callback_completion_t *token;
  uint16_t scheme;
  uint8_t *message;
  size_t message_length;
} signing_work_t;
static int finish_signing(void *data) {
  signing_work_t *work = data;
  uint8_t signature[4096]; size_t written = 0;
  rumqttc_tls_signing_request_t request = {0};
  request.signature_scheme = work->scheme;
  request.message = example_bytes(work->message, work->message_length);
  uint32_t reason = RUMQTTC_TLS_CALLBACK_FAILED;
  if (atomic_load(&work->host->cancelled_id) != work->id)
    reason = rumqttc_example_evp_sign(work->host->key, &request, signature, sizeof(signature), &written);
  uint32_t status = rumqttc_callback_tls_sign_complete(work->token, reason, example_bytes(signature, written));
  rumqttc_callback_completion_destroy(work->token);
  free(work->message); free(work);
  return status == RUMQTTC_OK || status == RUMQTTC_INVALID_STATE ? 0 : 1;
}
static void select_identity(void *data, uint64_t id, const rumqttc_tls_identity_request_t *request, rumqttc_callback_completion_t *token) {
  (void)data; (void)id; (void)request; (void)rumqttc_callback_tls_select_complete(token, 0, 0);
}
static void sign_message(void *data, uint64_t id, const rumqttc_tls_signing_request_t *request, rumqttc_callback_completion_t *token) {
  signer_host_t *host = data;
  unsigned slot = atomic_fetch_add(&host->count, 1);
  if (slot >= 64) { atomic_fetch_sub(&host->count, 1); (void)rumqttc_callback_tls_sign_complete(token, 2, example_bytes(NULL, 0)); return; }
  signing_work_t *work = calloc(1, sizeof(*work));
  if (work != NULL) work->message = malloc(request->message.len);
  if (work == NULL || work->message == NULL) {
    free(work); atomic_fetch_sub(&host->count, 1); (void)rumqttc_callback_tls_sign_complete(token, 2, example_bytes(NULL, 0)); return;
  }
  work->host = host; work->id = id; work->scheme = (uint16_t)request->signature_scheme;
  work->message_length = request->message.len; memcpy(work->message, request->message.data, request->message.len);
  if (rumqttc_callback_completion_retain(token, &work->token) != RUMQTTC_OK) {
    free(work->message); free(work); atomic_fetch_sub(&host->count, 1); return;
  }
  host->threads[slot] = example_thread_start(finish_signing, work);
  if (host->threads[slot] == NULL) {
    (void)rumqttc_callback_tls_sign_complete(token, 2, example_bytes(NULL, 0));
    rumqttc_callback_completion_destroy(work->token); free(work->message); free(work); atomic_fetch_sub(&host->count, 1);
  }
}
static void cancel_signing(void *data, uint64_t id) { signer_host_t *host = data; atomic_store(&host->cancelled_id, id); }
static void destroy_key(void *data) { signer_host_t *host = data; EVP_PKEY_free(host->key); host->key = NULL; }
#endif
static size_t read_public(const char *path, uint8_t *buffer, size_t capacity) {
  FILE *input = fopen(path, "rb"); if (input == NULL) return 0;
  size_t length = fread(buffer, 1, capacity, input);
  int invalid = ferror(input) || fgetc(input) != EOF;
  fclose(input); return invalid ? 0 : length;
}
/* HOST PORT CA_PEM CLIENT_CHAIN_PEM HOST_KEY_PEM [wss].
 * The wrapper receives a certificate and opaque ID; only this host opens the key. */
int main(int argc, char **argv) {
  if (argc < 6 || argc > 7) {
    fprintf(stderr, "usage: %s HOST PORT CA_PEM CLIENT_CHAIN_PEM HOST_KEY_PEM [wss]\n", argv[0]); return 2;
  }
  if (!(rumqttc_library_capabilities() & RUMQTTC_CAP_RUSTLS)) return 77;
  char *end = NULL; unsigned long port = strtoul(argv[2], &end, 10);
  if (end == argv[2] || *end != '\0' || port == 0 || port > UINT16_MAX) return 2;
  uint8_t ca[65536], certificate[65536];
  size_t ca_length = read_public(argv[3], ca, sizeof(ca));
  size_t cert_length = read_public(argv[4], certificate, sizeof(certificate));
  if (ca_length == 0 || cert_length == 0) return 2;
  BIO *input = BIO_new_file(argv[5], "rb"); if (input == NULL) return 2;
  EVP_PKEY *key = PEM_read_bio_PrivateKey(input, NULL, NULL, NULL); BIO_free(input);
  if (key == NULL) return 2;
  uint16_t scheme;
  if (EVP_PKEY_base_id(key) == EVP_PKEY_RSA) scheme = 0x0804;
  else if (EVP_PKEY_base_id(key) == EVP_PKEY_EC && EVP_PKEY_bits(key) == 256) scheme = 0x0403;
  else { EVP_PKEY_free(key); return 2; }
#ifdef RUMQTTC_EXAMPLE_DEFERRED
  signer_host_t *host = calloc(1, sizeof(*host)); if (host == NULL) { EVP_PKEY_free(key); return 1; }
  host->key = key; atomic_init(&host->count, 0); atomic_init(&host->cancelled_id, 0);
  rumqttc_tls_async_identity_vtable_t vtable = RUMQTTC_TLS_ASYNC_IDENTITY_VTABLE_INIT;
  vtable.cancel = cancel_signing;
#else
  rumqttc_tls_identity_vtable_t vtable = RUMQTTC_TLS_IDENTITY_VTABLE_INIT;
#endif
  vtable.select = select_identity; vtable.sign = sign_message; vtable.destroy = destroy_key;
  rumqttc_tls_external_identity_t identity = RUMQTTC_TLS_EXTERNAL_IDENTITY_INIT;
  identity.certificate_pem = example_bytes(certificate, cert_length);
  identity.key_id = example_bytes("host-key", 8); identity.signature_schemes = &scheme; identity.signature_scheme_count = 1;
  rumqttc_tls_identity_registration_t *registration = NULL;
  rumqttc_tls_profile_t *profile = NULL; rumqttc_config_t *config = NULL; rumqttc_client_t *client = NULL;
  rumqttc_error_t *error = NULL; rumqttc_event_t *event = NULL; int result = 1;
#ifdef RUMQTTC_EXAMPLE_DEFERRED
  if (example_report(rumqttc_tls_identity_registration_new_async(&vtable, host, &identity, 1, &registration, &error), &error, "identity_registration")) {
    host->key = NULL;
#else
  if (example_report(rumqttc_tls_identity_registration_new(&vtable, key, &identity, 1, &registration, &error), &error, "identity_registration")) {
#endif
    EVP_PKEY_free(key); goto cleanup;
  }
  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  tls.root_policy = RUMQTTC_TLS_ROOTS_PEM; tls.ca_pem = example_bytes(ca, ca_length);
  rumqttc_tls_profile_options_t options = RUMQTTC_TLS_PROFILE_OPTIONS_INIT; options.tls = &tls;
  rumqttc_tls_profile_extensions_t extensions = RUMQTTC_TLS_PROFILE_EXTENSIONS_INIT; extensions.external_identity = registration;
  if (example_report(rumqttc_tls_profile_new_with_extensions(&options, &extensions, &profile, &error), &error, "tls_profile") ||
      example_report(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, &error), &error, "config") ||
      example_report(rumqttc_config_set_broker(config, example_string(argv[1]), (uint16_t)port, &error), &error, "broker") ||
      example_report(rumqttc_config_set_client_id(config, example_string("c-external-identity"), &error), &error, "client_id")) goto cleanup;
  if (argc == 7) {
    char url[512];
    if (strcmp(argv[6], "wss") != 0 || snprintf(url, sizeof(url), "wss://%s:%lu/mqtt", argv[1], port) >= (int)sizeof(url)) { result = 2; goto cleanup; }
    if (example_report(rumqttc_config_set_transport_wss_with_profile(config, example_string(url), profile, &error), &error, "wss_profile")) goto cleanup;
  } else if (example_report(rumqttc_config_set_transport_tls_with_profile(config, profile, &error), &error, "tls_profile")) goto cleanup;
  rumqttc_tls_identity_registration_destroy(registration); registration = NULL;
  rumqttc_tls_profile_destroy(profile); profile = NULL;
  if (example_report(rumqttc_client_start(config, &client, &error), &error, "start")) goto cleanup;
  event = example_next_event(client, RUMQTTC_EVENT_CONNECTED); if (event == NULL) goto cleanup;
  if (example_report(rumqttc_client_close_timeout_ms(client, 5000, &error), &error, "close")) goto cleanup;
  result = 0;
cleanup:
  rumqttc_event_destroy(event); rumqttc_tls_profile_destroy(profile);
  rumqttc_tls_identity_registration_destroy(registration); rumqttc_config_destroy(config);
  rumqttc_error_destroy(error); example_destroy_client(&client);
#ifdef RUMQTTC_EXAMPLE_DEFERRED
  for (unsigned i = 0; i < atomic_load(&host->count); ++i) {
    int worker_result = 0;
    if (example_thread_join(host->threads[i], 5000, &worker_result) != 0) {
      /* Preserve the heap owner until this process exits if a host worker is
       * still alive; never free memory underneath retained completion work. */
      return 1;
    }
    if (worker_result != 0) result = 1;
  }
  if (host->key != NULL) return 1;
  free(host);
#endif
  return result;
}
