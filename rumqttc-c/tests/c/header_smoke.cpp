#include "rumqttc.h"

#include <type_traits>

static_assert(std::is_same_v<rumqttc_status_t, uint32_t>);
static_assert(RUMQTTC_ABI_VERSION == 0x00010000u);

int main() {
    rumqttc_config_t *config = nullptr;
    const auto status = rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, nullptr);
    if (status != RUMQTTC_OK) {
        return 1;
    }
    rumqttc_config_destroy(config);
    return 0;
}
