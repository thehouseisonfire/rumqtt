#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

typedef struct resolver_context {
  uint16_t port;
  atomic_uintptr_t worker;
  atomic_uint calls;
} resolver_context;

typedef struct resolve_work {
  rumqttc_callback_completion_t *completion;
  uint16_t port;
} resolve_work;

static atomic_uint destroyed;

static int finish_resolution(void *argument) {
  resolve_work *work = (resolve_work *)argument;
  rumqttc_srv_record_t records[] = {
      RUMQTTC_SRV_RECORD_INIT,
      RUMQTTC_SRV_RECORD_INIT,
      RUMQTTC_SRV_RECORD_INIT,
  };
  native_sleep_ms(20);
  records[0].priority = 10;
  records[0].weight = 5;
  records[0].port = work->port;
  records[0].target = native_string("localhost");
  records[1].priority = 10;
  records[1].weight = 25;
  records[1].port = work->port;
  records[1].target = native_string("localhost");
  records[2].priority = 20;
  records[2].weight = 1;
  records[2].port = work->port;
  records[2].target = native_string("later.invalid");
  REQUIRE(rumqttc_callback_srv_complete(work->completion, RUMQTTC_SRV_SUCCESS,
                                        records, 3) == RUMQTTC_OK);
  REQUIRE(rumqttc_callback_srv_complete(work->completion, RUMQTTC_SRV_SUCCESS,
                                        records, 3) == RUMQTTC_INVALID_STATE);
  rumqttc_callback_completion_destroy(work->completion);
  free(work);
  return 0;
}

static void resolve(void *user_data, const rumqttc_resolver_request_t *request,
                    rumqttc_callback_completion_t *completion) {
  resolver_context *context = (resolver_context *)user_data;
  rumqttc_callback_completion_t *retained = NULL;
  resolve_work *work;
  native_thread_t *worker;
  REQUIRE(request->owner.len >= strlen("_mqtt._tcp.service.invalid") &&
          request->owner.len <= strlen("_mqtt._tcp.service.invalid.") &&
          memcmp(request->owner.data, "_mqtt._tcp.service.invalid",
                 strlen("_mqtt._tcp.service.invalid")) == 0);
  atomic_fetch_add(&context->calls, 1);
  CHECK(rumqttc_callback_completion_retain(completion, &retained));
  work = malloc(sizeof(*work));
  REQUIRE(work != NULL);
  work->completion = retained;
  work->port = context->port;
  worker = native_thread_start(finish_resolution, work);
  REQUIRE(worker != NULL);
  REQUIRE(atomic_exchange(&context->worker, (uintptr_t)worker) == 0);
}

static void resolver_destroy(void *user_data) {
  free(user_data);
  atomic_fetch_add(&destroyed, 1);
}

int main(void) {
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_resolver_registration_t *registration = NULL;
  rumqttc_resolver_vtable_t vtable = RUMQTTC_RESOLVER_VTABLE_INIT;
  resolver_context *context = calloc(1, sizeof(*context));
  native_thread_t *worker;
  unsigned saw_target = 0;
  unsigned saw_connected = 0;
  REQUIRE(context != NULL);
  context->port = native_test_port();
  vtable.resolve = resolve;
  vtable.destroy = resolver_destroy;
  CHECK(rumqttc_resolver_registration_new(&vtable, context, &registration,
                                           NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  context->port, NULL));
  CHECK(rumqttc_config_set_client_id(config,
                                     native_string("native-v5-srv-redirect"), NULL));
  CHECK(rumqttc_config_set_v5_redirect_policy(config, RUMQTTC_REDIRECT_FOLLOW,
                                               3, RUMQTTC_REDIRECT_TRANSPORT_TCP,
                                               NULL, NULL));
  CHECK(rumqttc_config_set_v5_srv_resolver(config, registration, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  while (!saw_connected) {
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    CHECK(rumqttc_client_event_recv_timeout_ms(client, NATIVE_DEADLINE_MS,
                                               &event, NULL));
    CHECK(rumqttc_event_kind(event, &kind));
    if (kind == RUMQTTC_EVENT_REDIRECT) {
      uint32_t source = 0;
      uint32_t reason = 0;
      uint8_t failure_present = 0;
      uint32_t failure = 0;
      uint32_t decision = 0;
      uint64_t attempts = 0;
      uint64_t visited = 0;
      uint8_t candidate_present = 0;
      uint64_t candidate_index = 0;
      uint64_t candidate_count = 0;
      uint8_t target_present = 0;
      uint32_t target_kind = 0;
      rumqttc_string_view_t target = {NULL, 0};
      uint16_t port = 0;
      CHECK(rumqttc_event_redirect(event, &source, &reason, &failure_present, &failure,
                                   NULL, NULL));
      CHECK(rumqttc_event_redirect_diagnostics(
          event, &decision, &attempts, NULL, NULL, &visited, NULL, &candidate_present,
          &candidate_index, &candidate_count));
      CHECK(rumqttc_event_redirect_target(event, &target_present,
                                           &target_kind, &target, &port));
      REQUIRE(source == RUMQTTC_REDIRECT_SOURCE_CONNACK);
      REQUIRE(reason == RUMQTTC_REDIRECT_REASON_USE_ANOTHER_SERVER);
      REQUIRE(failure_present == 0 && failure == 0);
      REQUIRE(decision == RUMQTTC_REDIRECT_DECISION_FOLLOW);
      REQUIRE(attempts <= 1);
      if (target_present) {
        REQUIRE(attempts == 1);
        REQUIRE(visited == 2);
        REQUIRE(candidate_present == 1 && candidate_index == 1 && candidate_count == 3);
        REQUIRE(target_kind == RUMQTTC_REDIRECT_TARGET_TCP);
        REQUIRE(target.len == 9 && memcmp(target.data, "localhost", 9) == 0);
        REQUIRE(port == context->port);
        saw_target = 1;
      }
    } else if (kind == RUMQTTC_EVENT_CONNECTED) {
      REQUIRE(saw_target == 1);
      saw_connected = 1;
    }
    rumqttc_event_destroy(event);
  }
  worker = (native_thread_t *)atomic_exchange(&context->worker, 0);
  REQUIRE(worker != NULL && native_thread_join(worker) == 0);
  REQUIRE(atomic_load(&context->calls) == 1);
  native_close_destroy(client);
  rumqttc_config_destroy(config);
  rumqttc_resolver_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == 1);
  return 0;
}
