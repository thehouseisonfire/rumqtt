#include "rumqttc.h"

#include <type_traits>

static_assert(std::is_same_v<rumqttc_status_t, uint32_t>);
static_assert(RUMQTTC_ABI_VERSION == 0x00000001u);

int main() {
  rumqttc_user_property_t user_property = RUMQTTC_USER_PROPERTY_INIT;
  rumqttc_v5_publish_properties_t properties =
      RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  rumqttc_publish_options_t publish_options = RUMQTTC_PUBLISH_OPTIONS_INIT;
  rumqttc_v5_subscription_options_t v5_subscription_options =
      RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT;
  rumqttc_subscription_t subscription = RUMQTTC_SUBSCRIPTION_INIT;
  rumqttc_v5_subscribe_properties_t v5_subscribe_properties =
      RUMQTTC_V5_SUBSCRIBE_PROPERTIES_INIT;
  rumqttc_subscribe_options_t subscribe_options =
      RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
  rumqttc_v5_unsubscribe_properties_t v5_unsubscribe_properties =
      RUMQTTC_V5_UNSUBSCRIBE_PROPERTIES_INIT;
  rumqttc_unsubscribe_options_t unsubscribe_options =
      RUMQTTC_UNSUBSCRIBE_OPTIONS_INIT;
  rumqttc_diagnostics_t diagnostics = RUMQTTC_DIAGNOSTICS_INIT;
  rumqttc_config_t *config = nullptr;
  if (user_property.struct_size != sizeof(user_property) ||
      properties.struct_size != sizeof(properties) ||
      publish_options.struct_size != sizeof(publish_options) ||
      v5_subscription_options.struct_size != sizeof(v5_subscription_options) ||
      subscription.struct_size != sizeof(subscription) ||
      v5_subscribe_properties.struct_size != sizeof(v5_subscribe_properties) ||
      subscribe_options.struct_size != sizeof(subscribe_options) ||
      v5_unsubscribe_properties.struct_size !=
          sizeof(v5_unsubscribe_properties) ||
      unsubscribe_options.struct_size != sizeof(unsubscribe_options) ||
      diagnostics.struct_size != sizeof(diagnostics)) {
    return 1;
  }
  const auto status = rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, nullptr);
  if (status != RUMQTTC_OK) {
    return 1;
  }
  rumqttc_config_destroy(config);
  return 0;
}
