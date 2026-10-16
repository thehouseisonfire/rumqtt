//! Reuse the controlled transport to check the public C admission lifecycle.
use super::*;
use std::time::Instant;

fn pending_publish(client: &Client, qos: u32) -> *mut rumqttc_pending_admission {
    let mut out = ptr::null_mut();
    let options = rumqttc_publish_options_t {
        struct_size: struct_size::<rumqttc_publish_options_t>(),
        qos,
        retain: 0,
        reserved: [0; 3],
        protocol_options: 0,
        v5_properties: ptr::null(),
    };
    assert_eq!(
        unsafe {
            rumqttc_client_publish_deferred(
                client.raw,
                view_string("a"),
                view_bytes(b"data"),
                &raw const options,
                &raw mut out,
                ptr::null_mut(),
            )
        },
        0
    );
    out
}

fn pending_result(
    pending: *const rumqttc_pending_admission,
) -> (u32, u32, u64, *mut rumqttc_completion) {
    let mut state = 99;
    let mut id = 99;
    let mut completion = ptr::null_mut();
    let status = unsafe {
        rumqttc_pending_admission_wait_timeout_ms(
            pending,
            3000,
            &raw mut state,
            &raw mut id,
            &raw mut completion,
            ptr::null_mut(),
        )
    };
    (status, state, id, completion)
}

fn assert_empty(client: &Client) {
    let until = Instant::now() + DEADLINE;
    loop {
        let snapshot = unsafe { &*client.raw }
            .inner
            .handle
            .pending_admission_snapshot();
        if snapshot.pending == 0 {
            assert_eq!(snapshot.retained_bytes, 0);
            break;
        }
        assert!(Instant::now() < until, "pending storage was not released");
        thread::yield_now();
    }
}

#[test]
fn deferred_pending_capabilities_limits_timeouts_cancellation_and_last_owner() {
    for shared in [false, true] {
        let mut client = Client::configured(2, shared, false, false, |c| {
            c.common.pending_admission_limits = wc::PendingAdmissionLimits {
                max_pending: 1,
                max_bytes: 5,
            };
        });
        assert_eq!(client.peer.frame().0, 0x10);
        let pending = pending_publish(&client, 1);
        unsafe {
            let mut state = 99;
            let mut id = 99;
            let mut completion = std::ptr::dangling_mut();
            assert_eq!(
                rumqttc_pending_admission_wait_timeout_ms(
                    pending,
                    1,
                    &raw mut state,
                    &raw mut id,
                    &raw mut completion,
                    ptr::null_mut()
                ),
                5
            );
            assert_eq!((state, id, completion), (0, 0, ptr::null_mut()));
            let mut rejected = std::ptr::dangling_mut();
            assert_eq!(
                rumqttc_client_publish_deferred(
                    client.raw,
                    view_string("a"),
                    view_bytes(b"data"),
                    ptr::null(),
                    &raw mut rejected,
                    ptr::null_mut()
                ),
                4
            );
            assert!(rejected.is_null());
            assert_eq!(
                rumqttc_client_publish_deferred(
                    client.raw,
                    view_string("a"),
                    view_bytes(b"large"),
                    ptr::null(),
                    &raw mut rejected,
                    ptr::null_mut()
                ),
                1
            );
            assert!(rejected.is_null());
            let mut retained = ptr::null_mut();
            assert_eq!(
                rumqttc_pending_admission_retain(pending, &raw mut retained, ptr::null_mut()),
                0
            );
            rumqttc_pending_admission_release(pending);
            assert_eq!(
                rumqttc_pending_admission_poll(
                    retained,
                    &raw mut state,
                    &raw mut id,
                    ptr::null_mut(),
                    ptr::null_mut()
                ),
                11
            );
            assert_eq!(state, 0);
            assert_eq!(
                rumqttc_pending_admission_cancel(retained, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_pending_admission_cancel(retained, ptr::null_mut()),
                0
            );
            assert_eq!(pending_result(retained), (0, 3, 0, ptr::null_mut()));
            assert_eq!(pending_result(retained), (0, 3, 0, ptr::null_mut()));
            rumqttc_pending_admission_release(retained);
            assert_empty(&client);
            let abandoned = pending_publish(&client, 1);
            rumqttc_pending_admission_release(abandoned);
            assert_empty(&client);
            let failed = pending_publish(&client, 1);
            client.destroy();
            let result = pending_result(failed);
            assert_ne!(result.0, 0);
            assert_eq!((result.1, result.2, result.3), (4, 0, ptr::null_mut()));
            rumqttc_pending_admission_release(failed);
        }
    }
}

#[test]
fn deferred_full_queue_admission_results_are_repeatable_and_completions_independent() {
    for shared in [false, true] {
        for protocol in [1, 2] {
            let mut client = Client::configured(protocol, shared, false, false, |_| {});
            assert_eq!(client.peer.frame().0, 0x10);
            let mut first = ptr::null_mut();
            unsafe {
                assert_eq!(
                    rumqttc_client_publish_tracked(
                        client.raw,
                        view_string("a"),
                        view_bytes(b"first"),
                        ptr::null(),
                        &raw mut first,
                        ptr::null_mut()
                    ),
                    0
                );
                let pending = pending_publish(&client, 0);
                let mut state = 99;
                assert_eq!(
                    rumqttc_pending_admission_wait_timeout_ms(
                        pending,
                        1,
                        &raw mut state,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut()
                    ),
                    5
                );
                assert_eq!(state, 0);
                client.peer.send(if protocol == 1 {
                    vec![0x20, 2, 0, 0]
                } else {
                    vec![0x20, 3, 0, 0, 0]
                });
                rumqttc_event_destroy(client.event());
                let (status, state, id, completion) = pending_result(pending);
                assert_eq!((status, state), (0, 2));
                assert_ne!(id, 0);
                assert!(!completion.is_null());
                assert_eq!(
                    rumqttc_pending_admission_cancel(pending, ptr::null_mut()),
                    0
                );
                let again = pending_result(pending);
                assert_eq!((again.0, again.1, again.2), (0, 2, id));
                assert_ne!(again.3, completion);
                rumqttc_completion_destroy(completion);
                assert_eq!(client.peer.frame().0 >> 4, 3);
                assert_eq!(client.peer.frame().0 >> 4, 3);
                assert_eq!(wait(first), 0);
                assert_eq!(wait(again.3), 0);
                rumqttc_completion_destroy(first);
                client.destroy();
                let after_destroy = pending_result(pending);
                assert_eq!(
                    (after_destroy.0, after_destroy.1, after_destroy.2),
                    (0, 2, id)
                );
                assert_eq!(wait(after_destroy.3), 0);
                rumqttc_completion_destroy(after_destroy.3);
                rumqttc_pending_admission_release(pending);
                assert_eq!(wait(again.3), 0);
                rumqttc_completion_destroy(again.3);
            }
        }
    }
}

#[test]
fn deferred_ack_cancellation_restores_authority_and_admission_consumes_aliases() {
    for shared in [false, true] {
        for protocol in [1, 2] {
            let mut client = Client::new(protocol, shared, true);
            let first_event = client.publish(1, false, 7);
            let first = take(first_event);
            let second_event = client.publish(1, false, 8);
            let second = take(second_event);
            unsafe {
                rumqttc_event_destroy(first_event);
                rumqttc_event_destroy(second_event);
                client.peer.flush.hold.store(true, Ordering::Release);
                let mut first_completion = ptr::null_mut();
                assert_eq!(
                    rumqttc_client_acknowledge_token_tracked(
                        client.raw,
                        first,
                        &raw mut first_completion,
                        ptr::null_mut()
                    ),
                    0
                );
                client.peer.entered.recv_timeout(DEADLINE).unwrap();
                (*client.raw)
                    .inner
                    .handle
                    .try_admit(wc::Command::Unsubscribe(wc::UnsubscribeCommand {
                        filters: vec!["a".into()],
                        protocol: wc::UnsubscribeProtocolOptions::VersionNeutral,
                    }))
                    .unwrap();
                let mut pending = ptr::null_mut();
                assert_eq!(
                    rumqttc_client_acknowledge_token_deferred(
                        client.raw,
                        second,
                        ptr::null(),
                        &raw mut pending,
                        ptr::null_mut()
                    ),
                    0
                );
                let mut id = 99;
                assert_ne!(
                    rumqttc_client_try_acknowledge_token(
                        client.raw,
                        second,
                        &raw mut id,
                        ptr::null_mut()
                    ),
                    0
                );
                assert_eq!(id, 0);
                assert_eq!(
                    rumqttc_pending_admission_cancel(pending, ptr::null_mut()),
                    0
                );
                assert_eq!(pending_result(pending), (0, 3, 0, ptr::null_mut()));
                rumqttc_pending_admission_release(pending);
                assert_eq!(
                    rumqttc_client_acknowledge_token_deferred(
                        client.raw,
                        second,
                        ptr::null(),
                        &raw mut pending,
                        ptr::null_mut()
                    ),
                    0
                );
                client.peer.flush.hold.store(false, Ordering::Release);
                client.peer.flush.release.add_permits(1);
                let (status, state, mut id, completion) = pending_result(pending);
                assert_eq!((status, state), (0, 2));
                assert_ne!(id, 0);
                assert_eq!(wait(completion), 0);
                assert_eq!(client.peer.frame(), (0x40, vec![0, 7]));
                assert_eq!(client.peer.frame().0, 0xa2);
                assert_eq!(client.peer.frame(), (0x40, vec![0, 8]));
                assert_ne!(
                    rumqttc_client_try_acknowledge_token(
                        client.raw,
                        second,
                        &raw mut id,
                        ptr::null_mut()
                    ),
                    0
                );
                rumqttc_pending_admission_release(pending);
                rumqttc_completion_destroy(completion);
                rumqttc_completion_destroy(first_completion);
                rumqttc_ack_token_release(first);
                rumqttc_ack_token_release(second);
                assert_empty(&client);
                client.destroy();
            }
        }
    }
}

#[test]
fn deferred_invalid_inputs_preserve_outputs_and_capacity() {
    let client = Client::new(2, true, false);
    unsafe {
        let mut pending = std::ptr::dangling_mut();
        assert_eq!(
            rumqttc_client_publish_deferred(
                client.raw,
                view_string("bad/#"),
                view_bytes(b"x"),
                ptr::null(),
                &raw mut pending,
                ptr::null_mut()
            ),
            1
        );
        assert!(pending.is_null());
        let bad = rumqttc_string_view_t {
            data: ptr::null(),
            len: 1,
        };
        assert_eq!(
            rumqttc_client_unsubscribe_deferred(
                client.raw,
                &raw const bad,
                1,
                ptr::null(),
                &raw mut pending,
                ptr::null_mut()
            ),
            1
        );
        assert!(pending.is_null());
        assert_empty(&client);
        let mut state = 99;
        let mut id = 99;
        let mut completion = std::ptr::dangling_mut();
        assert_eq!(
            rumqttc_pending_admission_poll(
                ptr::null(),
                &raw mut state,
                &raw mut id,
                &raw mut completion,
                ptr::null_mut()
            ),
            1
        );
        assert_eq!((state, id, completion), (0, 0, ptr::null_mut()));
        let pending = pending_publish(&client, 0);
        rumqttc_pending_admission_release(pending);
    }
}

#[test]
fn deferred_publish_budget_wait_does_not_block_incoming_ack_admission() {
    for shared in [false, true] {
        let mut client = Client::configured(2, shared, true, true, |config| {
            if let wc::ProtocolConfig::V5(v5) = &mut config.protocol {
                v5.publish_budget.max_outstanding = 1;
            }
        });
        unsafe {
            (*client.raw)
                .inner
                .handle
                .try_admit(wc::Command::Publish(wc::PublishCommand {
                    topic: "a".into(),
                    payload: Bytes::new(),
                    qos: wc::QoS::AtLeastOnce,
                    retain: false,
                    protocol: wc::PublishProtocolOptions::VersionNeutral,
                }))
                .unwrap();
            assert_eq!(client.peer.frame().0, 0x32);
            let waiting = pending_publish(&client, 1);
            let event = client.publish(1, false, 11);
            let token = take(event);
            rumqttc_event_destroy(event);
            let mut ack = ptr::null_mut();
            assert_eq!(
                rumqttc_client_acknowledge_token_deferred(
                    client.raw,
                    token,
                    ptr::null(),
                    &raw mut ack,
                    ptr::null_mut()
                ),
                0
            );
            let result = pending_result(ack);
            assert_eq!((result.0, result.1), (0, 2));
            assert_eq!(wait(result.3), 0);
            assert_eq!(client.peer.frame(), (0x40, vec![0, 11]));
            let mut state = 99;
            assert_eq!(
                rumqttc_pending_admission_poll(
                    waiting,
                    &raw mut state,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut()
                ),
                11
            );
            assert_eq!(state, 0);
            rumqttc_pending_admission_cancel(waiting, ptr::null_mut());
            assert_eq!(pending_result(waiting).1, 3);
            rumqttc_pending_admission_release(waiting);
            rumqttc_pending_admission_release(ack);
            rumqttc_completion_destroy(result.3);
            rumqttc_ack_token_release(token);
        }
    }
}

#[test]
fn deferred_event_backpressure_terminates_waiters_and_releases_pending_storage() {
    for shared in [false, true] {
        let mut client = Client::configured(2, shared, false, true, |config| {
            config.common.event_delivery_timeout = Duration::from_millis(20);
            if let wc::ProtocolConfig::V5(v5) = &mut config.protocol {
                v5.publish_budget.max_outstanding = 1;
            }
        });
        unsafe {
            (*client.raw)
                .inner
                .handle
                .try_admit(wc::Command::Publish(wc::PublishCommand {
                    topic: "a".into(),
                    payload: Bytes::new(),
                    qos: wc::QoS::AtLeastOnce,
                    retain: false,
                    protocol: wc::PublishProtocolOptions::VersionNeutral,
                }))
                .unwrap();
            assert_eq!(client.peer.frame().0, 0x32);
            let waiting = pending_publish(&client, 1);
            client.peer.publish(2, 0, false, 0);
            client.peer.publish(2, 0, false, 0);
            let result = pending_result(waiting);
            assert_ne!(result.0, 0);
            assert_eq!((result.1, result.2, result.3), (4, 0, ptr::null_mut()));
            assert_empty(&client);
            client.destroy();
            assert_eq!(pending_result(waiting), result);
            rumqttc_pending_admission_release(waiting);
        }
    }
}

#[test]
fn deferred_creation_copies_inputs_before_callers_replace_them() {
    let mut client = Client::configured(2, true, false, false, |_| {});
    assert_eq!(client.peer.frame().0, 0x10);
    let mut topic = String::from("original");
    let mut payload = Vec::from(b"contents".as_slice());
    let mut options = rumqttc_publish_options_t {
        struct_size: struct_size::<rumqttc_publish_options_t>(),
        qos: 1,
        retain: 0,
        reserved: [0; 3],
        protocol_options: 0,
        v5_properties: ptr::null(),
    };
    let mut pending = ptr::null_mut();
    unsafe {
        assert_eq!(
            rumqttc_client_publish_deferred(
                client.raw,
                view_string(&topic),
                view_bytes(&payload),
                &raw const options,
                &raw mut pending,
                ptr::null_mut()
            ),
            0
        );
        topic.clear();
        payload.fill(b'x');
        options.qos = 0;
        client.peer.send(vec![0x20, 3, 0, 0, 0]);
        rumqttc_event_destroy(client.event());
        let result = pending_result(pending);
        assert_eq!((result.0, result.1), (0, 2));
        let (header, body) = client.peer.frame();
        assert_eq!(header, 0x32);
        assert_eq!(&body[2..10], b"original");
        assert_eq!(&body[13..], b"contents");
        client.peer.send(vec![0x40, 2, body[10], body[11]]);
        assert_eq!(wait(result.3), 0);
        rumqttc_completion_destroy(result.3);
        rumqttc_pending_admission_release(pending);
    }
    assert_eq!(options.qos, 0);
}

#[test]
fn deferred_waiters_preserve_keepalive_progress() {
    for shared in [false, true] {
        let mut client = Client::configured(2, shared, false, true, |config| {
            config.common.keep_alive = Duration::from_secs(1);
            if let wc::ProtocolConfig::V5(v5) = &mut config.protocol {
                v5.publish_budget.max_outstanding = 1;
            }
        });
        unsafe {
            (*client.raw)
                .inner
                .handle
                .try_admit(wc::Command::Publish(wc::PublishCommand {
                    topic: "a".into(),
                    payload: Bytes::new(),
                    qos: wc::QoS::AtLeastOnce,
                    retain: false,
                    protocol: wc::PublishProtocolOptions::VersionNeutral,
                }))
                .unwrap();
            assert_eq!(client.peer.frame().0, 0x32);
            let waiting: Vec<_> = (0..8).map(|_| pending_publish(&client, 1)).collect();
            assert_eq!(client.peer.frame(), (0xc0, Vec::new()));
            client.peer.send(vec![0xd0, 0]);
            for pending in waiting {
                rumqttc_pending_admission_cancel(pending, ptr::null_mut());
                assert_eq!(pending_result(pending).1, 3);
                rumqttc_pending_admission_release(pending);
            }
            assert_empty(&client);
        }
    }
}

#[test]
fn deferred_empty_properties_are_charged_for_collection_metadata() {
    let client = Client::configured(2, false, false, false, |config| {
        config.common.pending_admission_limits.max_bytes = 52;
    });
    let property = rumqttc_user_property_t {
        struct_size: struct_size::<rumqttc_user_property_t>(),
        name: view_string(""),
        value: view_string(""),
    };
    let properties = [
        property,
        rumqttc_user_property_t {
            struct_size: struct_size::<rumqttc_user_property_t>(),
            name: view_string(""),
            value: view_string(""),
        },
    ];
    unsafe {
        let mut content: rumqttc_v5_publish_properties_t = std::mem::zeroed();
        content.struct_size = struct_size::<rumqttc_v5_publish_properties_t>();
        content.user_properties = properties.as_ptr();
        content.user_property_count = 2;
        let options = rumqttc_publish_options_t {
            struct_size: struct_size::<rumqttc_publish_options_t>(),
            qos: 1,
            retain: 0,
            reserved: [0; 3],
            protocol_options: 5,
            v5_properties: &raw const content,
        };
        let mut pending = std::ptr::dangling_mut();
        assert_eq!(
            rumqttc_client_publish_deferred(
                client.raw,
                view_string("a"),
                view_bytes(b""),
                &raw const options,
                &raw mut pending,
                ptr::null_mut()
            ),
            1
        );
        assert!(pending.is_null());
        assert_empty(&client);
    }
}
