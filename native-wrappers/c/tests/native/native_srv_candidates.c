#include "native_common.h"

#include <stdlib.h>
#include <string.h>

typedef struct resolver_context {
  unsigned mode;
  unsigned calls;
  unsigned destroyed;
} resolver_context;

static uint16_t port(const char *name) {
  const char *value = getenv(name);
  REQUIRE(value != NULL);
  unsigned long parsed = strtoul(value, NULL, 10);
  REQUIRE(parsed > 0 && parsed <= UINT16_MAX);
  return (uint16_t)parsed;
}

static void resolve(void *data, const rumqttc_resolver_request_t *request, rumqttc_callback_completion_t *completion) {
  resolver_context *context = data;
  rumqttc_srv_record_t records[4] = {RUMQTTC_SRV_RECORD_INIT, RUMQTTC_SRV_RECORD_INIT, RUMQTTC_SRV_RECORD_INIT,
                                     RUMQTTC_SRV_RECORD_INIT};
  REQUIRE(request->owner.len >= strlen("_mqtt._tcp.service.invalid"));
  REQUIRE(memcmp(request->owner.data, "_mqtt._tcp.service.invalid", strlen("_mqtt._tcp.service.invalid")) == 0);
  ++context->calls;
  records[0].target = native_string("bad target");
  records[0].port = native_test_port();
  records[1].target = native_string("mqtt://bad");
  records[1].port = 0;
  REQUIRE(rumqttc_callback_srv_complete(completion, RUMQTTC_SRV_SUCCESS, records, 4) == RUMQTTC_INVALID_ARGUMENT);
  records[1].port = native_test_port();
  records[2].priority = 1;
  records[2].target = native_string("localhost.");
  records[2].port = port("RUMQTTC_TEST_REFUSED_PORT_1");
  records[3].priority = 2;
  records[3].target = native_string("localhost.");
  records[3].port =
      (context->mode == 0 || context->mode == 3) ? native_test_port() : port("RUMQTTC_TEST_REFUSED_PORT_2");
  if (context->mode == 3) {
    records[3].priority = records[2].priority;
    records[3].weight = UINT16_MAX;
  }
  CHECK(rumqttc_callback_srv_complete(completion, RUMQTTC_SRV_SUCCESS, records, context->mode == 2 ? 2 : 4));
  REQUIRE(rumqttc_callback_srv_complete(completion, RUMQTTC_SRV_SUCCESS, records, 4) == RUMQTTC_INVALID_STATE);
}

static void destroy(void *data) { ++((resolver_context *)data)->destroyed; }

static unsigned run(unsigned mode) {
  resolver_context context = {mode, 0, 0};
  rumqttc_resolver_vtable_t vtable = RUMQTTC_RESOLVER_VTABLE_INIT;
  rumqttc_resolver_registration_t *registration = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  const char *ids[] = {"native-v5-srv-fallback", "native-v5-srv-exhaust", "native-v5-srv-unusable",
                       "native-v5-srv-weighted"};
  unsigned finished = 0, terminal_failure = 0, selected = 0, first_candidate = 0;
  vtable.resolve = resolve;
  vtable.destroy = destroy;
  CHECK(rumqttc_resolver_registration_new(&vtable, &context, &registration, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string(ids[mode]), NULL));
  CHECK(rumqttc_config_set_v5_redirect_policy(config, RUMQTTC_REDIRECT_FOLLOW, 3, RUMQTTC_REDIRECT_TRANSPORT_TCP, NULL,
                                              NULL));
  CHECK(rumqttc_config_set_v5_srv_resolver(config, registration, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  for (unsigned i = 0; i < 12 && !finished; ++i) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS, &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    native_check_event_accessors(event);
    if (kind == RUMQTTC_EVENT_REDIRECT) {
      uint8_t failed = 0, candidate = 0, target_present = 0;
      uint32_t failure = 0;
      uint64_t attempts = 0, index = 0, count = 0, visited = 0;
      rumqttc_string_view_t target = {NULL, 0};
      uint16_t target_port = 0;
      CHECK(rumqttc_event_redirect(event, NULL, NULL, &failed, &failure, NULL, NULL));
      CHECK(rumqttc_event_redirect_diagnostics(event, NULL, &attempts, NULL, NULL, &visited, NULL, &candidate, &index,
                                               &count));
      CHECK(rumqttc_event_redirect_target(event, &target_present, NULL, &target, &target_port));
      REQUIRE(attempts <= 1);
      if (target_present || failed) {
        REQUIRE(attempts == 1);
        if (mode == 2) {
          REQUIRE(failed && failure == RUMQTTC_REDIRECT_FAILURE_DNS && !candidate);
        } else {
          REQUIRE(candidate && count == 2);
          if (mode == 3) {
            REQUIRE((index == 1 || index == 2) && visited == index + 1);
            first_candidate = index == 1;
          } else {
            REQUIRE(index == 2 && visited == 3);
          }
          if (failed)
            REQUIRE(mode == 1 && failure == RUMQTTC_REDIRECT_FAILURE_TRANSPORT);
          else {
            REQUIRE((mode == 0 || mode == 3) && target_present && target.len == 9 &&
                    memcmp(target.data, "localhost", 9) == 0);
            REQUIRE(target_port == native_test_port());
            selected = 1;
          }
        }
        terminal_failure = failed;
      }
    } else if (kind == RUMQTTC_EVENT_CONNECTED) {
      REQUIRE((mode == 0 || mode == 3) && selected && !terminal_failure);
      finished = 1;
    } else if (kind == RUMQTTC_EVENT_DRIVER_TERMINATED) {
      REQUIRE(mode != 0 && mode != 3 && terminal_failure);
      finished = 1;
    }
    rumqttc_event_destroy(event);
  }
  REQUIRE(finished && context.calls == 1);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
  rumqttc_resolver_registration_destroy(registration);
  REQUIRE(context.destroyed == 1);
  return first_candidate;
}

int main(void) {
  for (unsigned mode = 0; mode < 3; ++mode)
    (void)run(mode);
  unsigned heavy_first = 0;
  for (unsigned sample = 0; sample < 32; ++sample)
    heavy_first += run(3);
  /* RFC 2782 allows the inclusive zero draw to choose the zero-weight record.
   * Do not require every draw to choose the heavy record; seeded Rust tests
   * cover that edge exactly. This also catches discarded C weight fields. */
  REQUIRE(heavy_first >= 30);
  return 0;
}
