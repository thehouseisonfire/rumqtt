#ifndef RUMQTTC_EXAMPLE_COMMON_H
#define RUMQTTC_EXAMPLE_COMMON_H

#include "rumqttc.h"

#include <stddef.h>
#include <stdint.h>

rumqttc_string_view_t example_string(const char *value);
rumqttc_bytes_view_t example_bytes(const void *data, size_t length);
rumqttc_publish_options_t example_publish_options(rumqttc_qos_t qos);
int example_report(rumqttc_status_t status, rumqttc_error_t **error,
                   const char *operation);
rumqttc_client_t *example_connect(const char *host, uint16_t port,
                                  const char *client_id,
                                  rumqttc_ack_mode_t ack_mode);
rumqttc_event_t *example_next_event(rumqttc_client_t *client,
                                    rumqttc_event_kind_t wanted);
int example_wait(rumqttc_completion_t *completion,
                 rumqttc_completion_kind_t wanted);
void example_destroy_client(rumqttc_client_t **client);

typedef int (*example_thread_fn)(void *argument);
typedef struct example_thread example_thread_t;
example_thread_t *example_thread_start(example_thread_fn function,
                                       void *argument);
int example_thread_join(example_thread_t *thread, uint32_t timeout_ms,
                        int *result_out);

#endif
