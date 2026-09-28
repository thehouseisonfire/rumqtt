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

int main(void) {
  const uint64_t capabilities = rumqttc_library_capabilities();
  const uint64_t required = RUMQTTC_CAP_PROTOCOL_V4 | RUMQTTC_CAP_PROTOCOL_V5 | RUMQTTC_CAP_SESSION_STORE_CALLBACKS |
                            RUMQTTC_CAP_AUTH_CALLBACKS;
  const struct {
    uint64_t bit;
    int expected;
  } checks[] = {
      {RUMQTTC_CAP_RUSTLS, RUMQTTC_EXPECT_RUSTLS},
      {RUMQTTC_CAP_NATIVE_TLS, RUMQTTC_EXPECT_NATIVE_TLS},
      {RUMQTTC_CAP_WEBSOCKET, RUMQTTC_EXPECT_WEBSOCKET},
      {RUMQTTC_CAP_HTTP_PROXY, RUMQTTC_EXPECT_HTTP_PROXY},
      {RUMQTTC_CAP_SOCKS5_PROXY, RUMQTTC_EXPECT_SOCKS5_PROXY},
  };
  if ((capabilities & required) != required)
    return 1;
  for (size_t index = 0; index < sizeof(checks) / sizeof(checks[0]); ++index) {
    if ((int)((capabilities & checks[index].bit) != 0) != checks[index].expected)
      return 2;
  }
  /* A consumer must ignore capabilities added in future ABI-compatible builds. */
  return ((capabilities | (UINT64_C(1) << 63)) & required) == required ? 0 : 3;
}
