#include "rumqttc.h"

#include <assert.h>
#include <stddef.h>
#include <string.h>

#if UINTPTR_MAX == UINT64_MAX
_Static_assert(sizeof(rumqttc_bytes_view_t) == 16,
               "rumqttc_bytes_view_t ABI changed");
_Static_assert(sizeof(rumqttc_string_view_t) == 16,
               "rumqttc_string_view_t ABI changed");
_Static_assert(sizeof(rumqttc_user_property_t) == 40,
               "rumqttc_user_property_t ABI changed");
_Static_assert(sizeof(rumqttc_v5_publish_properties_t) == 96,
               "v5 properties ABI changed");
_Static_assert(sizeof(rumqttc_publish_options_t) == 24,
               "publish options ABI changed");
_Static_assert(sizeof(rumqttc_v5_subscription_options_t) == 12,
               "v5 subscription options ABI changed");
_Static_assert(sizeof(rumqttc_subscription_t) == 40,
               "subscription ABI changed");
_Static_assert(sizeof(rumqttc_v5_subscribe_properties_t) == 32,
               "v5 subscribe properties ABI changed");
_Static_assert(sizeof(rumqttc_subscribe_options_t) == 16,
               "subscribe options ABI changed");
_Static_assert(sizeof(rumqttc_v5_unsubscribe_properties_t) == 24,
               "v5 unsubscribe properties ABI changed");
_Static_assert(sizeof(rumqttc_unsubscribe_options_t) == 16,
               "unsubscribe options ABI changed");
_Static_assert(sizeof(rumqttc_diagnostics_t) == 48, "diagnostics ABI changed");
_Static_assert(offsetof(rumqttc_v5_publish_properties_t, user_properties) == 80,
               "v5 properties field offset changed");
#endif

int main(void) {
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
  rumqttc_config_t *config = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_string_view_t host = {"127.0.0.1", strlen("127.0.0.1")};
  rumqttc_string_view_t client_id = {"c-smoke", strlen("c-smoke")};

  assert(rumqttc_abi_version() == RUMQTTC_ABI_VERSION);
  assert(rumqttc_library_version() != NULL);
  assert(user_property.struct_size == sizeof(user_property));
  assert(properties.struct_size == sizeof(properties));
  assert(properties.user_properties == NULL &&
         properties.user_property_count == 0);
  assert(publish_options.struct_size == sizeof(publish_options));
  assert(publish_options.qos == RUMQTTC_QOS_0 &&
         publish_options.protocol_options ==
             RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL &&
         publish_options.v5_properties == NULL);
  assert(v5_subscription_options.struct_size ==
         sizeof(v5_subscription_options));
  assert(subscription.struct_size == sizeof(subscription));
  assert(subscription.filter.data == NULL && subscription.filter.len == 0 &&
         subscription.protocol_options ==
             RUMQTTC_PROTOCOL_OPTIONS_VERSION_NEUTRAL &&
         subscription.v5_options == NULL);
  assert(v5_subscribe_properties.struct_size ==
         sizeof(v5_subscribe_properties));
  assert(subscribe_options.struct_size == sizeof(subscribe_options));
  assert(v5_unsubscribe_properties.struct_size ==
         sizeof(v5_unsubscribe_properties));
  assert(unsubscribe_options.struct_size == sizeof(unsubscribe_options));
  assert(diagnostics.struct_size == sizeof(diagnostics));
  assert(rumqttc_config_new(RUMQTTC_PROTOCOL_V311, &config, &error) ==
         RUMQTTC_OK);
  assert(config != NULL && error == NULL);
  assert(rumqttc_config_set_broker(config, host, 1883, NULL) == RUMQTTC_OK);
  assert(rumqttc_config_set_client_id(config, client_id, NULL) == RUMQTTC_OK);
  rumqttc_config_destroy(config);
  rumqttc_config_destroy(NULL);
  return 0;
}
