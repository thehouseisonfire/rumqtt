#include "rumqttc.h"

#ifndef RUMQTTC_EXPECT_RUSTLS
#define RUMQTTC_EXPECT_RUSTLS 0
#endif
#ifndef RUMQTTC_EXPECT_NATIVE_TLS
#define RUMQTTC_EXPECT_NATIVE_TLS 0
#endif
#ifndef RUMQTTC_EXPECT_WEBSOCKET
#define RUMQTTC_EXPECT_WEBSOCKET 0
#endif
#ifndef RUMQTTC_EXPECT_HTTP_PROXY
#define RUMQTTC_EXPECT_HTTP_PROXY 0
#endif
#ifndef RUMQTTC_EXPECT_SOCKS5_PROXY
#define RUMQTTC_EXPECT_SOCKS5_PROXY 0
#endif

#ifndef RUMQTTC_EXPECT_ORDERED_SHUTDOWN
#define RUMQTTC_EXPECT_ORDERED_SHUTDOWN 0
#endif

int main(void) {
  const uint64_t capabilities = rumqttc_library_capabilities();
  const uint64_t required = RUMQTTC_CAP_PROTOCOL_V4 | RUMQTTC_CAP_PROTOCOL_V5 | RUMQTTC_CAP_SESSION_STORE_CALLBACKS |
                            RUMQTTC_CAP_AUTH_CALLBACKS | RUMQTTC_CAP_TRANSPORT_CALLBACKS |
                            RUMQTTC_CAP_RUNTIME_CONFIGURATION;
  const struct {
    uint64_t bit;
    int expected;
  } checks[] = {
      {RUMQTTC_CAP_ORDERED_SHUTDOWN, RUMQTTC_EXPECT_ORDERED_SHUTDOWN},
      {RUMQTTC_CAP_RUSTLS, RUMQTTC_EXPECT_RUSTLS},
      {RUMQTTC_CAP_NATIVE_TLS, RUMQTTC_EXPECT_NATIVE_TLS},
      {RUMQTTC_CAP_WEBSOCKET, RUMQTTC_EXPECT_WEBSOCKET},
      {RUMQTTC_CAP_WEBSOCKET_CALLBACKS, RUMQTTC_EXPECT_WEBSOCKET},
      {RUMQTTC_CAP_HTTP_PROXY, RUMQTTC_EXPECT_HTTP_PROXY},
      {RUMQTTC_CAP_SOCKS5_PROXY, RUMQTTC_EXPECT_SOCKS5_PROXY},
  };
  if ((capabilities & required) != required)
    return 1;
  for (size_t index = 0; index < sizeof(checks) / sizeof(checks[0]); ++index) {
    if ((int)((capabilities & checks[index].bit) != 0) != checks[index].expected)
      return 2;
  }
  for (uint32_t backend = 0; backend <= 1; ++backend) {
    rumqttc_tls_backend_capabilities_t tls = RUMQTTC_TLS_BACKEND_CAPABILITIES_INIT;
    if (rumqttc_tls_backend_capabilities(backend, &tls, NULL) != RUMQTTC_OK)
      return 4;
    rumqttc_tls_advanced_capabilities_t advanced = RUMQTTC_TLS_ADVANCED_CAPABILITIES_INIT;
    if (rumqttc_tls_advanced_capabilities(backend, &advanced, NULL) != RUMQTTC_OK) return 9;
    int enabled = backend == RUMQTTC_TLS_BACKEND_RUSTLS ? RUMQTTC_EXPECT_RUSTLS : RUMQTTC_EXPECT_NATIVE_TLS;
    if (!enabled) {
      if (tls.version_policy_mask != 0 || tls.root_policy_mask != 0 || tls.pin_target_mask != 0)
        return 5;
      if (advanced.sni_policy_mask || advanced.resumption_policy_mask || advanced.feature_mask || advanced.max_signature_bytes) return 10;
    } else {
      if (advanced.sni_policy_mask != 7) return 11;
      if (backend == RUMQTTC_TLS_BACKEND_RUSTLS) {
        if (advanced.resumption_policy_mask != 3 || advanced.feature_mask != 31 || advanced.max_signature_bytes != 4096) return 12;
        size_t count = 0;
        if (rumqttc_tls_supported_cipher_suites(backend, NULL, 0, &count, NULL) != RUMQTTC_OK || count == 0) return 13;
        if (rumqttc_tls_supported_signature_schemes(backend, NULL, 0, &count, NULL) != RUMQTTC_OK || count == 0) return 14;
      } else if (advanced.resumption_policy_mask != 1 || advanced.feature_mask || advanced.max_signature_bytes) return 15;
      if (!(tls.version_policy_mask & (1u << RUMQTTC_TLS_VERSION_DEFAULT)) || (tls.root_policy_mask & 7u) != 7u)
        return 6;
      if (backend == RUMQTTC_TLS_BACKEND_RUSTLS && (tls.pin_target_mask & 3u) != 3u)
        return 7;
    }
  }
  rumqttc_tls_profile_t *profile = (rumqttc_tls_profile_t *)(uintptr_t)1;
  if (rumqttc_tls_profile_new(NULL, &profile, NULL) != RUMQTTC_INVALID_ARGUMENT || profile != NULL)
    return 8;
  rumqttc_tls_profile_destroy(NULL);
  /* A consumer must ignore capabilities added in future ABI-compatible builds. */
  return ((capabilities | (UINT64_C(1) << 63)) & required) == required ? 0 : 3;
}
