#include "native_common.h"

#include <string.h>

#define EXPECT_FAILURE(expression) REQUIRE((expression) != RUMQTTC_OK)

/*
 * Keep calls explicit: check_error_out_coverage.py derives the API list from
 * rumqttc.h and requires both markers whenever an optional error output is
 * added. Each marked function below is called with NULL on both paths.
 */
void native_test_error_out_contract(void) {
  rumqttc_config_t *v4 = NULL;
  rumqttc_config_t *v5 = NULL;
  rumqttc_config_t *start_config = NULL;
  rumqttc_client_t *client = NULL;
  rumqttc_client_t *failed_client = NULL;
  rumqttc_error_t *ignored_error = NULL;
  rumqttc_string_view_t valid = native_string("value");
  rumqttc_string_view_t invalid_string = {NULL, 1};
  rumqttc_bytes_view_t empty = {NULL, 0};
  rumqttc_bytes_view_t invalid_bytes = {NULL, 1};

  /* ERROR_OUT_SUCCESS: rumqttc_config_new */
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &v4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_new */
  EXPECT_FAILURE(rumqttc_config_new(99, &v5, NULL));
  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V5, &v5, NULL));

  /* ERROR_OUT_SUCCESS: rumqttc_config_set_broker */
  CHECK(rumqttc_config_set_broker(v4, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_broker */
  EXPECT_FAILURE(rumqttc_config_set_broker(NULL, valid, 1, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_client_id */
  CHECK(rumqttc_config_set_client_id(v4, valid, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_client_id */
  EXPECT_FAILURE(rumqttc_config_set_client_id(v4, invalid_string, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_username */
  CHECK(rumqttc_config_set_username(v4, valid, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_username */
  EXPECT_FAILURE(rumqttc_config_set_username(NULL, valid, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_clear_username */
  CHECK(rumqttc_config_clear_username(v4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_clear_username */
  EXPECT_FAILURE(rumqttc_config_clear_username(NULL, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_password */
  CHECK(rumqttc_config_set_password(v4, empty, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_password */
  EXPECT_FAILURE(rumqttc_config_set_password(v4, invalid_bytes, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_clear_password */
  CHECK(rumqttc_config_clear_password(v4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_clear_password */
  EXPECT_FAILURE(rumqttc_config_clear_password(NULL, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_tcp */
  CHECK(rumqttc_config_set_transport_tcp(v4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_tcp */
  EXPECT_FAILURE(rumqttc_config_set_transport_tcp(NULL, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_tls */
  CHECK(rumqttc_config_set_transport_tls(v4, empty, empty, empty, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_tls */
  EXPECT_FAILURE(
      rumqttc_config_set_transport_tls(NULL, empty, empty, empty, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_websocket */
  CHECK(rumqttc_config_set_transport_websocket(
      v4, native_string("ws://localhost"), NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_websocket */
  EXPECT_FAILURE(rumqttc_config_set_transport_websocket(NULL, valid, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_transport_wss */
  CHECK(rumqttc_config_set_transport_wss(v4, native_string("wss://localhost"),
                                         empty, empty, empty, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_transport_wss */
  EXPECT_FAILURE(
      rumqttc_config_set_transport_wss(NULL, valid, empty, empty, empty, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_keep_alive_seconds */
  CHECK(rumqttc_config_set_keep_alive_seconds(v4, 5, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_keep_alive_seconds */
  EXPECT_FAILURE(rumqttc_config_set_keep_alive_seconds(NULL, 5, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_connection_timeout_seconds */
  CHECK(rumqttc_config_set_connection_timeout_seconds(v4, 1, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_connection_timeout_seconds */
  EXPECT_FAILURE(rumqttc_config_set_connection_timeout_seconds(NULL, 1, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_request_capacity */
  CHECK(rumqttc_config_set_request_capacity(v4, 4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_request_capacity */
  EXPECT_FAILURE(rumqttc_config_set_request_capacity(NULL, 4, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_event_capacity */
  CHECK(rumqttc_config_set_event_capacity(v4, 4, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_event_capacity */
  EXPECT_FAILURE(rumqttc_config_set_event_capacity(NULL, 4, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_event_delivery_timeout_ms */
  CHECK(rumqttc_config_set_event_delivery_timeout_ms(v4, 100, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_event_delivery_timeout_ms */
  EXPECT_FAILURE(rumqttc_config_set_event_delivery_timeout_ms(NULL, 100, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_ack_mode */
  CHECK(rumqttc_config_set_ack_mode(v4, RUMQTTC_ACK_AUTOMATIC, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_ack_mode */
  EXPECT_FAILURE(rumqttc_config_set_ack_mode(v4, 99, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_incoming_packet_limit */
  CHECK(rumqttc_config_set_incoming_packet_limit(v4, 1024, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_incoming_packet_limit */
  EXPECT_FAILURE(rumqttc_config_set_incoming_packet_limit(NULL, 1024, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_emit_outgoing_events */
  CHECK(rumqttc_config_set_emit_outgoing_events(v4, 0, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_emit_outgoing_events */
  EXPECT_FAILURE(rumqttc_config_set_emit_outgoing_events(v4, 2, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_v4_clean_session */
  CHECK(rumqttc_config_set_v4_clean_session(v4, 1, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_v4_clean_session */
  EXPECT_FAILURE(rumqttc_config_set_v4_clean_session(NULL, 1, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_config_set_v5_session */
  CHECK(rumqttc_config_set_v5_session(v5, 1, 0, 0, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_config_set_v5_session */
  EXPECT_FAILURE(rumqttc_config_set_v5_session(NULL, 1, 0, 0, NULL));
  rumqttc_config_destroy(v4);
  rumqttc_config_destroy(v5);

  CHECK(rumqttc_config_new(RUMQTTC_PROTOCOL_V4, &start_config, NULL));
  CHECK(rumqttc_config_set_broker(start_config, native_string("127.0.0.1"),
                                  native_test_port(), NULL));
  CHECK(rumqttc_config_set_client_id(start_config, native_string("error-out"),
                                     NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_client_destroy_timeout_ms */
  CHECK(rumqttc_client_destroy_timeout_ms(NULL, 0, NULL));
  /* ERROR_OUT_SUCCESS: rumqttc_client_start */
  CHECK(rumqttc_client_start(start_config, &client, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_client_start */
  EXPECT_FAILURE(rumqttc_client_start(NULL, &failed_client, NULL));
  rumqttc_config_destroy(start_config);
  {
    rumqttc_event_t *connected =
        native_wait_event(client, RUMQTTC_EVENT_CONNECTED);
    rumqttc_event_t *event = NULL;
    rumqttc_event_kind_t kind = 0;
    rumqttc_publish_options_t publish = native_publish_options(RUMQTTC_QOS_1);
    rumqttc_publish_options_t invalid_publish = native_publish_options(99);
    rumqttc_subscription_t subscription =
        native_subscription("rumqttc/native/incoming", RUMQTTC_QOS_1);
    rumqttc_completion_t *publish_completion = NULL;
    rumqttc_completion_t *subscribe_completion = NULL;
    rumqttc_completion_t *diagnostics_completion = NULL;
    rumqttc_completion_t *failed_completion = NULL;
    uint64_t operation = 0;
    size_t count = 0;
    rumqttc_diagnostics_t diagnostics = RUMQTTC_DIAGNOSTICS_INIT;

    /* ERROR_OUT_SUCCESS: rumqttc_client_try_publish */
    CHECK(rumqttc_client_try_publish(client, native_string("error/out"), empty,
                                     &publish, &operation, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_try_publish */
    EXPECT_FAILURE(
        rumqttc_client_try_publish(client, native_string("error/out"), empty,
                                   &invalid_publish, &operation, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_client_publish_tracked */
    CHECK(rumqttc_client_publish_tracked(client, native_string("error/out"),
                                         empty, &publish, &publish_completion,
                                         NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_publish_tracked */
    EXPECT_FAILURE(rumqttc_client_publish_tracked(NULL, valid, empty, &publish,
                                                  &failed_completion, NULL));
    native_wait_completion(publish_completion,
                           RUMQTTC_COMPLETION_QOS1_ACKNOWLEDGED);
    /* ERROR_OUT_SUCCESS: rumqttc_completion_poll */
    CHECK(rumqttc_completion_poll(publish_completion, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_poll */
    EXPECT_FAILURE(rumqttc_completion_poll(NULL, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_completion_wait_timeout_ms */
    CHECK(rumqttc_completion_wait_timeout_ms(publish_completion, 0, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_wait_timeout_ms */
    EXPECT_FAILURE(rumqttc_completion_wait_timeout_ms(NULL, 0, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_completion_kind */
    CHECK(rumqttc_completion_kind(publish_completion, &kind, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_kind */
    EXPECT_FAILURE(rumqttc_completion_kind(NULL, &kind, NULL));
    rumqttc_completion_destroy(publish_completion);

    /* ERROR_OUT_SUCCESS: rumqttc_client_try_subscribe */
    CHECK(rumqttc_client_try_subscribe(client, &subscription, 1, NULL, &operation,
                                       NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_try_subscribe */
    EXPECT_FAILURE(
        rumqttc_client_try_subscribe(NULL, &subscription, 1, NULL, &operation, NULL));
    /* ERROR_OUT_SUCCESS: rumqttc_client_subscribe_tracked */
    CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, NULL,
                                           &subscribe_completion, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_subscribe_tracked */
    EXPECT_FAILURE(rumqttc_client_subscribe_tracked(NULL, &subscription, 1, NULL,
                                                    &failed_completion, NULL));
    native_wait_completion(subscribe_completion, RUMQTTC_COMPLETION_SUBSCRIBE);
    /* ERROR_OUT_SUCCESS: rumqttc_completion_result_count */
    CHECK(rumqttc_completion_result_count(subscribe_completion, &count, NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_result_count */
    EXPECT_FAILURE(rumqttc_completion_result_count(NULL, &count, NULL));
    {
      uint8_t success = 0, reason_present = 0, reason = 0;
      rumqttc_qos_t qos = 0;
      /* ERROR_OUT_SUCCESS: rumqttc_completion_result_at */
      CHECK(rumqttc_completion_result_at(subscribe_completion, 0, &success,
                                         &qos, &reason_present, &reason, NULL));
      success = 0;
      CHECK(rumqttc_completion_result_at(subscribe_completion, 0, &success,
                                         NULL, NULL, NULL, NULL));
      REQUIRE(success == 1);
      CHECK(rumqttc_completion_result_at(subscribe_completion, 0, NULL, &qos,
                                         NULL, NULL, NULL));
      REQUIRE(qos == RUMQTTC_QOS_1);
      EXPECT_FAILURE(rumqttc_completion_result_at(subscribe_completion, 0, NULL,
                                                  NULL, NULL, NULL, NULL));
      /* ERROR_OUT_FAILURE: rumqttc_completion_result_at */
      EXPECT_FAILURE(rumqttc_completion_result_at(
          NULL, 0, &success, &qos, &reason_present, &reason, NULL));
    }
    rumqttc_completion_destroy(subscribe_completion);
    {
      rumqttc_string_view_t filter = subscription.filter;
      /* ERROR_OUT_SUCCESS: rumqttc_client_try_unsubscribe */
      CHECK(
          rumqttc_client_try_unsubscribe(client, &filter, 1, NULL, &operation, NULL));
      /* ERROR_OUT_FAILURE: rumqttc_client_try_unsubscribe */
      EXPECT_FAILURE(
          rumqttc_client_try_unsubscribe(NULL, &filter, 1, NULL, &operation, NULL));
      /* ERROR_OUT_SUCCESS: rumqttc_client_unsubscribe_tracked */
      CHECK(rumqttc_client_unsubscribe_tracked(client, &filter, 1, NULL,
                                               &subscribe_completion, NULL));
      /* ERROR_OUT_FAILURE: rumqttc_client_unsubscribe_tracked */
      EXPECT_FAILURE(rumqttc_client_unsubscribe_tracked(
          NULL, &filter, 1, NULL, &failed_completion, NULL));
      native_wait_completion(subscribe_completion,
                             RUMQTTC_COMPLETION_UNSUBSCRIBE);
      rumqttc_completion_destroy(subscribe_completion);
    }
    /* ERROR_OUT_SUCCESS: rumqttc_client_diagnostics_tracked */
    CHECK(rumqttc_client_diagnostics_tracked(client, &diagnostics_completion,
                                             NULL));
    /* ERROR_OUT_FAILURE: rumqttc_client_diagnostics_tracked */
    EXPECT_FAILURE(
        rumqttc_client_diagnostics_tracked(NULL, &failed_completion, NULL));
    native_wait_completion(diagnostics_completion,
                           RUMQTTC_COMPLETION_DIAGNOSTICS);
    /* ERROR_OUT_SUCCESS: rumqttc_completion_diagnostics */
    CHECK(rumqttc_completion_diagnostics(diagnostics_completion, &diagnostics,
                                         NULL));
    /* ERROR_OUT_FAILURE: rumqttc_completion_diagnostics */
    EXPECT_FAILURE(rumqttc_completion_diagnostics(NULL, &diagnostics, NULL));
    rumqttc_completion_destroy(diagnostics_completion);

    /* ERROR_OUT_SUCCESS: rumqttc_client_event_try_recv */
    CHECK(rumqttc_client_event_try_recv(client, &event, NULL));
    rumqttc_event_destroy(event);
    /* ERROR_OUT_FAILURE: rumqttc_client_event_try_recv */
    EXPECT_FAILURE(rumqttc_client_event_try_recv(NULL, &event, NULL));
    subscribe_completion = NULL;
    CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, NULL,
                                           &subscribe_completion, NULL));
    native_wait_completion(subscribe_completion, RUMQTTC_COMPLETION_SUBSCRIBE);
    rumqttc_completion_destroy(subscribe_completion);
    /* ERROR_OUT_SUCCESS: rumqttc_client_event_recv_timeout_ms */
    CHECK(rumqttc_client_event_recv_timeout_ms(client, 5000, &event, NULL));
    rumqttc_event_destroy(event);
    /* ERROR_OUT_FAILURE: rumqttc_client_event_recv_timeout_ms */
    EXPECT_FAILURE(rumqttc_client_event_recv_timeout_ms(NULL, 0, &event, NULL));
    EXPECT_FAILURE(
        rumqttc_event_disconnected(connected, &kind, &ignored_error));
    rumqttc_error_destroy(ignored_error);
    ignored_error = NULL;
    rumqttc_event_destroy(connected);
  }
  /* Let automatic protocol acknowledgements queued by the event loop reach the
   * fixture. */
  native_sleep_ms(100);
  /* ERROR_OUT_SUCCESS: rumqttc_client_close_now_timeout_ms */
  CHECK(rumqttc_client_close_now_timeout_ms(client, 5000, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_client_close_now_timeout_ms */
  EXPECT_FAILURE(rumqttc_client_close_now_timeout_ms(NULL, 5000, NULL));
  CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));

  client = native_start_client(RUMQTTC_PROTOCOL_V4, "error-disconnect",
                               RUMQTTC_ACK_AUTOMATIC, 8, 8, 1000);
  {
    rumqttc_publish_options_t interrupt = native_publish_options(RUMQTTC_QOS_0);
    rumqttc_event_t *disconnected;
    rumqttc_error_t *disconnect_error = NULL;
    uint64_t operation = 0;
    uint32_t phase = 0;
    CHECK(rumqttc_client_try_publish(client,
                                     native_string("rumqttc/native/interrupt"),
                                     empty, &interrupt, &operation, NULL));
    disconnected = native_wait_event(client, RUMQTTC_EVENT_DISCONNECTED);
    CHECK(rumqttc_event_disconnected(disconnected, &phase, &disconnect_error));
    rumqttc_error_destroy(disconnect_error);
    rumqttc_event_destroy(disconnected);
  }
  native_close_destroy(client);

  {
    unsigned tracked;
    for (tracked = 0; tracked < 2; ++tracked) {
      rumqttc_subscription_t subscription =
          native_subscription("rumqttc/native/incoming", RUMQTTC_QOS_1);
      rumqttc_completion_t *completion = NULL;
      rumqttc_event_t *incoming;
      uint64_t operation = 0;
      client =
          native_start_client(RUMQTTC_PROTOCOL_V4,
                              tracked ? "error-ack-tracked" : "error-ack-try",
                              RUMQTTC_ACK_MANUAL, 8, 8, 1000);
      CHECK(rumqttc_client_subscribe_tracked(client, &subscription, 1, NULL,
                                             &completion, NULL));
      native_wait_completion(completion, RUMQTTC_COMPLETION_SUBSCRIBE);
      rumqttc_completion_destroy(completion);
      incoming = native_wait_event(client, RUMQTTC_EVENT_INCOMING_PUBLISH);
      if (tracked == 0) {
        /* ERROR_OUT_SUCCESS: rumqttc_client_try_acknowledge */
        CHECK(
            rumqttc_client_try_acknowledge(client, incoming, &operation, NULL));
        /* ERROR_OUT_FAILURE: rumqttc_client_try_acknowledge */
        EXPECT_FAILURE(
            rumqttc_client_try_acknowledge(client, incoming, &operation, NULL));
      } else {
        completion = NULL;
        /* ERROR_OUT_SUCCESS: rumqttc_client_acknowledge_tracked */
        CHECK(rumqttc_client_acknowledge_tracked(client, incoming, &completion,
                                                 NULL));
        native_wait_completion(completion, RUMQTTC_COMPLETION_ACKNOWLEDGED);
        rumqttc_completion_destroy(completion);
        completion = NULL;
        /* ERROR_OUT_FAILURE: rumqttc_client_acknowledge_tracked */
        EXPECT_FAILURE(rumqttc_client_acknowledge_tracked(client, incoming,
                                                          &completion, NULL));
      }
      rumqttc_event_destroy(incoming);
      if (tracked == 0) {
        /* The untracked API reports admission, so allow the admitted ACK to
         * flush. */
        native_sleep_ms(100);
      }
      native_close_destroy(client);
    }
  }

  client = native_start_client(RUMQTTC_PROTOCOL_V4, "error-close",
                               RUMQTTC_ACK_AUTOMATIC, 8, 8, 1000);
  /* ERROR_OUT_SUCCESS: rumqttc_client_close_timeout_ms */
  CHECK(rumqttc_client_close_timeout_ms(client, 5000, NULL));
  /* ERROR_OUT_FAILURE: rumqttc_client_close_timeout_ms */
  EXPECT_FAILURE(rumqttc_client_close_timeout_ms(NULL, 0, NULL));
  CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));

  client = native_start_client(RUMQTTC_PROTOCOL_V4, "error-destroy",
                               RUMQTTC_ACK_AUTOMATIC, 8, 8, 1000);
  /* ERROR_OUT_FAILURE: rumqttc_client_destroy_timeout_ms */
  EXPECT_FAILURE(rumqttc_client_destroy_timeout_ms(client, 0, NULL));
  /* A failed destroy retained ownership, so the same handle can be retried. */
  CHECK(rumqttc_client_destroy_timeout_ms(client, 5000, NULL));

  client = native_start_client(RUMQTTC_PROTOCOL_V4, "error-abandon",
                               RUMQTTC_ACK_AUTOMATIC, 8, 8, 1000);
  rumqttc_client_abandon(client);
  rumqttc_client_abandon(NULL);

  (void)ignored_error;
}
