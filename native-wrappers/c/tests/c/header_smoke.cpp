#include "rumqttc.h"

#include <type_traits>

static_assert(std::is_same_v<rumqttc_status_t, uint32_t>);
static_assert(RUMQTTC_ABI_VERSION == 0x00000001u);

int main() {
  rumqttc_v5_acknowledgement_options_t ack_content = RUMQTTC_V5_ACKNOWLEDGEMENT_OPTIONS_INIT;
  rumqttc_acknowledgement_options_t ack_options = RUMQTTC_ACKNOWLEDGEMENT_OPTIONS_INIT;
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
  rumqttc_tls_options_t tls_options = RUMQTTC_TLS_OPTIONS_INIT;
  rumqttc_tls_pin_t tls_pin = RUMQTTC_TLS_PIN_INIT;
  rumqttc_tls_profile_options_t tls_profile = RUMQTTC_TLS_PROFILE_OPTIONS_INIT;
  rumqttc_tls_backend_capabilities_t tls_caps = RUMQTTC_TLS_BACKEND_CAPABILITIES_INIT;
  rumqttc_tls_pem_identity_t pem_identity = RUMQTTC_TLS_PEM_IDENTITY_INIT;
  rumqttc_tls_pkcs12_identity_t pkcs12_identity = RUMQTTC_TLS_PKCS12_IDENTITY_INIT;
  rumqttc_proxy_options_t proxy_options = RUMQTTC_PROXY_OPTIONS_INIT;
  rumqttc_store_vtable_t store_vtable = RUMQTTC_STORE_VTABLE_INIT;
  rumqttc_resolver_vtable_t resolver_vtable = RUMQTTC_RESOLVER_VTABLE_INIT;
  rumqttc_auth_vtable_t auth_vtable = RUMQTTC_AUTH_VTABLE_INIT;
  rumqttc_auth_response_t auth_response = RUMQTTC_AUTH_RESPONSE_INIT;
  rumqttc_srv_record_t srv_record = RUMQTTC_SRV_RECORD_INIT;
  rumqttc_config_t *config = nullptr;
  if (ack_content.struct_size != sizeof(ack_content) || ack_content.reason_code != 0 ||
      ack_options.struct_size != sizeof(ack_options) || ack_options.v5_options != nullptr) return 1;
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
      diagnostics.struct_size != sizeof(diagnostics) ||
      tls_pin.struct_size != sizeof(tls_pin) ||
      tls_profile.struct_size != sizeof(tls_profile) ||
      tls_caps.struct_size != sizeof(tls_caps) ||
      tls_options.struct_size != sizeof(tls_options) ||
      pem_identity.struct_size != sizeof(pem_identity) ||
      pkcs12_identity.struct_size != sizeof(pkcs12_identity)) {
    return 1;
  }
  if (proxy_options.struct_size != sizeof(proxy_options) ||
      store_vtable.struct_size != sizeof(store_vtable) ||
      resolver_vtable.struct_size != sizeof(resolver_vtable) ||
      auth_vtable.struct_size != sizeof(auth_vtable) ||
      auth_response.struct_size != sizeof(auth_response) ||
      srv_record.struct_size != sizeof(srv_record)) {
    return 1;
  }
  const auto status = rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, nullptr);
  if (status != RUMQTTC_OK) {
    return 1;
  }
  rumqttc_config_destroy(config);
  return 0;
}
