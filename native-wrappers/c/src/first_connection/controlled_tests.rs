//! Use the controlled transport to exercise C ownership without consuming startup events.
use super::*;

fn acquire(client: &Client) -> *mut rumqttc_first_connection {
    let mut observer = ptr::null_mut();
    assert_eq!(
        unsafe { rumqttc_client_first_connection(client.raw, &raw mut observer, ptr::null_mut()) },
        0
    );
    observer
}

fn observe(observer: *const rumqttc_first_connection, timeout: u64) -> (u32, u32, u32, u8) {
    let (mut state, mut protocol, mut session) = (99, 99, 99);
    let status = unsafe {
        rumqttc_first_connection_wait_timeout_ms(
            observer,
            timeout,
            &raw mut state,
            &raw mut protocol,
            &raw mut session,
            ptr::null_mut(),
        )
    };
    (status, state, protocol, session)
}

#[test]
fn retained_observers_share_success_without_consuming_events_or_retaining_the_client() {
    for protocol in [1, 2] {
        for shared in [false, true] {
            let mut client = Client::configured(protocol, shared, false, false, |_| {});
            assert_eq!(client.peer.frame().0, 0x10);
            let observer = acquire(&client);
            assert_eq!(observe(observer, 1), (5, 1, 0, 0));
            let mut waiters = Vec::new();
            for _ in 0..4 {
                let mut retained = ptr::null_mut();
                assert_eq!(
                    unsafe {
                        rumqttc_first_connection_retain(
                            observer,
                            &raw mut retained,
                            ptr::null_mut(),
                        )
                    },
                    0
                );
                let retained = retained as usize;
                waiters.push(thread::spawn(move || {
                    let retained = retained as *mut rumqttc_first_connection;
                    let result = observe(retained, 3000);
                    unsafe { rumqttc_first_connection_release(retained) };
                    result
                }));
            }
            unsafe { rumqttc_first_connection_release(observer) };
            // Polling/waiting must not acquire the event-consumer lock.
            let events = unsafe { &*client.raw }.inner.events.lock().unwrap();
            client.peer.send(if protocol == 1 {
                vec![0x20, 2, 0, 0]
            } else {
                vec![0x20, 3, 0, 0, 0]
            });
            for waiter in waiters {
                assert_eq!(waiter.join().unwrap(), (0, 2, protocol, 0));
            }
            drop(events);
            let observer = acquire(&client);
            let event = client.event();
            assert!(matches!(
                unsafe { &*event }.inner.event,
                wc::WrapperEvent::Connected { .. }
            ));
            unsafe { rumqttc_event_destroy(event) };
            client.reconnect();
            assert_eq!(observe(observer, 0), (0, 2, protocol, 0));
            assert_eq!(
                unsafe { rumqttc_client_close_now_timeout_ms(client.raw, 3000, ptr::null_mut()) },
                0
            );
            let late = acquire(&client);
            client.destroy(); // Also proves the transport callback owner was released.
            assert_eq!(observe(observer, 0), (0, 2, protocol, 0));
            assert_eq!(observe(late, 0), (0, 2, protocol, 0));
            unsafe {
                rumqttc_first_connection_release(observer);
                rumqttc_first_connection_release(late);
            }
        }
    }
}

#[test]
fn releasing_the_last_external_observer_does_not_cancel_startup() {
    for protocol in [1, 2] {
        let mut client = Client::configured(protocol, false, false, false, |_| {});
        let observer = acquire(&client);
        unsafe { rumqttc_first_connection_release(observer) };
        client.handshake();
        let observer = acquire(&client);
        client.destroy();
        assert_eq!(observe(observer, 0), (0, 2, protocol, 0));
        unsafe { rumqttc_first_connection_release(observer) };
    }
}

#[test]
fn acquisition_while_closing_or_failed_retains_the_existing_terminal_error() {
    for protocol in [1, 2] {
        for poison in [false, true] {
            let mut client = Client::configured(protocol, true, false, false, |_| {});
            assert_eq!(client.peer.frame().0, 0x10);
            let first = acquire(&client);
            if poison {
                unsafe { &*client.raw }.inner.poison();
            } else {
                unsafe { &*client.raw }
                    .inner
                    .handle
                    .try_admit(wc::Command::GracefulDisconnect {
                        timeout: Some(DEADLINE),
                    })
                    .unwrap();
                assert_eq!(
                    unsafe { &*client.raw }.inner.handle.state(),
                    wc::LifecycleState::Closing
                );
            }
            let closing = acquire(&client);
            assert_eq!(
                unsafe { rumqttc_client_close_now_timeout_ms(client.raw, 3000, ptr::null_mut()) },
                0
            );
            let late = acquire(&client);
            client.destroy();
            for observer in [first, closing, late] {
                assert_eq!(observe(observer, 0), (6, 3, 0, 0));
                unsafe { rumqttc_first_connection_release(observer) };
            }
        }
    }
}

#[test]
fn v4_raw_session_present_is_retained_across_an_effectively_fresh_reconnect() {
    let mut client = Client::configured(1, false, false, false, |config| {
        let wc::ProtocolConfig::V4(v4) = &mut config.protocol else {
            unreachable!()
        };
        v4.session_present_mismatch_policy = wc::SessionPresentMismatchPolicy::AcceptAsClean;
    });
    assert_eq!(client.peer.frame().0, 0x10);
    let observer = acquire(&client);
    client.peer.send(vec![0x20, 2, 1, 0]);
    assert_eq!(observe(observer, 3000), (0, 2, 1, 1));
    let event = client.event();
    unsafe { rumqttc_event_destroy(event) };
    let connack = unsafe { &*client.raw }
        .inner
        .handle
        .diagnostics_snapshot()
        .native
        .unwrap()
        .session
        .connack
        .unwrap();
    assert!(connack.raw_session_present && !connack.session_resumed);
    client.reconnect();
    let late = acquire(&client);
    client.destroy();
    assert_eq!(observe(observer, 0), (0, 2, 1, 1));
    assert_eq!(observe(late, 0), (0, 2, 1, 1));
    unsafe {
        rumqttc_first_connection_release(observer);
        rumqttc_first_connection_release(late);
    }
}

#[cfg(feature = "readiness")]
unsafe fn ready_entry(
    context: *const rumqttc_notification_context,
) -> rumqttc_notification_ready_t {
    let mut entry: rumqttc_notification_ready_t = unsafe { std::mem::zeroed() };
    let mut count = 0;
    let until = std::time::Instant::now() + DEADLINE;
    loop {
        let status = unsafe {
            rumqttc_notification_collect(
                context,
                &raw mut entry,
                size_of::<rumqttc_notification_ready_t>(),
                1,
                &raw mut count,
                ptr::null_mut(),
            )
        };
        if status == 0 {
            assert_eq!(count, 1);
            return entry;
        }
        assert_eq!(status, 11);
        assert!(std::time::Instant::now() < until);
        thread::yield_now();
    }
}

#[cfg(feature = "readiness")]
#[test]
fn readiness_retains_observation_without_client_execution_or_event_consumption() {
    for (protocol, success) in [(1, false), (1, true), (2, false), (2, true)] {
        let mut client = Client::configured(protocol, true, false, false, |_| {});
        assert_eq!(client.peer.frame().0, 0x10);
        let observer = acquire(&client);
        unsafe {
            let mut context = ptr::null_mut();
            let mut key = rumqttc_notification_key_t::default();
            assert_eq!(
                rumqttc_notification_context_new(ptr::null(), &raw mut context, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_notification_register_first_connection(
                    context,
                    observer,
                    17,
                    &raw mut key,
                    ptr::null_mut()
                ),
                0
            );
            rumqttc_first_connection_release(observer);
            let expected = if success {
                client.peer.send(if protocol == 1 {
                    vec![0x20, 2, 0, 0]
                } else {
                    vec![0x20, 3, 0, 0, 0]
                });
                (0, 2, protocol, 0)
            } else {
                assert_eq!(
                    rumqttc_client_close_now_timeout_ms(client.raw, 3000, ptr::null_mut()),
                    0
                );
                (6, 3, 0, 0)
            };
            let observer = acquire(&client);
            assert_eq!(observe(observer, 3000), expected);
            if success {
                rumqttc_event_destroy(client.event());
            }
            client.destroy();
            let entry = ready_entry(context);
            assert_eq!((entry.source_kind, entry.tag, entry.flags), (4, 17, 0));
            assert_eq!(
                rumqttc_notification_acknowledge(context, key, entry.revision, ptr::null_mut()),
                0
            );
            let mut service = 99;
            assert_eq!(
                rumqttc_notification_prepare_wait(context, &raw mut service, ptr::null_mut()),
                0
            );
            assert_eq!(service, 0);
            assert_eq!(observe(observer, 0), expected);
            let mut late = rumqttc_notification_key_t::default();
            assert_eq!(
                rumqttc_notification_register_first_connection(
                    context,
                    observer,
                    18,
                    &raw mut late,
                    ptr::null_mut()
                ),
                0
            );
            let entry = ready_entry(context);
            assert_eq!(
                (entry.key.slot, entry.key.generation),
                (late.slot, late.generation)
            );
            rumqttc_first_connection_release(observer);
            assert_eq!(
                rumqttc_notification_context_request_close(context, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_notification_context_try_join(context, ptr::null_mut()),
                0
            );
            rumqttc_notification_context_release(context);
        }
    }
}
