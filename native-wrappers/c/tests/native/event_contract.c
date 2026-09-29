#include "native_common.h"

static void expect_kind(rumqttc_status_t status, int matches) {
  REQUIRE(status == (matches ? RUMQTTC_OK : RUMQTTC_INVALID_STATE));
}

static unsigned output_mask(unsigned index, unsigned count) {
  unsigned all = (1u << count) - 1;
  if (index == 0)
    return all;
  if (index <= count)
    return 1u << (index - 1);
  return all & ~(1u << (index - count - 1));
}

static void check_connack_outputs(const rumqttc_event_t *event, int matches) {
  for (unsigned strings = 0; strings < 2; ++strings) {
    uint32_t maximum = strings ? 5 : 10;
    for (uint32_t selector = 0; selector <= maximum + 1; ++selector) {
      uint8_t present = UINT8_MAX;
      uint64_t value = UINT64_MAX;
      rumqttc_string_view_t text = {(const char *)(uintptr_t)1, SIZE_MAX};
      rumqttc_status_t expected = !matches                              ? RUMQTTC_INVALID_STATE
                                  : selector == 0 || selector > maximum ? RUMQTTC_INVALID_ARGUMENT
                                                                        : RUMQTTC_OK;
      rumqttc_status_t status = strings ? rumqttc_event_connack_v5_string(event, selector, &present, &text)
                                        : rumqttc_event_connack_v5_scalar(event, selector, &present, &value);
      REQUIRE(status == expected);
      if (status != RUMQTTC_OK)
        REQUIRE(present == 0 && (strings ? text.data == NULL && text.len == 0 : value == 0));
      for (unsigned mask = 1; mask <= 3; ++mask) {
        uint8_t optional_present = UINT8_MAX;
        uint64_t optional_value = UINT64_MAX;
        rumqttc_string_view_t optional_text = {(const char *)(uintptr_t)1, SIZE_MAX};
        status = strings ? rumqttc_event_connack_v5_string(event, selector, mask & 1 ? &optional_present : NULL,
                                                           mask & 2 ? &optional_text : NULL)
                         : rumqttc_event_connack_v5_scalar(event, selector, mask & 1 ? &optional_present : NULL,
                                                           mask & 2 ? &optional_value : NULL);
        REQUIRE(status == expected);
        if (mask & 1)
          REQUIRE(optional_present == present);
        if (mask & 2)
          REQUIRE(strings ? optional_text.data == text.data && optional_text.len == text.len : optional_value == value);
      }
      REQUIRE((strings ? rumqttc_event_connack_v5_string(event, selector, NULL, NULL)
                       : rumqttc_event_connack_v5_scalar(event, selector, NULL, NULL)) == RUMQTTC_INVALID_ARGUMENT);
    }
    uint8_t present = UINT8_MAX;
    uint64_t value = UINT64_MAX;
    rumqttc_string_view_t text = {(const char *)(uintptr_t)1, SIZE_MAX};
    REQUIRE((strings ? rumqttc_event_connack_v5_string(event, UINT32_MAX, &present, &text)
                     : rumqttc_event_connack_v5_scalar(event, UINT32_MAX, &present, &value)) ==
            (matches ? RUMQTTC_INVALID_ARGUMENT : RUMQTTC_INVALID_STATE));
    REQUIRE(present == 0 && (strings ? text.data == NULL && text.len == 0 : value == 0));
    for (unsigned mask = 1; mask <= 3; ++mask) {
      present = UINT8_MAX;
      value = UINT64_MAX;
      text.data = (const char *)(uintptr_t)1;
      text.len = SIZE_MAX;
      REQUIRE((strings ? rumqttc_event_connack_v5_string(NULL, 1, mask & 1 ? &present : NULL, mask & 2 ? &text : NULL)
                       : rumqttc_event_connack_v5_scalar(NULL, 1, mask & 1 ? &present : NULL,
                                                         mask & 2 ? &value : NULL)) == RUMQTTC_INVALID_ARGUMENT);
      if (mask & 1)
        REQUIRE(present == 0);
      if (mask & 2)
        REQUIRE(strings ? text.data == NULL && text.len == 0 : value == 0);
    }
  }
}

static void check_authentication_outputs(const rumqttc_event_t *event, int matches) {
  uint32_t exchange = UINT32_MAX;
  uint32_t stage = UINT32_MAX;
  uint8_t failure_present = UINT8_MAX;
  uint32_t failure = UINT32_MAX;
  rumqttc_string_view_t method = {(const char *)(uintptr_t)1, SIZE_MAX};
  rumqttc_status_t expected = matches ? RUMQTTC_OK : RUMQTTC_INVALID_STATE;
  REQUIRE(rumqttc_event_authentication(event, &exchange, &stage, &failure_present, &failure, &method) == expected);
  if (!matches) {
    REQUIRE(exchange == 0);
    REQUIRE(stage == 0);
    REQUIRE(failure_present == 0);
    REQUIRE(failure == 0);
    REQUIRE(method.data == NULL && method.len == 0);
  }
  for (unsigned index = 0; index <= 10; ++index) {
    unsigned mask = output_mask(index, 5);
    uint32_t optional_exchange = UINT32_MAX;
    uint32_t optional_stage = UINT32_MAX;
    uint8_t optional_failure_present = UINT8_MAX;
    uint32_t optional_failure = UINT32_MAX;
    rumqttc_string_view_t optional_method = {(const char *)(uintptr_t)1, SIZE_MAX};
    REQUIRE(rumqttc_event_authentication(event, mask & 1 ? &optional_exchange : NULL, mask & 2 ? &optional_stage : NULL,
                                         mask & 4 ? &optional_failure_present : NULL,
                                         mask & 8 ? &optional_failure : NULL,
                                         mask & 16 ? &optional_method : NULL) == expected);
    if (mask & 1)
      REQUIRE(optional_exchange == exchange);
    if (mask & 2)
      REQUIRE(optional_stage == stage);
    if (mask & 4)
      REQUIRE(optional_failure_present == failure_present);
    if (mask & 8)
      REQUIRE(optional_failure == failure);
    if (mask & 16)
      REQUIRE(optional_method.data == method.data && optional_method.len == method.len);
  }
  REQUIRE(rumqttc_event_authentication(event, NULL, NULL, NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_event_authentication(NULL, &exchange, &stage, &failure_present, &failure, &method) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(exchange == 0);
  REQUIRE(stage == 0);
  REQUIRE(failure_present == 0);
  REQUIRE(failure == 0);
  REQUIRE(method.data == NULL && method.len == 0);
}

static void check_authentication_details_outputs(const rumqttc_event_t *event, int matches) {
  uint8_t reason_present = UINT8_MAX;
  uint8_t reason = UINT8_MAX;
  uint8_t properties_present = UINT8_MAX;
  uint8_t method_present = UINT8_MAX;
  rumqttc_string_view_t method = {(const char *)(uintptr_t)1, SIZE_MAX};
  uint8_t data_present = UINT8_MAX;
  rumqttc_bytes_view_t data = {(const uint8_t *)(uintptr_t)1, SIZE_MAX};
  uint8_t text_present = UINT8_MAX;
  rumqttc_string_view_t text = {(const char *)(uintptr_t)1, SIZE_MAX};
  rumqttc_status_t expected = matches ? RUMQTTC_OK : RUMQTTC_INVALID_STATE;
  REQUIRE(rumqttc_event_authentication_details(event, &reason_present, &reason, &properties_present, &method_present,
                                               &method, &data_present, &data, &text_present, &text) == expected);
  if (!matches) {
    REQUIRE(reason_present == 0);
    REQUIRE(reason == 0);
    REQUIRE(properties_present == 0);
    REQUIRE(method_present == 0);
    REQUIRE(method.data == NULL && method.len == 0);
    REQUIRE(data_present == 0);
    REQUIRE(data.data == NULL && data.len == 0);
    REQUIRE(text_present == 0);
    REQUIRE(text.data == NULL && text.len == 0);
  }
  for (unsigned index = 0; index <= 18; ++index) {
    unsigned mask = output_mask(index, 9);
    uint8_t optional_reason_present = UINT8_MAX;
    uint8_t optional_reason = UINT8_MAX;
    uint8_t optional_properties_present = UINT8_MAX;
    uint8_t optional_method_present = UINT8_MAX;
    rumqttc_string_view_t optional_method = {(const char *)(uintptr_t)1, SIZE_MAX};
    uint8_t optional_data_present = UINT8_MAX;
    rumqttc_bytes_view_t optional_data = {(const uint8_t *)(uintptr_t)1, SIZE_MAX};
    uint8_t optional_text_present = UINT8_MAX;
    rumqttc_string_view_t optional_text = {(const char *)(uintptr_t)1, SIZE_MAX};
    REQUIRE(rumqttc_event_authentication_details(
                event, mask & 1 ? &optional_reason_present : NULL, mask & 2 ? &optional_reason : NULL,
                mask & 4 ? &optional_properties_present : NULL, mask & 8 ? &optional_method_present : NULL,
                mask & 16 ? &optional_method : NULL, mask & 32 ? &optional_data_present : NULL,
                mask & 64 ? &optional_data : NULL, mask & 128 ? &optional_text_present : NULL,
                mask & 256 ? &optional_text : NULL) == expected);
    if (mask & 1)
      REQUIRE(optional_reason_present == reason_present);
    if (mask & 2)
      REQUIRE(optional_reason == reason);
    if (mask & 4)
      REQUIRE(optional_properties_present == properties_present);
    if (mask & 8)
      REQUIRE(optional_method_present == method_present);
    if (mask & 16)
      REQUIRE(optional_method.data == method.data && optional_method.len == method.len);
    if (mask & 32)
      REQUIRE(optional_data_present == data_present);
    if (mask & 64)
      REQUIRE(optional_data.data == data.data && optional_data.len == data.len);
    if (mask & 128)
      REQUIRE(optional_text_present == text_present);
    if (mask & 256)
      REQUIRE(optional_text.data == text.data && optional_text.len == text.len);
  }
  REQUIRE(rumqttc_event_authentication_details(event, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL) ==
          RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_event_authentication_details(NULL, &reason_present, &reason, &properties_present, &method_present,
                                               &method, &data_present, &data, &text_present,
                                               &text) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(reason_present == 0);
  REQUIRE(reason == 0);
  REQUIRE(properties_present == 0);
  REQUIRE(method_present == 0);
  REQUIRE(method.data == NULL && method.len == 0);
  REQUIRE(data_present == 0);
  REQUIRE(data.data == NULL && data.len == 0);
  REQUIRE(text_present == 0);
  REQUIRE(text.data == NULL && text.len == 0);
}

static void check_broker_disconnect_outputs(const rumqttc_event_t *event, int matches) {
  uint8_t reason = UINT8_MAX;
  uint8_t expiry_present = UINT8_MAX;
  uint32_t expiry = UINT32_MAX;
  uint8_t text_present = UINT8_MAX;
  rumqttc_string_view_t text = {(const char *)(uintptr_t)1, SIZE_MAX};
  uint8_t reference_present = UINT8_MAX;
  rumqttc_string_view_t reference = {(const char *)(uintptr_t)1, SIZE_MAX};
  rumqttc_status_t expected = matches ? RUMQTTC_OK : RUMQTTC_INVALID_STATE;
  REQUIRE(rumqttc_event_broker_disconnect(event, &reason, &expiry_present, &expiry, &text_present, &text,
                                          &reference_present, &reference) == expected);
  if (!matches) {
    REQUIRE(reason == 0);
    REQUIRE(expiry_present == 0);
    REQUIRE(expiry == 0);
    REQUIRE(text_present == 0);
    REQUIRE(text.data == NULL && text.len == 0);
    REQUIRE(reference_present == 0);
    REQUIRE(reference.data == NULL && reference.len == 0);
  }
  for (unsigned index = 0; index <= 14; ++index) {
    unsigned mask = output_mask(index, 7);
    uint8_t optional_reason = UINT8_MAX;
    uint8_t optional_expiry_present = UINT8_MAX;
    uint32_t optional_expiry = UINT32_MAX;
    uint8_t optional_text_present = UINT8_MAX;
    rumqttc_string_view_t optional_text = {(const char *)(uintptr_t)1, SIZE_MAX};
    uint8_t optional_reference_present = UINT8_MAX;
    rumqttc_string_view_t optional_reference = {(const char *)(uintptr_t)1, SIZE_MAX};
    REQUIRE(rumqttc_event_broker_disconnect(
                event, mask & 1 ? &optional_reason : NULL, mask & 2 ? &optional_expiry_present : NULL,
                mask & 4 ? &optional_expiry : NULL, mask & 8 ? &optional_text_present : NULL,
                mask & 16 ? &optional_text : NULL, mask & 32 ? &optional_reference_present : NULL,
                mask & 64 ? &optional_reference : NULL) == expected);
    if (mask & 1)
      REQUIRE(optional_reason == reason);
    if (mask & 2)
      REQUIRE(optional_expiry_present == expiry_present);
    if (mask & 4)
      REQUIRE(optional_expiry == expiry);
    if (mask & 8)
      REQUIRE(optional_text_present == text_present);
    if (mask & 16)
      REQUIRE(optional_text.data == text.data && optional_text.len == text.len);
    if (mask & 32)
      REQUIRE(optional_reference_present == reference_present);
    if (mask & 64)
      REQUIRE(optional_reference.data == reference.data && optional_reference.len == reference.len);
  }
  REQUIRE(rumqttc_event_broker_disconnect(event, NULL, NULL, NULL, NULL, NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_event_broker_disconnect(NULL, &reason, &expiry_present, &expiry, &text_present, &text,
                                          &reference_present, &reference) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(reason == 0);
  REQUIRE(expiry_present == 0);
  REQUIRE(expiry == 0);
  REQUIRE(text_present == 0);
  REQUIRE(text.data == NULL && text.len == 0);
  REQUIRE(reference_present == 0);
  REQUIRE(reference.data == NULL && reference.len == 0);
}

static void check_connack_v5_authentication_data_outputs(const rumqttc_event_t *event, int matches) {
  uint8_t present = UINT8_MAX;
  rumqttc_bytes_view_t data = {(const uint8_t *)(uintptr_t)1, SIZE_MAX};
  rumqttc_status_t expected = matches ? RUMQTTC_OK : RUMQTTC_INVALID_STATE;
  REQUIRE(rumqttc_event_connack_v5_authentication_data(event, &present, &data) == expected);
  if (!matches) {
    REQUIRE(present == 0);
    REQUIRE(data.data == NULL && data.len == 0);
  }
  for (unsigned index = 0; index <= 4; ++index) {
    unsigned mask = output_mask(index, 2);
    uint8_t optional_present = UINT8_MAX;
    rumqttc_bytes_view_t optional_data = {(const uint8_t *)(uintptr_t)1, SIZE_MAX};
    REQUIRE(rumqttc_event_connack_v5_authentication_data(event, mask & 1 ? &optional_present : NULL,
                                                         mask & 2 ? &optional_data : NULL) == expected);
    if (mask & 1)
      REQUIRE(optional_present == present);
    if (mask & 2)
      REQUIRE(optional_data.data == data.data && optional_data.len == data.len);
  }
  REQUIRE(rumqttc_event_connack_v5_authentication_data(event, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(rumqttc_event_connack_v5_authentication_data(NULL, &present, &data) == RUMQTTC_INVALID_ARGUMENT);
  REQUIRE(present == 0);
  REQUIRE(data.data == NULL && data.len == 0);
}

void native_check_event_accessors(const rumqttc_event_t *event) {
  rumqttc_event_kind_t kind = 0;
  CHECK(rumqttc_event_kind(event, &kind));
  /* Rejected CONNACKs retain the same properties. An absent property record
     returns INVALID_STATE even for a successful MQTT 5 CONNACK. */
  uint8_t probe_present = 0;
  uint64_t probe_value = 0;
  rumqttc_status_t probe = rumqttc_event_connack_v5_scalar(event, 2, &probe_present, &probe_value);
  REQUIRE(probe == RUMQTTC_OK || probe == RUMQTTC_INVALID_STATE);
  int connack = probe == RUMQTTC_OK;
  if (connack)
    REQUIRE(kind == RUMQTTC_EVENT_CONNECTED || kind == RUMQTTC_EVENT_CONNECTION_REJECTED);
  check_connack_outputs(event, connack);
  check_connack_v5_authentication_data_outputs(event, connack);
  check_authentication_outputs(event, kind == RUMQTTC_EVENT_AUTHENTICATION);
  check_authentication_details_outputs(event, kind == RUMQTTC_EVENT_AUTHENTICATION);
  check_broker_disconnect_outputs(event, kind == RUMQTTC_EVENT_BROKER_DISCONNECT);
  uint32_t scalar = UINT32_MAX;
  uint8_t present = UINT8_MAX;
  rumqttc_string_view_t text = {(const char *)(uintptr_t)1, SIZE_MAX};
  expect_kind(rumqttc_event_connected(event, &scalar, &present), kind == RUMQTTC_EVENT_CONNECTED);
  if (kind != RUMQTTC_EVENT_CONNECTED)
    REQUIRE(scalar == 0 && present == 0);
  REQUIRE(rumqttc_event_connected(event, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  uint64_t connack_value = UINT64_MAX;
  uint8_t connack_present = UINT8_MAX;
  rumqttc_status_t connack_status = rumqttc_event_connack_v5_scalar(event, 2, &connack_present, &connack_value);
  REQUIRE(connack_status == RUMQTTC_OK || connack_status == RUMQTTC_INVALID_STATE);
  if (connack_status != RUMQTTC_OK)
    REQUIRE(connack_present == 0 && connack_value == 0);

  scalar = UINT32_MAX;
  present = UINT8_MAX;
  expect_kind(rumqttc_event_authentication(event, &scalar, NULL, &present, NULL, &text),
              kind == RUMQTTC_EVENT_AUTHENTICATION);
  if (kind != RUMQTTC_EVENT_AUTHENTICATION)
    REQUIRE(scalar == 0 && present == 0 && text.data == NULL && text.len == 0);
  REQUIRE(rumqttc_event_authentication(event, NULL, NULL, NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  present = UINT8_MAX;
  rumqttc_bytes_view_t data = {(const uint8_t *)(uintptr_t)1, SIZE_MAX};
  expect_kind(rumqttc_event_authentication_details(event, &present, NULL, NULL, NULL, NULL, NULL, &data, NULL, NULL),
              kind == RUMQTTC_EVENT_AUTHENTICATION);
  if (kind != RUMQTTC_EVENT_AUTHENTICATION)
    REQUIRE(present == 0 && data.data == NULL && data.len == 0);

  scalar = UINT32_MAX;
  present = UINT8_MAX;
  text = (rumqttc_string_view_t){(const char *)(uintptr_t)1, SIZE_MAX};
  expect_kind(rumqttc_event_redirect(event, &scalar, NULL, &present, NULL, NULL, &text),
              kind == RUMQTTC_EVENT_REDIRECT);
  if (kind != RUMQTTC_EVENT_REDIRECT)
    REQUIRE(scalar == 0 && present == 0 && text.data == NULL && text.len == 0);
  REQUIRE(rumqttc_event_redirect(event, NULL, NULL, NULL, NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);
  uint16_t port = UINT16_MAX;
  present = UINT8_MAX;
  text = (rumqttc_string_view_t){(const char *)(uintptr_t)1, SIZE_MAX};
  expect_kind(rumqttc_event_redirect_target(event, &present, NULL, &text, &port), kind == RUMQTTC_EVENT_REDIRECT);
  if (kind != RUMQTTC_EVENT_REDIRECT)
    REQUIRE(present == 0 && text.data == NULL && text.len == 0 && port == 0);

  present = UINT8_MAX;
  scalar = UINT32_MAX;
  text = (rumqttc_string_view_t){(const char *)(uintptr_t)1, SIZE_MAX};
  expect_kind(rumqttc_event_broker_disconnect(event, NULL, &present, &scalar, NULL, &text, NULL, NULL),
              kind == RUMQTTC_EVENT_BROKER_DISCONNECT);
  if (kind != RUMQTTC_EVENT_BROKER_DISCONNECT)
    REQUIRE(present == 0 && scalar == 0 && text.data == NULL && text.len == 0);
  REQUIRE(rumqttc_event_broker_disconnect(event, NULL, NULL, NULL, NULL, NULL, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);

  present = UINT8_MAX;
  uint16_t packet_id = UINT16_MAX;
  expect_kind(rumqttc_event_outgoing_packet_id(event, &present, &packet_id), kind == RUMQTTC_EVENT_OUTGOING);
  if (kind != RUMQTTC_EVENT_OUTGOING)
    REQUIRE(present == 0 && packet_id == 0);
  REQUIRE(rumqttc_event_outgoing_packet_id(event, NULL, NULL) == RUMQTTC_INVALID_ARGUMENT);

  for (uint32_t property_class = RUMQTTC_EVENT_PROPERTIES_CONNACK;
       property_class <= RUMQTTC_EVENT_PROPERTIES_AUTHENTICATION; ++property_class) {
    int matches = property_class == RUMQTTC_EVENT_PROPERTIES_CONNACK ? connack_status == RUMQTTC_OK
                  : property_class == RUMQTTC_EVENT_PROPERTIES_BROKER_DISCONNECT
                      ? kind == RUMQTTC_EVENT_BROKER_DISCONNECT
                      : kind == RUMQTTC_EVENT_AUTHENTICATION;
    size_t count = SIZE_MAX;
    expect_kind(rumqttc_event_user_property_count(event, property_class, &count), matches);
    if (!matches)
      REQUIRE(count == 0);
    rumqttc_string_view_t name = {(const char *)(uintptr_t)1, SIZE_MAX};
    rumqttc_string_view_t value = {(const char *)(uintptr_t)1, SIZE_MAX};
    REQUIRE(rumqttc_event_user_property_at(event, property_class, count, &name, &value) ==
            (matches ? RUMQTTC_INVALID_ARGUMENT : RUMQTTC_INVALID_STATE));
    REQUIRE(name.data == NULL && name.len == 0 && value.data == NULL && value.len == 0);
    for (size_t index = 0; index < count; ++index) {
      CHECK(rumqttc_event_user_property_at(event, property_class, index, &name, &value));
      rumqttc_string_view_t optional = {NULL, 0};
      CHECK(rumqttc_event_user_property_at(event, property_class, index, &optional, NULL));
      REQUIRE(optional.data == name.data && optional.len == name.len);
      CHECK(rumqttc_event_user_property_at(event, property_class, index, NULL, &optional));
      REQUIRE(optional.data == value.data && optional.len == value.len);
    }
  }
}
