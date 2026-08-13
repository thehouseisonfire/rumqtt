#![allow(clippy::borrow_as_ptr)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::ptr;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rumqttc::*;

const fn string_view(value: &str) -> rumqttc_string_view_t {
    rumqttc_string_view_t {
        data: value.as_ptr().cast(),
        len: value.len(),
    }
}

const fn bytes_view(value: &[u8]) -> rumqttc_bytes_view_t {
    rumqttc_bytes_view_t {
        data: value.as_ptr(),
        len: value.len(),
    }
}

fn read_frame(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut header = [0];
    stream.read_exact(&mut header).ok()?;
    let mut multiplier = 1usize;
    let mut len = 0usize;
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).ok()?;
        len += usize::from(byte[0] & 0x7f) * multiplier;
        if byte[0] & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0; len];
    stream.read_exact(&mut body).ok()?;
    Some((header[0], body))
}

fn spawn_broker(protocol: u32) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let join = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (header, _) = read_frame(&mut stream).unwrap();
        assert_eq!(header >> 4, 1);
        if protocol == 1 {
            stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
        } else {
            stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
        }
        while let Some((header, body)) = read_frame(&mut stream) {
            match header >> 4 {
                3 => {
                    let qos = (header >> 1) & 0x03;
                    if qos == 0 {
                        continue;
                    }
                    let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                    let packet_id = [body[2 + topic_len], body[3 + topic_len]];
                    if qos == 1 {
                        if protocol == 1 {
                            stream
                                .write_all(&[0x40, 0x02, packet_id[0], packet_id[1]])
                                .unwrap();
                        } else {
                            stream
                                .write_all(&[0x40, 0x04, packet_id[0], packet_id[1], 0x00, 0x00])
                                .unwrap();
                        }
                    } else {
                        stream
                            .write_all(&[0x50, 0x02, packet_id[0], packet_id[1]])
                            .unwrap();
                    }
                }
                6 => {
                    stream.write_all(&[0x70, 0x02, body[0], body[1]]).unwrap();
                }
                8 => {
                    if protocol == 1 {
                        stream
                            .write_all(&[0x90, 0x03, body[0], body[1], 0x01])
                            .unwrap();
                    } else {
                        stream
                            .write_all(&[0x90, 0x04, body[0], body[1], 0x00, 0x01])
                            .unwrap();
                    }
                }
                10 => {
                    if protocol == 1 {
                        stream.write_all(&[0xb0, 0x02, body[0], body[1]]).unwrap();
                    } else {
                        stream
                            .write_all(&[0xb0, 0x04, body[0], body[1], 0x00, 0x11])
                            .unwrap();
                    }
                }
                14 => break,
                _ => {}
            }
        }
    });
    (port, join)
}

fn spawn_incoming_broker(protocol: u32) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let join = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(read_frame(&mut stream).unwrap().0 >> 4, 1);
        if protocol == 1 {
            stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
        } else {
            stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
        }

        let topic = b"ffi/incoming";
        let mut body = Vec::new();
        body.extend_from_slice(&u16::try_from(topic.len()).unwrap().to_be_bytes());
        body.extend_from_slice(topic);
        body.extend_from_slice(&[0, 7]);
        if protocol == 2 {
            body.extend_from_slice(&[4, 0x0b, 7, 0x0b, 9]);
        }
        body.extend_from_slice(&[0, 9, 0]);
        stream
            .write_all(&[0x32, u8::try_from(body.len()).unwrap()])
            .unwrap();
        stream.write_all(&body).unwrap();

        while let Some((header, body)) = read_frame(&mut stream) {
            if header >> 4 == 4 {
                assert_eq!(&body[..2], &[0, 7]);
                break;
            }
        }
        while let Some((header, _)) = read_frame(&mut stream) {
            if header >> 4 == 14 {
                break;
            }
        }
    });
    (port, join)
}

fn spawn_stalled_publish_broker() -> (u16, mpsc::Receiver<()>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (publish_tx, publish_rx) = mpsc::channel();
    let join = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(read_frame(&mut stream).unwrap().0 >> 4, 1);
        stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
        let mut publish_tx = Some(publish_tx);
        while let Some((header, _)) = read_frame(&mut stream) {
            if header >> 4 == 3
                && let Some(sender) = publish_tx.take()
            {
                sender.send(()).unwrap();
            }
        }
    });
    (port, publish_rx, join)
}

#[allow(clippy::too_many_lines)]
fn assert_protocol_round_trip(protocol: u32) {
    // SAFETY: This test owns every handle and provides valid views and output locations for each
    // ABI call.
    unsafe {
        let (port, broker) = spawn_broker(protocol);
        let mut error = ptr::null_mut();
        let mut config = ptr::null_mut();
        assert_eq!(rumqttc_config_new(protocol, &mut config, &mut error), 0);
        assert!(error.is_null());
        assert_eq!(
            rumqttc_config_set_broker(config, string_view("127.0.0.1"), port, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_config_set_client_id(config, string_view("c-abi-test"), ptr::null_mut()),
            0
        );

        let mut client = ptr::null_mut();
        assert_eq!(rumqttc_client_start(config, &mut client, &mut error), 0);
        rumqttc_config_destroy(config);
        assert!(error.is_null());

        let mut event = ptr::null_mut();
        assert_eq!(
            rumqttc_client_event_recv_timeout_ms(client, 2_000, &mut event, &mut error),
            0
        );
        let mut kind = 0;
        assert_eq!(rumqttc_event_kind(event, &mut kind), 0);
        assert_eq!(kind, 1);
        rumqttc_event_destroy(event);

        for (qos, expected_kind, payload) in [
            (0, 1, &[0, 0][..]),
            (1, 2, &[0, 1, 0, 2, 255][..]),
            (2, 3, &[2, 0, 2][..]),
        ] {
            let options = rumqttc_publish_options_t {
                struct_size: u32::try_from(size_of::<rumqttc_publish_options_t>()).unwrap(),
                qos,
                retain: 0,
                reserved: [0; 3],
                protocol_options: 0,
                v5_properties: ptr::null(),
            };
            let mut completion = ptr::null_mut();
            assert_eq!(
                rumqttc_client_publish_tracked(
                    client,
                    string_view("ffi/binary"),
                    bytes_view(payload),
                    &options,
                    &mut completion,
                    &mut error,
                ),
                0
            );
            assert_eq!(
                rumqttc_completion_wait_timeout_ms(completion, 2_000, &mut error),
                0
            );
            assert_eq!(
                rumqttc_completion_kind(completion, &mut kind, &mut error),
                0
            );
            assert_eq!(kind, expected_kind);
            rumqttc_completion_destroy(completion);
        }

        let user_property = rumqttc_user_property_t {
            struct_size: u32::try_from(size_of::<rumqttc_user_property_t>()).unwrap(),
            name: string_view("source"),
            value: string_view("ffi"),
        };
        let v5_filter_options = rumqttc_v5_subscription_options_t {
            struct_size: u32::try_from(size_of::<rumqttc_v5_subscription_options_t>()).unwrap(),
            no_local: 1,
            retain_as_published: 1,
            reserved: [0; 2],
            retain_forward_rule: 1,
        };
        let v5_subscribe_properties = rumqttc_v5_subscribe_properties_t {
            struct_size: u32::try_from(size_of::<rumqttc_v5_subscribe_properties_t>()).unwrap(),
            subscription_identifier_present: 1,
            reserved: [0; 3],
            subscription_identifier: 7,
            user_properties: &user_property,
            user_property_count: 1,
        };
        let subscribe_options = rumqttc_subscribe_options_t {
            struct_size: u32::try_from(size_of::<rumqttc_subscribe_options_t>()).unwrap(),
            protocol_options: if protocol == 2 { 5 } else { 0 },
            v5_properties: if protocol == 2 {
                &v5_subscribe_properties
            } else {
                ptr::null()
            },
        };
        let subscription = rumqttc_subscription_t {
            struct_size: u32::try_from(size_of::<rumqttc_subscription_t>()).unwrap(),
            filter: string_view("ffi/events"),
            qos: 1,
            protocol_options: if protocol == 2 { 5 } else { 0 },
            v5_options: if protocol == 2 {
                &v5_filter_options
            } else {
                ptr::null()
            },
        };
        let mut completion = ptr::null_mut();
        assert_eq!(
            rumqttc_client_subscribe_tracked(
                client,
                &subscription,
                1,
                &subscribe_options,
                &mut completion,
                ptr::null_mut(),
            ),
            0
        );
        assert_eq!(
            rumqttc_completion_wait_timeout_ms(completion, 2_000, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_completion_kind(completion, &mut kind, ptr::null_mut()),
            0
        );
        assert_eq!(kind, 4);
        let (mut success, mut granted, mut reason_present, mut reason) = (0, 0, 0, 0);
        assert_eq!(
            rumqttc_completion_result_at(
                completion,
                0,
                &mut success,
                &mut granted,
                &mut reason_present,
                &mut reason,
                ptr::null_mut(),
            ),
            0
        );
        assert_eq!((success, granted, reason_present), (1, 1, 0));
        rumqttc_completion_destroy(completion);

        let filter = string_view("ffi/events");
        let v5_unsubscribe_properties = rumqttc_v5_unsubscribe_properties_t {
            struct_size: u32::try_from(size_of::<rumqttc_v5_unsubscribe_properties_t>()).unwrap(),
            user_properties: &user_property,
            user_property_count: 1,
        };
        let unsubscribe_options = rumqttc_unsubscribe_options_t {
            struct_size: u32::try_from(size_of::<rumqttc_unsubscribe_options_t>()).unwrap(),
            protocol_options: if protocol == 2 { 5 } else { 0 },
            v5_properties: if protocol == 2 {
                &v5_unsubscribe_properties
            } else {
                ptr::null()
            },
        };
        completion = ptr::null_mut();
        assert_eq!(
            rumqttc_client_unsubscribe_tracked(
                client,
                &filter,
                1,
                &unsubscribe_options,
                &mut completion,
                ptr::null_mut(),
            ),
            0
        );
        assert_eq!(
            rumqttc_completion_wait_timeout_ms(completion, 2_000, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_completion_kind(completion, &mut kind, ptr::null_mut()),
            0
        );
        assert_eq!(kind, 5);
        let mut count = usize::MAX;
        assert_eq!(
            rumqttc_completion_result_count(completion, &mut count, ptr::null_mut()),
            0
        );
        assert_eq!(count, usize::from(protocol == 2));
        if protocol == 2 {
            assert_eq!(
                rumqttc_completion_result_at(
                    completion,
                    0,
                    &mut success,
                    &mut granted,
                    &mut reason_present,
                    &mut reason,
                    ptr::null_mut(),
                ),
                0
            );
            assert_eq!((success, granted, reason_present, reason), (1, 0, 1, 0x11));
        }
        rumqttc_completion_destroy(completion);

        assert_eq!(
            rumqttc_client_close_now_timeout_ms(client, 5000, &mut error),
            0
        );
        assert_eq!(
            rumqttc_client_close_now_timeout_ms(client, 5000, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_client_destroy_timeout_ms(client, 5_000, ptr::null_mut()),
            0
        );
        broker.join().unwrap();
    }
}

#[test]
fn v311_c_boundary_round_trip() {
    assert_protocol_round_trip(1);
}

#[test]
fn v5_c_boundary_round_trip() {
    assert_protocol_round_trip(2);
}

#[test]
fn invalid_inputs_initialize_outputs_and_return_owned_errors() {
    // SAFETY: The deliberately invalid values exercise validation paths that do not dereference
    // them; all non-null output locations are valid for writes.
    unsafe {
        let sentinel = ptr::dangling_mut::<rumqttc_config>();
        let mut config = sentinel;
        let mut error = ptr::null_mut();
        assert_eq!(rumqttc_config_new(99, &mut config, &mut error), 1);
        assert!(config.is_null());
        assert!(!error.is_null());

        let mut status = 0;
        assert_eq!(rumqttc_error_status(error, &mut status), 0);
        assert_eq!(status, 1);
        let mut kind = u32::MAX;
        assert_eq!(rumqttc_error_kind(error, &mut kind), 0);
        assert_eq!(kind, 0);
        rumqttc_error_destroy(error);

        let invalid = rumqttc_string_view_t {
            data: ptr::null(),
            len: 1,
        };
        assert_eq!(rumqttc_string_copy(invalid, ptr::null_mut(), 0, &mut 0), 1);
    }
}

#[test]
fn concurrent_close_honors_each_callers_timeout() {
    let (port, publish_rx, broker) = spawn_stalled_publish_broker();
    // SAFETY: This test owns every handle and keeps the client alive until both concurrent calls
    // have returned.
    unsafe {
        let mut config = ptr::null_mut();
        assert_eq!(rumqttc_config_new(1, &mut config, ptr::null_mut()), 0);
        assert_eq!(
            rumqttc_config_set_broker(config, string_view("127.0.0.1"), port, ptr::null_mut(),),
            0
        );
        assert_eq!(
            rumqttc_config_set_client_id(config, string_view("close-race"), ptr::null_mut()),
            0
        );
        let mut client = ptr::null_mut();
        assert_eq!(
            rumqttc_client_start(config, &mut client, ptr::null_mut()),
            0
        );
        rumqttc_config_destroy(config);

        let mut event = ptr::null_mut();
        assert_eq!(
            rumqttc_client_event_recv_timeout_ms(client, 2_000, &mut event, ptr::null_mut()),
            0
        );
        rumqttc_event_destroy(event);

        let options = rumqttc_publish_options_t {
            struct_size: u32::try_from(size_of::<rumqttc_publish_options_t>()).unwrap(),
            qos: 1,
            retain: 0,
            reserved: [0; 3],
            protocol_options: 0,
            v5_properties: ptr::null(),
        };
        let mut completion = ptr::null_mut();
        assert_eq!(
            rumqttc_client_publish_tracked(
                client,
                string_view("ffi/stalled"),
                bytes_view(b"pending"),
                &options,
                &mut completion,
                ptr::null_mut(),
            ),
            0
        );
        publish_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let client_address = client as usize;
        let first = thread::spawn(move || {
            let client = client_address as *mut rumqttc_client;
            let mut error = ptr::null_mut();
            let status = rumqttc_client_close_timeout_ms(client, 800, &mut error);
            rumqttc_error_destroy(error);
            status
        });
        thread::sleep(Duration::from_millis(25));

        let started = Instant::now();
        let mut error = ptr::null_mut();
        let status = rumqttc_client_close_timeout_ms(client, 25, &mut error);
        let elapsed = started.elapsed();
        assert_eq!(status, 5);
        assert!(elapsed < Duration::from_millis(300), "elapsed: {elapsed:?}");
        rumqttc_error_destroy(error);

        assert_eq!(first.join().unwrap(), 5);
        rumqttc_completion_destroy(completion);
        assert_eq!(
            rumqttc_client_destroy_timeout_ms(client, 5_000, ptr::null_mut()),
            0
        );
    }
    broker.join().unwrap();
}

fn assert_manual_ack(protocol: u32) {
    // SAFETY: This test owns every handle and provides valid views and output locations for each
    // ABI call.
    unsafe {
        let (port, broker) = spawn_incoming_broker(protocol);
        let mut config = ptr::null_mut();
        assert_eq!(
            rumqttc_config_new(protocol, &mut config, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_config_set_broker(config, string_view("127.0.0.1"), port, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_config_set_client_id(config, string_view("manual-ack"), ptr::null_mut()),
            0
        );
        assert_eq!(rumqttc_config_set_ack_mode(config, 1, ptr::null_mut()), 0);
        let mut client = ptr::null_mut();
        assert_eq!(
            rumqttc_client_start(config, &mut client, ptr::null_mut()),
            0
        );
        rumqttc_config_destroy(config);

        let mut event = ptr::null_mut();
        assert_eq!(
            rumqttc_client_event_recv_timeout_ms(client, 2_000, &mut event, ptr::null_mut()),
            0
        );
        rumqttc_event_destroy(event);
        event = ptr::null_mut();
        assert_eq!(
            rumqttc_client_event_recv_timeout_ms(client, 2_000, &mut event, ptr::null_mut()),
            0
        );
        let mut topic = rumqttc_string_view_t {
            data: ptr::null(),
            len: 0,
        };
        let mut payload = rumqttc_bytes_view_t {
            data: ptr::null(),
            len: 0,
        };
        let (mut qos, mut retain, mut duplicate, mut ack) = (0, 0, 0, 0);
        assert_eq!(
            rumqttc_event_publish(
                event,
                &mut topic,
                &mut payload,
                &mut qos,
                &mut retain,
                &mut duplicate,
                &mut ack,
            ),
            0
        );
        assert_eq!(
            std::slice::from_raw_parts(topic.data.cast::<u8>(), topic.len),
            b"ffi/incoming"
        );
        assert_eq!(
            std::slice::from_raw_parts(payload.data, payload.len),
            &[0, 9, 0]
        );
        assert_eq!((qos, ack), (1, 1));
        if protocol == 2 {
            let mut count = 0;
            assert_eq!(
                rumqttc_event_v5_subscription_identifier_count(event, &mut count),
                0
            );
            assert_eq!(count, 2);
            for (index, expected) in [7, 9].into_iter().enumerate() {
                let mut identifier = 0;
                assert_eq!(
                    rumqttc_event_v5_subscription_identifier_at(event, index, &mut identifier),
                    0
                );
                assert_eq!(identifier, expected);
            }
        }

        let mut completion = ptr::null_mut();
        assert_eq!(
            rumqttc_client_acknowledge_tracked(client, event, &mut completion, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_completion_wait_timeout_ms(completion, 2_000, ptr::null_mut()),
            0
        );
        let mut kind = 0;
        assert_eq!(
            rumqttc_completion_kind(completion, &mut kind, ptr::null_mut()),
            0
        );
        assert_eq!(kind, 6);
        rumqttc_completion_destroy(completion);
        completion = ptr::dangling_mut();
        let mut state_error = ptr::null_mut();
        assert_eq!(
            rumqttc_client_acknowledge_tracked(client, event, &mut completion, &mut state_error),
            2
        );
        assert!(completion.is_null());
        assert!(!state_error.is_null());
        let mut error_kind = 0;
        assert_eq!(rumqttc_error_kind(state_error, &mut error_kind), 0);
        assert_eq!(error_kind, 10);
        rumqttc_error_destroy(state_error);
        rumqttc_event_destroy(event);
        assert_eq!(
            rumqttc_client_close_now_timeout_ms(client, 5000, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_client_destroy_timeout_ms(client, 5_000, ptr::null_mut()),
            0
        );
        broker.join().unwrap();
    }
}

#[test]
fn manual_acknowledgement_is_event_bound_for_both_protocols() {
    assert_manual_ack(1);
    assert_manual_ack(2);
}
