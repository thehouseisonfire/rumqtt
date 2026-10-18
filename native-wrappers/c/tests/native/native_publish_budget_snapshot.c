#include "native_common.h"
#include <stdatomic.h>

typedef struct host {
  atomic_uintptr_t load;
  atomic_uintptr_t connect;
  atomic_uint loads;
  atomic_uint cancelled;
  atomic_uint destroyed;
} host;

static void load_held(void *data, const rumqttc_store_request_t *request,
                      rumqttc_callback_completion_t *completion) {
  host *state = data;
  rumqttc_callback_completion_t *retained = NULL;
  REQUIRE(request->protocol == RUMQTTC_PROTOCOL_V5);
  CHECK(rumqttc_callback_completion_retain(completion, &retained));
  atomic_fetch_add(&state->loads, 1);
  REQUIRE(atomic_exchange(&state->load, (uintptr_t)retained) == 0);
}

static void unexpected_write(void *data, const rumqttc_store_request_t *request,
                             rumqttc_callback_completion_t *completion) {
  (void)data;
  (void)request;
  (void)completion;
  REQUIRE(0);
}

static void connect_held(void *data, const rumqttc_transport_connect_request_t *request,
                         rumqttc_callback_completion_t *completion) {
  host *state = data;
  rumqttc_callback_completion_t *retained = NULL;
  REQUIRE(request->protocol == RUMQTTC_PROTOCOL_V5);
  CHECK(rumqttc_callback_completion_retain(completion, &retained));
  REQUIRE(atomic_exchange(&state->connect, (uintptr_t)retained) == 0);
}

static void cancel_connect(void *data, uint64_t id) {
  REQUIRE(id != 0);
  atomic_fetch_add(&((host *)data)->cancelled, 1);
}

static void destroy_host(void *data) {
  atomic_fetch_add(&((host *)data)->destroyed, 1);
}

static rumqttc_callback_completion_t *wait_token(atomic_uintptr_t *slot) {
  uint64_t deadline = native_monotonic_ms() + NATIVE_DEADLINE_MS;
  while (atomic_load(slot) == 0) {
    REQUIRE(native_monotonic_ms() < deadline);
    native_sleep_ms(1);
  }
  return (rumqttc_callback_completion_t *)atomic_load(slot);
}

static void snapshot(rumqttc_client_t *client, uint8_t pending) {
  rumqttc_v5_publish_budget_snapshot_v2_t out = RUMQTTC_V5_PUBLISH_BUDGET_SNAPSHOT_V2_INIT;
  size_t count = 99, bytes = 99, max_count = 99, max_bytes = 99;
  out.recovery_pending = 99;
  out.reserved[0] = 99;
  CHECK(rumqttc_client_v5_publish_budget_snapshot_v2(client, &out, NULL));
  REQUIRE(out.struct_size == sizeof(out) && out.recovery_pending == pending);
  REQUIRE(out.reserved[0] == 0 && out.reserved[1] == 0 && out.reserved[2] == 0);
  REQUIRE(out.outstanding == 0 && out.retained_bytes == 0);
  REQUIRE(out.max_outstanding == 8 && out.max_bytes == 1024);
  /* Loading/connection barriers keep accounting stable across these reads. */
  CHECK(rumqttc_client_v5_publish_budget_snapshot(client, &count, &bytes, &max_count, &max_bytes, NULL));
  REQUIRE(count == out.outstanding && bytes == out.retained_bytes);
  REQUIRE(max_count == out.max_outstanding && max_bytes == out.max_bytes);
}

static void scenario(unsigned mode) {
  host state = {0};
  rumqttc_store_vtable_t store_table = RUMQTTC_STORE_VTABLE_INIT;
  rumqttc_transport_vtable_t transport_table = RUMQTTC_TRANSPORT_VTABLE_INIT;
  rumqttc_store_registration_t *store = NULL;
  rumqttc_transport_registration_t *transport = NULL;
  rumqttc_config_t *config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_callback_completion_t *load;
  store_table.load = load_held;
  store_table.save = unexpected_write;
  store_table.clear = unexpected_write;
  store_table.destroy = destroy_host;
  transport_table.connect = connect_held;
  transport_table.cancel = cancel_connect;
  transport_table.destroy = destroy_host;
  CHECK(rumqttc_store_registration_new(&store_table, &state, &store, NULL));
  CHECK(rumqttc_transport_registration_new(&transport_table, &state, &transport, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("memory"), 1883, NULL));
  CHECK(rumqttc_config_set_client_id(config, native_string("budget-snapshot"), NULL));
  CHECK(rumqttc_config_set_v5_session(config, 0, 1, 60, NULL));
  CHECK(rumqttc_config_set_v5_publish_budget(config, 8, 1024, NULL));
  CHECK(rumqttc_config_set_session_store(config, store, native_string("snapshot"), NATIVE_DEADLINE_MS, 1024, NULL));
  CHECK(rumqttc_config_set_transport_connector(config, transport, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  rumqttc_config_destroy(config);
  rumqttc_store_registration_destroy(store);
  rumqttc_transport_registration_destroy(transport);
  load = wait_token(&state.load);
  /* No publish probe or event consumer: reads cannot drive or cancel loading. */
  for (unsigned i = 0; i < 16; ++i)
    snapshot(client, 1);
  REQUIRE(atomic_load(&state.loads) == 1 && atomic_load(&state.connect) == 0);
  REQUIRE(atomic_load(&state.destroyed) == 0);
  if (mode != 2) {
    CHECK(rumqttc_callback_store_load_complete(load, mode == 0 ? RUMQTTC_STORE_NOT_FOUND : RUMQTTC_STORE_FAILED,
                                              native_bytes(NULL, 0)));
    if (mode == 0) {
      (void)wait_token(&state.connect);
      snapshot(client, 0);
      REQUIRE(atomic_load(&state.cancelled) == 0);
    } else {
      rumqttc_event_t *event = native_wait_event(client, RUMQTTC_EVENT_DRIVER_TERMINATED);
      rumqttc_event_destroy(event);
      snapshot(client, 1);
      REQUIRE(atomic_load(&state.connect) == 0);
    }
  }
  CHECK(rumqttc_client_close_now_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  snapshot(client, mode == 0 ? 0 : 1);
  if (mode == 2)
    REQUIRE(rumqttc_callback_store_load_complete(load, RUMQTTC_STORE_NOT_FOUND, native_bytes(NULL, 0)) ==
            RUMQTTC_INVALID_STATE);
  CHECK(rumqttc_client_destroy_timeout_ms(client, NATIVE_DEADLINE_MS, NULL));
  rumqttc_callback_completion_destroy(load);
  if (mode == 0) {
    REQUIRE(atomic_load(&state.cancelled) == 1);
    rumqttc_callback_completion_destroy((rumqttc_callback_completion_t *)atomic_load(&state.connect));
  }
  REQUIRE(atomic_load(&state.destroyed) == 2);
}

int main(void) {
  for (unsigned mode = 0; mode < 3; ++mode)
    scenario(mode);
  return 0;
}
