#include "rumqttc.h"

#include <assert.h>
#include <stddef.h>
#include <string.h>

#if UINTPTR_MAX == UINT64_MAX
_Static_assert(sizeof(rumqttc_v5_acknowledgement_options_t) == 48,
               "v5 acknowledgement options ABI changed");
_Static_assert(sizeof(rumqttc_acknowledgement_options_t) == 32,
               "acknowledgement options ABI changed");
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
  rumqttc_reconnect_options_t reconnect = RUMQTTC_RECONNECT_OPTIONS_INIT;
  rumqttc_reconnect_diagnostics_t retry_diagnostics = RUMQTTC_RECONNECT_DIAGNOSTICS_INIT;
  assert(reconnect.struct_size == sizeof(reconnect) && reconnect.mode == RUMQTTC_RECONNECT_CLASSIFIED);
  assert(retry_diagnostics.struct_size == sizeof(retry_diagnostics));
  rumqttc_diagnostics_status_t structured_status = RUMQTTC_DIAGNOSTICS_STATUS_INIT;
  assert(structured_status.struct_size == sizeof(structured_status));
  rumqttc_diagnostics_group_info_t structured_group_info = RUMQTTC_DIAGNOSTICS_GROUP_INFO_INIT;
  assert(structured_group_info.struct_size == sizeof(structured_group_info));
  rumqttc_diagnostics_queues_t structured_queues = RUMQTTC_DIAGNOSTICS_QUEUES_INIT;
  assert(structured_queues.struct_size == sizeof(structured_queues));
  rumqttc_diagnostics_outbound_t structured_outbound = RUMQTTC_DIAGNOSTICS_OUTBOUND_INIT;
  assert(structured_outbound.struct_size == sizeof(structured_outbound));
  rumqttc_diagnostics_session_t structured_session = RUMQTTC_DIAGNOSTICS_SESSION_INIT;
  assert(structured_session.struct_size == sizeof(structured_session));
  rumqttc_diagnostics_batching_t structured_batching = RUMQTTC_DIAGNOSTICS_BATCHING_INIT;
  assert(structured_batching.struct_size == sizeof(structured_batching));
  rumqttc_diagnostics_redirect_t structured_redirect = RUMQTTC_DIAGNOSTICS_REDIRECT_INIT;
  assert(structured_redirect.struct_size == sizeof(structured_redirect));
  rumqttc_diagnostics_t diagnostics = RUMQTTC_DIAGNOSTICS_INIT;
  rumqttc_runtime_network_options_t network_update = RUMQTTC_RUNTIME_NETWORK_OPTIONS_INIT;
  rumqttc_configuration_status_t configuration = RUMQTTC_CONFIGURATION_STATUS_INIT;
  rumqttc_configuration_receipt_status_t activation = RUMQTTC_CONFIGURATION_RECEIPT_STATUS_INIT;
  rumqttc_runtime_tuning_t tuning = RUMQTTC_RUNTIME_TUNING_INIT;
  rumqttc_connection_profile_summary_t profile_summary = RUMQTTC_CONNECTION_PROFILE_SUMMARY_INIT;
  assert(network_update.struct_size == sizeof(network_update) && network_update.present_fields == 0);
  assert(configuration.struct_size == sizeof(configuration) && configuration.revision == 0);
  assert(activation.struct_size == sizeof(activation) && activation.revision == 0);
  assert(tuning.struct_size == sizeof(tuning) && tuning.read_batch_size == 0);
  assert(profile_summary.struct_size == sizeof(profile_summary) && profile_summary.flags == 0);
  rumqttc_tls_options_t tls_options = RUMQTTC_TLS_OPTIONS_INIT;
  rumqttc_tls_pin_t tls_pin = RUMQTTC_TLS_PIN_INIT;
  rumqttc_tls_profile_options_t tls_profile = RUMQTTC_TLS_PROFILE_OPTIONS_INIT;
  rumqttc_tls_backend_capabilities_t tls_caps = RUMQTTC_TLS_BACKEND_CAPABILITIES_INIT;
  rumqttc_tls_profile_extensions_t tls_extensions = RUMQTTC_TLS_PROFILE_EXTENSIONS_INIT;
  rumqttc_tls_advanced_capabilities_t tls_advanced = RUMQTTC_TLS_ADVANCED_CAPABILITIES_INIT;
  rumqttc_tls_async_verifier_vtable_t deferred_verifier = RUMQTTC_TLS_ASYNC_VERIFIER_VTABLE_INIT;
  rumqttc_tls_async_identity_vtable_t deferred_identity = RUMQTTC_TLS_ASYNC_IDENTITY_VTABLE_INIT;
  assert(deferred_verifier.struct_size == sizeof(deferred_verifier) && deferred_verifier.max_retained_operations == 64);
  assert(deferred_identity.struct_size == sizeof(deferred_identity) && deferred_identity.max_retained_operations == 64);
  rumqttc_tls_verifier_vtable_t tls_verifier = RUMQTTC_TLS_VERIFIER_VTABLE_INIT;
  rumqttc_tls_identity_vtable_t tls_signer = RUMQTTC_TLS_IDENTITY_VTABLE_INIT;
  rumqttc_tls_external_identity_t tls_external_identity = RUMQTTC_TLS_EXTERNAL_IDENTITY_INIT;
  rumqttc_tls_pem_identity_t pem_identity = RUMQTTC_TLS_PEM_IDENTITY_INIT;
  rumqttc_tls_pkcs12_identity_t pkcs12_identity = RUMQTTC_TLS_PKCS12_IDENTITY_INIT;
  rumqttc_proxy_options_t proxy_options = RUMQTTC_PROXY_OPTIONS_INIT;
  rumqttc_store_vtable_t store_vtable = RUMQTTC_STORE_VTABLE_INIT;
  rumqttc_resolver_vtable_t resolver_vtable = RUMQTTC_RESOLVER_VTABLE_INIT;
  rumqttc_auth_vtable_t auth_vtable = RUMQTTC_AUTH_VTABLE_INIT;
  rumqttc_auth_response_t auth_response = RUMQTTC_AUTH_RESPONSE_INIT;
  rumqttc_srv_record_t srv_record = RUMQTTC_SRV_RECORD_INIT;
  rumqttc_config_t *config = NULL;
  rumqttc_error_t *error = NULL;
  rumqttc_string_view_t host = {"127.0.0.1", strlen("127.0.0.1")};
  rumqttc_string_view_t client_id = {"c-smoke", strlen("c-smoke")};

  assert(rumqttc_abi_version() == RUMQTTC_ABI_VERSION);
  assert(rumqttc_library_version() != NULL);
  assert(ack_content.struct_size == sizeof(ack_content) && ack_content.reason_code == 0);
  assert(ack_options.struct_size == sizeof(ack_options) && ack_options.v5_options == NULL);
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
  assert(tls_options.struct_size == sizeof(tls_options));
  assert(tls_pin.struct_size == sizeof(tls_pin));
  assert(tls_profile.struct_size == sizeof(tls_profile));
  assert(tls_caps.struct_size == sizeof(tls_caps));
  assert(tls_extensions.struct_size == sizeof(tls_extensions));
  assert(tls_advanced.struct_size == sizeof(tls_advanced));
  assert(tls_verifier.struct_size == sizeof(tls_verifier));
  assert(tls_signer.struct_size == sizeof(tls_signer));
  assert(tls_external_identity.struct_size == sizeof(tls_external_identity));
  assert(pem_identity.struct_size == sizeof(pem_identity));
  assert(pkcs12_identity.struct_size == sizeof(pkcs12_identity));
  assert(proxy_options.struct_size == sizeof(proxy_options));
  assert(store_vtable.struct_size == sizeof(store_vtable));
  assert(resolver_vtable.struct_size == sizeof(resolver_vtable));
  assert(auth_vtable.struct_size == sizeof(auth_vtable));
  assert(auth_response.struct_size == sizeof(auth_response));
  assert(srv_record.struct_size == sizeof(srv_record));
  assert(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &config, &error) ==
         RUMQTTC_OK);
  assert(config != NULL && error == NULL);
  assert(rumqttc_config_set_broker(config, host, 1883, NULL) == RUMQTTC_OK);
  assert(rumqttc_config_set_client_id(config, client_id, NULL) == RUMQTTC_OK);
  rumqttc_config_destroy(config);
  rumqttc_config_destroy(NULL);
  return 0;
}
