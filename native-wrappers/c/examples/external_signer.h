#ifndef RUMQTTC_EXTERNAL_SIGNER_H
#define RUMQTTC_EXTERNAL_SIGNER_H
#include "rumqttc.h"
#include <openssl/evp.h>
/* Illustrates a host-owned key. An HSM adapter can replace EVP without changing
 * the profile: the wrapper receives only certificates and opaque key IDs. */
uint32_t rumqttc_example_evp_sign(EVP_PKEY *key, const rumqttc_tls_signing_request_t *request,
                                uint8_t *output, size_t capacity, size_t *written);
#endif
