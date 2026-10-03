#include "example_common.h"

#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* HOST PORT CA_PEM SPKI_SHA256_HEX. The pin supplements normal verification. */
int main(int argc, char **argv) {
  if (argc != 5) {
    fprintf(stderr, "usage: %s HOST PORT CA_PEM SPKI_SHA256_HEX\n", argv[0]);
    return 2;
  }
  char *end = NULL;
  unsigned long port = strtoul(argv[2], &end, 10);
  if (*argv[1] == '\0' || end == argv[2] || *end != '\0' || port == 0 || port > UINT16_MAX || strlen(argv[4]) != 64)
    return 2;
  rumqttc_tls_pin_t pin = RUMQTTC_TLS_PIN_INIT;
  pin.target = RUMQTTC_TLS_PIN_LEAF_SPKI;
  for (size_t i = 0; i < 32; ++i) {
    char pair[3] = {argv[4][i * 2], argv[4][i * 2 + 1], '\0'};
    if (!isxdigit((unsigned char)pair[0]) || !isxdigit((unsigned char)pair[1])) return 2;
    unsigned long byte = strtoul(pair, &end, 16);
    if (end != pair + 2 || byte > UINT8_MAX) return 2;
    pin.sha256[i] = (uint8_t)byte;
  }
  rumqttc_tls_backend_capabilities_t capabilities = RUMQTTC_TLS_BACKEND_CAPABILITIES_INIT;
  rumqttc_error_t *error = NULL;
  if (example_report(rumqttc_tls_backend_capabilities(RUMQTTC_TLS_BACKEND_RUSTLS, &capabilities, &error),
                     &error, "tls_backend_capabilities")) return 1;
  if (!(capabilities.pin_target_mask & (1u << RUMQTTC_TLS_PIN_LEAF_SPKI))) return 77;
  FILE *file = fopen(argv[3], "rb");
  if (file == NULL) return 2;
  uint8_t ca[65536];
  size_t length = fread(ca, 1, sizeof(ca), file);
  int extra = fgetc(file);
  int invalid = ferror(file) || length == 0 || extra != EOF;
  fclose(file);
  if (invalid) return 2;

  rumqttc_tls_options_t tls = RUMQTTC_TLS_OPTIONS_INIT;
  tls.root_policy = RUMQTTC_TLS_ROOTS_PEM;
  tls.ca_pem = example_bytes(ca, length);
  rumqttc_tls_profile_options_t options = RUMQTTC_TLS_PROFILE_OPTIONS_INIT;
  options.tls = &tls;
  options.version_policy = RUMQTTC_TLS_VERSION_13_ONLY;
  options.pins = &pin;
  options.pin_count = 1;
  rumqttc_tls_profile_t *profile = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_event_t *event = NULL;
  int result = 1;
  if (example_report(rumqttc_tls_profile_new(&options, &profile, &error), &error, "tls_profile_new") ||
      example_report(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, &error), &error, "config_new") ||
      example_report(rumqttc_config_set_broker(config, example_string(argv[1]), (uint16_t)port, &error), &error, "set_broker") ||
      example_report(rumqttc_config_set_client_id(config, example_string("c-tls-profile"), &error), &error, "set_client_id") ||
      example_report(rumqttc_config_set_transport_tls_with_profile(config, profile, &error), &error, "set_tls_profile"))
    goto cleanup;
  rumqttc_tls_profile_destroy(profile);
  profile = NULL;
  memset(ca, 0, sizeof(ca));
  if (example_report(rumqttc_client_start(config, &client, &error), &error, "client_start")) goto cleanup;
  event = example_next_event(client, RUMQTTC_EVENT_CONNECTED);
  if (event == NULL) goto cleanup;
  if (example_report(rumqttc_client_close_timeout_ms(client, 5000, &error), &error, "close")) goto cleanup;
  result = 0;
cleanup:
  rumqttc_event_destroy(event);
  rumqttc_tls_profile_destroy(profile);
  rumqttc_config_destroy(config);
  rumqttc_error_destroy(error);
  example_destroy_client(&client);
  return result;
}
