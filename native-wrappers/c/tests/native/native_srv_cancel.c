#include "native_common.h"

#include <stdatomic.h>
#include <stdlib.h>

typedef struct resolver_context {
  atomic_uintptr_t pending;
} resolver_context;

static atomic_uint destroyed;

static void resolve(void *user_data, const rumqttc_resolver_request_t *request,
                    rumqttc_callback_completion_t *completion) {
  resolver_context *context = (resolver_context *)user_data;
  rumqttc_callback_completion_t *retained = NULL;
  REQUIRE(request->owner.len > 0);
  CHECK(rumqttc_callback_completion_retain(completion, &retained));
  REQUIRE(atomic_exchange(&context->pending, (uintptr_t)retained) == 0);
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
  rumqttc_callback_completion_t *pending;
  rumqttc_srv_record_t record = RUMQTTC_SRV_RECORD_INIT;
  REQUIRE(context != NULL);
  vtable.resolve = resolve;
  vtable.destroy = resolver_destroy;
  CHECK(rumqttc_resolver_registration_new(&vtable, context, &registration,
                                           NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &config, NULL));
  CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(config,
                                     native_string("native-v5-srv-cancel"), NULL));
  CHECK(rumqttc_config_set_v5_redirect_policy(config, RUMQTTC_REDIRECT_FOLLOW,
                                               3, RUMQTTC_REDIRECT_TRANSPORT_TCP,
                                               NULL, NULL));
  CHECK(rumqttc_config_set_v5_srv_resolver(config, registration, NULL));
  CHECK(rumqttc_client_start(config, &client, NULL));
  for (unsigned attempt = 0; attempt < 500 &&
                             atomic_load(&context->pending) == 0; ++attempt)
    native_sleep_ms(10);
  pending = (rumqttc_callback_completion_t *)atomic_exchange(&context->pending, 0);
  REQUIRE(pending != NULL);
  native_close_destroy(client);
  record.port = native_test_port();
  record.target = native_string("localhost");
  REQUIRE(rumqttc_callback_srv_complete(pending, RUMQTTC_SRV_SUCCESS,
                                        &record, 1) == RUMQTTC_INVALID_STATE);
  rumqttc_config_destroy(config);
  rumqttc_resolver_registration_destroy(registration);
  REQUIRE(atomic_load(&destroyed) == 0);
  rumqttc_callback_completion_destroy(pending);
  REQUIRE(atomic_load(&destroyed) == 1);
  return 0;
}
