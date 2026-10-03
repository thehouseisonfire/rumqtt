#include "external_signer.h"
#include <openssl/rsa.h>
uint32_t rumqttc_example_evp_sign(EVP_PKEY *key, const rumqttc_tls_signing_request_t *request,
                                uint8_t *output, size_t capacity, size_t *written) {
  const EVP_MD *digest = NULL;
  int pss = 0;
  *written = 0;
  switch (request->signature_scheme) {
    case 0x0804: pss = 1; /* fall through */
    case 0x0401: case 0x0403: digest = EVP_sha256(); break;
    case 0x0805: pss = 1; /* fall through */
    case 0x0501: case 0x0503: digest = EVP_sha384(); break;
    case 0x0806: pss = 1; /* fall through */
    case 0x0601: case 0x0603: digest = EVP_sha512(); break;
    case 0x0807: break; /* Ed25519 uses the complete message with no prehash. */
    default: return RUMQTTC_TLS_CALLBACK_FAILED;
  }
  EVP_MD_CTX *context = EVP_MD_CTX_new();
  EVP_PKEY_CTX *key_context = NULL;
  uint32_t result = RUMQTTC_TLS_CALLBACK_FAILED;
  size_t length = capacity;
  if (context == NULL || EVP_DigestSignInit(context, &key_context, digest, NULL, key) <= 0) goto cleanup;
  if (pss && (EVP_PKEY_CTX_set_rsa_padding(key_context, RSA_PKCS1_PSS_PADDING) <= 0 ||
              EVP_PKEY_CTX_set_rsa_pss_saltlen(key_context, RSA_PSS_SALTLEN_DIGEST) <= 0 ||
              EVP_PKEY_CTX_set_rsa_mgf1_md(key_context, digest) <= 0)) goto cleanup;
  if (EVP_DigestSign(context, output, &length, request->message.data, request->message.len) <= 0 || length > capacity)
    goto cleanup;
  *written = length;
  result = RUMQTTC_TLS_CALLBACK_OK;
cleanup:
  EVP_MD_CTX_free(context);
  return result;
}
