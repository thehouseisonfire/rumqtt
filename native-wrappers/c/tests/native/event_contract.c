#include "native_common.h"

static void expect_kind(rumqttc_status_t status, int matches) {
  REQUIRE(status == (matches ? RUMQTTC_OK : RUMQTTC_INVALID_STATE));
}

void native_check_event_accessors(const rumqttc_event_t *event) {
  rumqttc_event_kind_t kind = 0;
  CHECK(rumqttc_event_kind(event, &kind));
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
