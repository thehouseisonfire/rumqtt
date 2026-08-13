#include "native_common.h"

#include <stdint.h>

#define EXPECT_REJECTION(expected_kind, expression)                            \
  do {                                                                         \
    uint64_t operation = UINT64_MAX;                                           \
    rumqttc_error_t *error = (rumqttc_error_t *)(uintptr_t)1;                  \
    rumqttc_error_kind_t kind = UINT32_MAX;                                    \
    rumqttc_status_t status = (expression);                                    \
    REQUIRE(status == RUMQTTC_INVALID_ARGUMENT);                               \
    REQUIRE(operation == 0);                                                   \
    REQUIRE(error != NULL && error != (rumqttc_error_t *)(uintptr_t)1);        \
    CHECK(rumqttc_error_kind(error, &kind));                                   \
    REQUIRE(kind == (expected_kind));                                          \
    rumqttc_error_destroy(error);                                              \
  } while (0)

static void expect_publish_rejection(rumqttc_client_t *client,
                                     rumqttc_publish_options_t *options,
                                     rumqttc_error_kind_t expected_kind) {
  EXPECT_REJECTION(expected_kind,
                   rumqttc_client_try_publish(
                       client, native_string("rumqttc/native/rejected"),
                       native_bytes(NULL, 0), options, &operation, &error));
}

static void expect_subscribe_rejection(rumqttc_client_t *client,
                                       rumqttc_subscription_t *subscription,
                                       rumqttc_subscribe_options_t *options,
                                       rumqttc_error_kind_t expected_kind) {
  EXPECT_REJECTION(expected_kind,
                   rumqttc_client_try_subscribe(client, subscription, 1,
                                                options, &operation, &error));
}

static void expect_unsubscribe_rejection(rumqttc_client_t *client,
                                         rumqttc_unsubscribe_options_t *options,
                                         rumqttc_error_kind_t expected_kind) {
  rumqttc_string_view_t filter = native_string("rumqttc/native/rejected");
  EXPECT_REJECTION(expected_kind,
                   rumqttc_client_try_unsubscribe(client, &filter, 1, options,
                                                  &operation, &error));
}

static void test_unknown_and_inconsistent_selectors(rumqttc_client_t *client) {
  rumqttc_v5_publish_properties_t publish_properties =
      RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  rumqttc_publish_options_t publish_options = RUMQTTC_PUBLISH_OPTIONS_INIT;
  rumqttc_v5_subscription_options_t filter_options =
      RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT;
  rumqttc_subscription_t subscription =
      native_subscription("rumqttc/native/rejected", RUMQTTC_QOS_0);
  rumqttc_v5_subscribe_properties_t subscribe_properties =
      RUMQTTC_V5_SUBSCRIBE_PROPERTIES_INIT;
  rumqttc_subscribe_options_t subscribe_options =
      RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
  rumqttc_v5_unsubscribe_properties_t unsubscribe_properties =
      RUMQTTC_V5_UNSUBSCRIBE_PROPERTIES_INIT;
  rumqttc_unsubscribe_options_t unsubscribe_options =
      RUMQTTC_UNSUBSCRIBE_OPTIONS_INIT;

  publish_options.protocol_options = 99;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_NONE);
  subscribe_options.protocol_options = 99;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
  subscribe_options =
      (rumqttc_subscribe_options_t)RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
  subscription.protocol_options = 99;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
  subscription = native_subscription("rumqttc/native/rejected", RUMQTTC_QOS_0);
  unsubscribe_options.protocol_options = 99;
  expect_unsubscribe_rejection(client, &unsubscribe_options,
                               RUMQTTC_ERROR_NONE);

  publish_options = (rumqttc_publish_options_t)RUMQTTC_PUBLISH_OPTIONS_INIT;
  publish_options.v5_properties = &publish_properties;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_NONE);
  subscription.v5_options = &filter_options;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
  subscription = native_subscription("rumqttc/native/rejected", RUMQTTC_QOS_0);
  subscribe_options.v5_properties = &subscribe_properties;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
  subscribe_options =
      (rumqttc_subscribe_options_t)RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
  unsubscribe_options =
      (rumqttc_unsubscribe_options_t)RUMQTTC_UNSUBSCRIBE_OPTIONS_INIT;
  unsubscribe_options.v5_properties = &unsubscribe_properties;
  expect_unsubscribe_rejection(client, &unsubscribe_options,
                               RUMQTTC_ERROR_NONE);

  publish_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  publish_options.v5_properties = NULL;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_NONE);
  subscription.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  subscription.v5_options = NULL;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
  subscription = native_subscription("rumqttc/native/rejected", RUMQTTC_QOS_0);
  subscribe_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  subscribe_options.v5_properties = NULL;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
  unsubscribe_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  unsubscribe_options.v5_properties = NULL;
  expect_unsubscribe_rejection(client, &unsubscribe_options,
                               RUMQTTC_ERROR_NONE);
}

static void test_undersized_records(rumqttc_client_t *client) {
  rumqttc_user_property_t user_property = RUMQTTC_USER_PROPERTY_INIT;
  rumqttc_v5_publish_properties_t publish_properties =
      RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  rumqttc_publish_options_t publish_options = RUMQTTC_PUBLISH_OPTIONS_INIT;
  rumqttc_v5_subscription_options_t filter_options =
      RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT;
  rumqttc_subscription_t subscription =
      native_subscription("rumqttc/native/rejected", RUMQTTC_QOS_0);
  rumqttc_v5_subscribe_properties_t subscribe_properties =
      RUMQTTC_V5_SUBSCRIBE_PROPERTIES_INIT;
  rumqttc_subscribe_options_t subscribe_options =
      RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
  rumqttc_v5_unsubscribe_properties_t unsubscribe_properties =
      RUMQTTC_V5_UNSUBSCRIBE_PROPERTIES_INIT;
  rumqttc_unsubscribe_options_t unsubscribe_options =
      RUMQTTC_UNSUBSCRIBE_OPTIONS_INIT;

  publish_options.struct_size--;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_NONE);
  publish_options = (rumqttc_publish_options_t)RUMQTTC_PUBLISH_OPTIONS_INIT;
  publish_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  publish_options.v5_properties = &publish_properties;
  publish_properties.struct_size--;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_NONE);

  subscription.struct_size--;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
  subscription = native_subscription("rumqttc/native/rejected", RUMQTTC_QOS_0);
  subscription.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  subscription.v5_options = &filter_options;
  filter_options.struct_size--;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);

  subscription = native_subscription("rumqttc/native/rejected", RUMQTTC_QOS_0);
  subscribe_options.struct_size--;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
  subscribe_options =
      (rumqttc_subscribe_options_t)RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
  subscribe_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  subscribe_options.v5_properties = &subscribe_properties;
  subscribe_properties.struct_size--;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);

  unsubscribe_options.struct_size--;
  expect_unsubscribe_rejection(client, &unsubscribe_options,
                               RUMQTTC_ERROR_NONE);
  unsubscribe_options =
      (rumqttc_unsubscribe_options_t)RUMQTTC_UNSUBSCRIBE_OPTIONS_INIT;
  unsubscribe_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  unsubscribe_options.v5_properties = &unsubscribe_properties;
  unsubscribe_properties.struct_size--;
  expect_unsubscribe_rejection(client, &unsubscribe_options,
                               RUMQTTC_ERROR_NONE);

  user_property.struct_size--;
  subscribe_properties =
      (rumqttc_v5_subscribe_properties_t)RUMQTTC_V5_SUBSCRIBE_PROPERTIES_INIT;
  subscribe_properties.user_properties = &user_property;
  subscribe_properties.user_property_count = 1;
  subscribe_options.v5_properties = &subscribe_properties;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
}

static void test_invalid_boolean_and_retain_values(rumqttc_client_t *client) {
  rumqttc_v5_publish_properties_t publish_properties =
      RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  rumqttc_publish_options_t publish_options = RUMQTTC_PUBLISH_OPTIONS_INIT;
  rumqttc_v5_subscription_options_t filter_options =
      RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT;
  rumqttc_subscription_t subscription =
      native_subscription("rumqttc/native/rejected", RUMQTTC_QOS_0);
  rumqttc_v5_subscribe_properties_t subscribe_properties =
      RUMQTTC_V5_SUBSCRIBE_PROPERTIES_INIT;
  rumqttc_subscribe_options_t subscribe_options =
      RUMQTTC_SUBSCRIBE_OPTIONS_INIT;

  publish_options.retain = 2;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_NONE);
  publish_options = (rumqttc_publish_options_t)RUMQTTC_PUBLISH_OPTIONS_INIT;
  publish_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  publish_options.v5_properties = &publish_properties;
  publish_properties.response_topic_present = 2;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_NONE);
  publish_properties =
      (rumqttc_v5_publish_properties_t)RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  publish_properties.correlation_data_present = 2;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_NONE);
  publish_properties =
      (rumqttc_v5_publish_properties_t)RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  publish_properties.content_type_present = 2;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_NONE);
  publish_properties =
      (rumqttc_v5_publish_properties_t)RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  publish_properties.payload_format_present = 2;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_NONE);
  publish_properties =
      (rumqttc_v5_publish_properties_t)RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  publish_properties.message_expiry_present = 2;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_NONE);

  subscription.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  subscription.v5_options = &filter_options;
  filter_options.no_local = 2;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
  filter_options =
      (rumqttc_v5_subscription_options_t)RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT;
  filter_options.retain_as_published = 2;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
  filter_options =
      (rumqttc_v5_subscription_options_t)RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT;
  filter_options.retain_forward_rule = 3;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
  filter_options.retain_forward_rule = UINT32_MAX;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);

  subscription = native_subscription("rumqttc/native/rejected", RUMQTTC_QOS_0);
  subscribe_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  subscribe_options.v5_properties = &subscribe_properties;
  subscribe_properties.subscription_identifier_present = 2;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_NONE);
}

static void test_v5_options_rejected_by_v4(void) {
  rumqttc_client_t *client =
      native_start_client(RUMQTTC_PROTOCOL_V4, "native-v4-protocol-options",
                          RUMQTTC_ACK_AUTOMATIC, 16, 16, 1000);
  rumqttc_v5_publish_properties_t publish_properties =
      RUMQTTC_V5_PUBLISH_PROPERTIES_INIT;
  rumqttc_publish_options_t publish_options = RUMQTTC_PUBLISH_OPTIONS_INIT;
  rumqttc_v5_subscription_options_t filter_options =
      RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT;
  rumqttc_subscription_t subscription =
      native_subscription("rumqttc/native/rejected", RUMQTTC_QOS_0);
  rumqttc_v5_subscribe_properties_t subscribe_properties =
      RUMQTTC_V5_SUBSCRIBE_PROPERTIES_INIT;
  rumqttc_subscribe_options_t subscribe_options =
      RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
  rumqttc_v5_unsubscribe_properties_t unsubscribe_properties =
      RUMQTTC_V5_UNSUBSCRIBE_PROPERTIES_INIT;
  rumqttc_unsubscribe_options_t unsubscribe_options =
      RUMQTTC_UNSUBSCRIBE_OPTIONS_INIT;

  publish_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  publish_options.v5_properties = &publish_properties;
  expect_publish_rejection(client, &publish_options, RUMQTTC_ERROR_ADMISSION);

  subscribe_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  subscribe_options.v5_properties = &subscribe_properties;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_ADMISSION);
  subscribe_options =
      (rumqttc_subscribe_options_t)RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
  subscription.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  subscription.v5_options = &filter_options;
  expect_subscribe_rejection(client, &subscription, &subscribe_options,
                             RUMQTTC_ERROR_ADMISSION);

  unsubscribe_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  unsubscribe_options.v5_properties = &unsubscribe_properties;
  expect_unsubscribe_rejection(client, &unsubscribe_options,
                               RUMQTTC_ERROR_ADMISSION);
  native_close_destroy(client);
}

static void test_v5_options_reach_the_wire(void) {
  rumqttc_client_t *client =
      native_start_client(RUMQTTC_PROTOCOL_V5, "native-v5-protocol-options",
                          RUMQTTC_ACK_AUTOMATIC, 16, 16, 1000);
  rumqttc_v5_subscription_options_t default_filter_options =
      RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT;
  rumqttc_subscription_t default_subscription =
      native_subscription("rumqttc/native/v5/default", RUMQTTC_QOS_0);
  rumqttc_v5_subscribe_properties_t default_properties =
      RUMQTTC_V5_SUBSCRIBE_PROPERTIES_INIT;
  rumqttc_subscribe_options_t default_options = RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
  rumqttc_completion_t *completion = NULL;

  default_subscription.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  default_subscription.v5_options = &default_filter_options;
  default_options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
  default_options.v5_properties = &default_properties;
  CHECK(rumqttc_client_subscribe_tracked(client, &default_subscription, 1,
                                         &default_options, &completion, NULL));
  native_wait_completion(completion, RUMQTTC_COMPLETION_SUBSCRIBE);
  rumqttc_completion_destroy(completion);

  {
    rumqttc_user_property_t property = RUMQTTC_USER_PROPERTY_INIT;
    rumqttc_v5_subscription_options_t filter_options[3] = {
        RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT,
        RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT,
        RUMQTTC_V5_SUBSCRIPTION_OPTIONS_INIT,
    };
    rumqttc_subscription_t subscriptions[3] = {
        RUMQTTC_SUBSCRIPTION_INIT,
        RUMQTTC_SUBSCRIPTION_INIT,
        RUMQTTC_SUBSCRIPTION_INIT,
    };
    rumqttc_v5_subscribe_properties_t properties =
        RUMQTTC_V5_SUBSCRIBE_PROPERTIES_INIT;
    rumqttc_subscribe_options_t options = RUMQTTC_SUBSCRIBE_OPTIONS_INIT;
    size_t index;

    property.name = native_string("k");
    property.value = native_string("v");
    properties.subscription_identifier_present = 1;
    properties.subscription_identifier = 7;
    properties.user_properties = &property;
    properties.user_property_count = 1;
    options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
    options.v5_properties = &properties;

    subscriptions[0].filter = native_string("rumqttc/native/v5/options/0");
    subscriptions[0].qos = RUMQTTC_QOS_0;
    filter_options[0].no_local = 1;
    filter_options[0].retain_forward_rule = RUMQTTC_RETAIN_ON_EVERY_SUBSCRIBE;
    subscriptions[1].filter = native_string("rumqttc/native/v5/options/1");
    subscriptions[1].qos = RUMQTTC_QOS_1;
    filter_options[1].retain_as_published = 1;
    filter_options[1].retain_forward_rule = RUMQTTC_RETAIN_ON_NEW_SUBSCRIBE;
    subscriptions[2].filter = native_string("rumqttc/native/v5/options/2");
    subscriptions[2].qos = RUMQTTC_QOS_2;
    filter_options[2].retain_forward_rule = RUMQTTC_RETAIN_NEVER;
    for (index = 0; index < 3; ++index) {
      subscriptions[index].protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
      subscriptions[index].v5_options = &filter_options[index];
    }
    completion = NULL;
    CHECK(rumqttc_client_subscribe_tracked(client, subscriptions, 3, &options,
                                           &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_SUBSCRIBE);
    rumqttc_completion_destroy(completion);
  }

  {
    rumqttc_user_property_t property = RUMQTTC_USER_PROPERTY_INIT;
    rumqttc_v5_unsubscribe_properties_t properties =
        RUMQTTC_V5_UNSUBSCRIBE_PROPERTIES_INIT;
    rumqttc_unsubscribe_options_t options = RUMQTTC_UNSUBSCRIBE_OPTIONS_INIT;
    rumqttc_string_view_t filter = native_string("rumqttc/native/v5/options/2");
    property.name = native_string("u");
    property.value = native_string("p");
    properties.user_properties = &property;
    properties.user_property_count = 1;
    options.protocol_options = RUMQTTC_PROTOCOL_OPTIONS_V5;
    options.v5_properties = &properties;
    completion = NULL;
    CHECK(rumqttc_client_unsubscribe_tracked(client, &filter, 1, &options,
                                             &completion, NULL));
    native_wait_completion(completion, RUMQTTC_COMPLETION_UNSUBSCRIBE);
    rumqttc_completion_destroy(completion);
  }
  native_close_destroy(client);
}

void native_test_protocol_options(void) {
  rumqttc_client_t *client = native_start_client(
      RUMQTTC_PROTOCOL_V5, "native-invalid-protocol-options",
      RUMQTTC_ACK_AUTOMATIC, 16, 16, 1000);
  test_unknown_and_inconsistent_selectors(client);
  test_undersized_records(client);
  test_invalid_boolean_and_retain_values(client);
  native_close_destroy(client);
  test_v5_options_rejected_by_v4();
  test_v5_options_reach_the_wire();
}
