#include "rumqttc.h"

int main(void) {
  const uint64_t required = RUMQTTC_CAP_HTTP_PROXY | RUMQTTC_CAP_SOCKS5_PROXY;
  return (rumqttc_library_capabilities() & required) == required ? 0 : 1;
}
