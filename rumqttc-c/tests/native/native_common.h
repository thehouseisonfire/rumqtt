#ifndef RUMQTTC_NATIVE_COMMON_H
#define RUMQTTC_NATIVE_COMMON_H

#include "rumqttc.h"

#include <stddef.h>
#include <stdint.h>

#define NATIVE_DEADLINE_MS 5000u

void native_fail(const char *file, int line, const char *expression,
                 rumqttc_status_t status);

#define CHECK(expression)                                                      \
  do {                                                                         \
    rumqttc_status_t native_status_ = (expression);                            \
    if (native_status_ != RUMQTTC_OK) {                                        \
      native_fail(__FILE__, __LINE__, #expression, native_status_);            \
    }                                                                          \
  } while (0)

#define REQUIRE(condition)                                                     \
  do {                                                                         \
    if (!(condition)) {                                                        \
      native_fail(__FILE__, __LINE__, #condition, UINT32_MAX);                 \
    }                                                                          \
  } while (0)

rumqttc_string_view_t native_string(const char *value);
rumqttc_bytes_view_t native_bytes(const uint8_t *data, size_t length);
rumqttc_publish_options_t native_publish_options(rumqttc_qos_t qos);
rumqttc_subscription_t native_subscription(const char *filter,
                                           rumqttc_qos_t qos);
uint16_t native_test_port(void);
rumqttc_client_t *
native_start_client(rumqttc_protocol_t protocol, const char *client_id,
                    rumqttc_ack_mode_t ack_mode, uint32_t request_capacity,
                    uint32_t event_capacity, uint64_t event_timeout_ms);
rumqttc_event_t *native_wait_event(rumqttc_client_t *client,
                                   rumqttc_event_kind_t expected);
void native_wait_completion(rumqttc_completion_t *completion,
                            rumqttc_completion_kind_t expected);
void native_close_destroy(rumqttc_client_t *client);
void native_sleep_ms(uint32_t milliseconds);
size_t native_process_thread_count(void);

typedef int (*native_thread_fn)(void *argument);
typedef struct native_thread native_thread_t;
native_thread_t *native_thread_start(native_thread_fn function, void *argument);
int native_thread_join(native_thread_t *thread);

#endif
