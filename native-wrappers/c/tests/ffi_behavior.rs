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

#[cfg(unix)]
#[test]
fn tcp_broker_replaces_unix_transport_and_connects() {
    // SAFETY: Every view refers to live storage, and this test owns all C handles.
    unsafe {
        for protocol in [1, 2] {
            let (port, broker) = spawn_broker(protocol);
            let mut config = ptr::null_mut();
            assert_eq!(
                rumqttc_config_new(protocol, &mut config, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_config_set_unix_broker(
                    config,
                    bytes_view(b"/tmp/rumqttc-unused-broker.sock"),
                    ptr::null_mut(),
                ),
                0
            );
            assert_eq!(
                rumqttc_config_set_broker(config, string_view("127.0.0.1"), port, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_config_set_client_id(
                    config,
                    string_view("unix-to-tcp-regression"),
                    ptr::null_mut(),
                ),
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
            let mut kind = 0;
            assert_eq!(rumqttc_event_kind(event, &mut kind), 0);
            assert_eq!(kind, 1);
            rumqttc_event_destroy(event);
            assert_eq!(
                rumqttc_client_close_timeout_ms(client, 2_000, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_client_destroy_timeout_ms(client, 2_000, ptr::null_mut()),
                0
            );
            broker.join().unwrap();
        }
    }
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
            let mut ack = rumqttc_acknowledgement_details_t {
                struct_size: u32::try_from(size_of::<rumqttc_acknowledgement_details_t>()).unwrap(),
                protocol: 0,
                packet_kind: 0,
                packet_id: 0,
                present: 0,
                reason_present: 0,
                reason: 0,
                properties_present: 0,
                recovered: 0,
                reserved: [0; 5],
            };
            assert_eq!(
                rumqttc_completion_acknowledgement(completion, &mut ack, &mut error),
                0
            );
            assert!(error.is_null());
            assert_eq!(ack.present, u8::from(qos != 0));
            if qos != 0 {
                assert_eq!(ack.protocol, protocol);
                assert_eq!(ack.packet_kind, if qos == 1 { 4 } else { 7 });
                assert_ne!(ack.packet_id, 0);
                assert_eq!(ack.reason_present, u8::from(protocol == 2));
                assert_eq!(ack.reason, 0);
            }
            assert_eq!(ack.properties_present, 0);
            assert_eq!(ack.recovered, 0);
            let mut present = 9;
            let mut count = usize::MAX;
            assert_eq!(
                rumqttc_completion_acknowledgement_result_count(
                    completion,
                    &mut present,
                    &mut count,
                    ptr::null_mut()
                ),
                2
            );
            assert_eq!((present, count), (0, 0));
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
fn v4_c_boundary_round_trip() {
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
        let mut code = rumqttc_string_view_t {
            data: ptr::null(),
            len: 0,
        };
        assert_eq!(rumqttc_error_code(error, &mut code), 0);
        let code = std::slice::from_raw_parts(code.data.cast::<u8>(), code.len);
        assert_eq!(code, b"INVALID_ARGUMENT");
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

#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
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

        // Every rejected option attempt must preserve the event's acknowledgement token.
        let mut content: rumqttc_v5_acknowledgement_options_t = std::mem::zeroed();
        content.struct_size = u32::try_from(std::mem::size_of_val(&content)).unwrap();
        let mut options: rumqttc_acknowledgement_options_t = std::mem::zeroed();
        options.struct_size = u32::try_from(std::mem::size_of_val(&options)).unwrap();
        options.protocol_options = 5;
        options.v5_options = &content;
        for reason in if protocol == 1 {
            vec![0, 0x80]
        } else {
            vec![0x10, 0xff]
        } {
            content.reason_code = reason;
            options.v5_options = &raw const content;
            let mut operation = u64::MAX;
            let mut rejected_completion = ptr::dangling_mut();
            assert_ne!(
                rumqttc_client_try_acknowledge_with_options(
                    client,
                    event,
                    &options,
                    &mut operation,
                    ptr::null_mut()
                ),
                0
            );
            assert_eq!(operation, 0);
            assert_ne!(
                rumqttc_client_acknowledge_with_options_tracked(
                    client,
                    event,
                    &options,
                    &mut rejected_completion,
                    ptr::null_mut()
                ),
                0
            );
            assert!(rejected_completion.is_null());
            let mut available = 0;
            assert_eq!(
                rumqttc_event_publish(
                    event,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut available
                ),
                0
            );
            assert_eq!(available, 1);
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
        assert_eq!(error_kind, 2);
        let mut error_code = rumqttc_string_view_t {
            data: ptr::null(),
            len: 0,
        };
        assert_eq!(rumqttc_error_code(state_error, &mut error_code), 0);
        let error_code = std::slice::from_raw_parts(error_code.data.cast::<u8>(), error_code.len);
        assert_eq!(error_code, b"INVALID_STATE");
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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn acknowledgement_options_serialize_duplicate_calls_and_close_races() {
    // Handles stay live until every concurrent call has returned. Event/client destruction may
    // then overlap the driver's processing of admitted work, but never a call using their pointers.
    unsafe {
        for race_close in [false, true] {
            let (port, broker) = spawn_incoming_broker(2);
            let mut config = ptr::null_mut();
            assert_eq!(rumqttc_config_new(2, &mut config, ptr::null_mut()), 0);
            assert_eq!(
                rumqttc_config_set_broker(config, string_view("127.0.0.1"), port, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_config_set_client_id(config, string_view("ack-race"), ptr::null_mut()),
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
            assert_eq!(
                rumqttc_client_event_recv_timeout_ms(client, 2_000, &mut event, ptr::null_mut()),
                0
            );
            let barrier =
                std::sync::Arc::new(std::sync::Barrier::new(if race_close { 4 } else { 3 }));
            #[allow(
                clippy::needless_collect,
                reason = "All workers must start before the barrier is released"
            )]
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    let barrier = std::sync::Arc::clone(&barrier);
                    let client = client as usize;
                    let event = event as usize;
                    thread::spawn(move || {
                        let content = rumqttc_v5_acknowledgement_options_t {
                            struct_size: u32::try_from(std::mem::size_of::<
                                rumqttc_v5_acknowledgement_options_t,
                            >())
                            .unwrap(),
                            reason_code: 0x99,
                            reason_string_present: 0,
                            reserved: [0; 7],
                            reason_string: string_view(""),
                            user_properties: ptr::null(),
                            user_property_count: 0,
                        };
                        let options = rumqttc_acknowledgement_options_t {
                            struct_size: u32::try_from(std::mem::size_of::<
                                rumqttc_acknowledgement_options_t,
                            >())
                            .unwrap(),
                            protocol_options: 5,
                            v5_options: &content,
                            reserved: [0; 2],
                        };
                        let mut completion = ptr::dangling_mut();
                        barrier.wait();
                        let status = rumqttc_client_acknowledge_with_options_tracked(
                            client as *mut rumqttc_client,
                            event as *mut rumqttc_event,
                            &options,
                            &mut completion,
                            ptr::null_mut(),
                        );
                        if status != 0 {
                            assert!(completion.is_null());
                        }
                        (status, completion as usize)
                    })
                })
                .collect();
            let closer = race_close.then(|| {
                let barrier = std::sync::Arc::clone(&barrier);
                let client = client as usize;
                thread::spawn(move || {
                    barrier.wait();
                    assert_eq!(
                        rumqttc_client_close_now_timeout_ms(
                            client as *mut rumqttc_client,
                            5_000,
                            ptr::null_mut()
                        ),
                        0
                    );
                })
            });
            barrier.wait();
            let outcomes: Vec<_> = workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect();
            let admitted = outcomes.iter().filter(|(status, _)| *status == 0).count();
            assert!(admitted <= 1);
            if !race_close {
                assert_eq!(admitted, 1);
            }
            if let Some(closer) = closer {
                closer.join().unwrap();
            }
            // Destroy the event while an admitted ACK may still be owned by the driver.
            rumqttc_event_destroy(event);
            for (status, completion) in outcomes {
                if status == 0 {
                    let completion = completion as *mut rumqttc_completion;
                    let result =
                        rumqttc_completion_wait_timeout_ms(completion, 2_000, ptr::null_mut());
                    if !race_close {
                        assert_eq!(result, 0);
                    }
                    // A concurrent close may make delivery ambiguous, but cannot leave a waiter pending.
                    assert_ne!(result, 5);
                    rumqttc_completion_destroy(completion);
                }
            }
            assert_eq!(
                rumqttc_client_destroy_timeout_ms(client, 5_000, ptr::null_mut()),
                0
            );
            broker.join().unwrap();
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn added_configuration_records_validate_before_start() {
    // SAFETY: All handles and input arrays remain live for each call and are destroyed here.
    unsafe {
        let capabilities = rumqttc_library_capabilities();
        assert_eq!(capabilities & 3, 3);
        let mut v4 = ptr::null_mut();
        let mut v5 = ptr::null_mut();
        assert_eq!(rumqttc_config_new(1, &mut v4, ptr::null_mut()), 0);
        assert_eq!(rumqttc_config_new(2, &mut v5, ptr::null_mut()), 0);

        let mut properties = rumqttc_v5_will_properties_t {
            struct_size: u32::try_from(std::mem::size_of::<rumqttc_v5_will_properties_t>())
                .unwrap(),
            will_delay_present: 0,
            payload_format_present: 0,
            message_expiry_present: 0,
            content_type_present: 0,
            response_topic_present: 0,
            correlation_data_present: 0,
            reserved: [0; 2],
            will_delay_interval: 0,
            payload_format_indicator: 0,
            message_expiry_interval: 0,
            content_type: string_view(""),
            response_topic: string_view(""),
            correlation_data: bytes_view(&[]),
            user_properties: ptr::null(),
            user_property_count: 0,
        };
        let will = rumqttc_last_will_t {
            struct_size: u32::try_from(std::mem::size_of::<rumqttc_last_will_t>()).unwrap(),
            topic: string_view("test/will"),
            payload: bytes_view(b"payload"),
            qos: 1,
            retain: 0,
            reserved: [0; 3],
            protocol_options: 5,
            v5_properties: &properties,
        };
        assert_eq!(rumqttc_config_set_last_will(v4, &will, ptr::null_mut()), 3);
        assert_eq!(rumqttc_config_set_last_will(v5, &will, ptr::null_mut()), 0);
        properties.reserved[0] = 1;
        std::hint::black_box(&properties);
        assert_eq!(rumqttc_config_set_last_will(v5, &will, ptr::null_mut()), 1);
        assert_eq!(rumqttc_config_clear_last_will(v5, ptr::null_mut()), 0);

        assert_eq!(
            rumqttc_config_set_v4_inflight_limit(v5, 1, ptr::null_mut()),
            1
        );
        assert_eq!(
            rumqttc_config_set_v4_inflight_limit(v4, 0, ptr::null_mut()),
            1
        );
        assert_eq!(
            rumqttc_config_set_v4_inflight_limit(v4, 2, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_config_set_local_incoming_packet_limit_bytes(v5, 0, ptr::null_mut()),
            1
        );
        assert_eq!(
            rumqttc_config_set_local_incoming_packet_limit_mode(v5, 1, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_config_set_v5_topic_alias_policy(v4, 1, ptr::null_mut()),
            1
        );
        assert_eq!(
            rumqttc_config_set_v5_topic_alias_policy(v5, 2, ptr::null_mut()),
            0
        );

        let connect = rumqttc_v5_connect_properties_t {
            struct_size: u32::try_from(std::mem::size_of::<rumqttc_v5_connect_properties_t>())
                .unwrap(),
            session_expiry_present: 0,
            receive_maximum_present: 1,
            maximum_packet_size_present: 0,
            topic_alias_maximum_present: 0,
            request_response_info_present: 0,
            request_problem_info_present: 0,
            authentication_method_present: 0,
            authentication_data_present: 0,
            reserved: [0; 4],
            session_expiry_interval: 0,
            receive_maximum: 0,
            maximum_packet_size: 0,
            topic_alias_maximum: 0,
            request_response_information: 0,
            request_problem_information: 0,
            reserved_tail: [0; 2],
            authentication_method: string_view(""),
            authentication_data: bytes_view(&[]),
            user_properties: ptr::null(),
            user_property_count: 0,
        };
        assert_eq!(
            rumqttc_config_set_v5_connect_properties(v5, &connect, ptr::null_mut()),
            3
        );
        assert_eq!(
            rumqttc_config_clear_v5_connect_properties(v5, ptr::null_mut()),
            0
        );

        rumqttc_config_destroy(v4);
        rumqttc_config_destroy(v5);
    }
}

#[test]
fn disconnect_options_reject_v4_without_closing_it() {
    // SAFETY: The test owns the live client, config, and options throughout each call.
    unsafe {
        let (port, broker) = spawn_broker(1);
        let mut config = ptr::null_mut();
        assert_eq!(rumqttc_config_new(1, &mut config, ptr::null_mut()), 0);
        assert_eq!(
            rumqttc_config_set_broker(config, string_view("127.0.0.1"), port, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_config_set_client_id(config, string_view("v4-close-options"), ptr::null_mut()),
            0
        );
        let mut client = ptr::null_mut();
        assert_eq!(
            rumqttc_client_start(config, &mut client, ptr::null_mut()),
            0
        );
        rumqttc_config_destroy(config);
        let mut connected = ptr::null_mut();
        assert_eq!(
            rumqttc_client_event_recv_timeout_ms(client, 2_000, &mut connected, ptr::null_mut()),
            0
        );
        rumqttc_event_destroy(connected);
        let properties = rumqttc_v5_disconnect_properties_t {
            struct_size: u32::try_from(std::mem::size_of::<rumqttc_v5_disconnect_properties_t>())
                .unwrap(),
            reason_code: 0,
            session_expiry_present: 0,
            reason_string_present: 0,
            server_reference_present: 0,
            reserved: [0; 5],
            session_expiry_interval: 0,
            reason_string: string_view(""),
            server_reference: string_view(""),
            user_properties: ptr::null(),
            user_property_count: 0,
        };
        let options = rumqttc_disconnect_options_t {
            struct_size: u32::try_from(std::mem::size_of::<rumqttc_disconnect_options_t>())
                .unwrap(),
            protocol_options: 5,
            v5_properties: &properties,
            reserved: [0; 2],
        };
        assert_eq!(
            rumqttc_client_close_with_options_timeout_ms(client, 500, &options, ptr::null_mut()),
            1
        );
        assert_eq!(
            rumqttc_client_close_now_timeout_ms(client, 5_000, ptr::null_mut()),
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
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn explicit_tls_backend_matches_loaded_capabilities() {
    // SAFETY: Every C input points to live caller-owned storage for the call;
    // each returned handle is destroyed before the test ends.
    unsafe {
        let mut config = ptr::null_mut();
        assert_eq!(rumqttc_config_new(1, &mut config, ptr::null_mut()), 0);
        let mut options = rumqttc_tls_options_t {
            struct_size: u32::try_from(std::mem::size_of::<rumqttc_tls_options_t>()).unwrap(),
            backend: 1,
            root_policy: 0,
            reserved: 0,
            ca_pem: bytes_view(&[]),
            pem_identity: ptr::null(),
            pkcs12_identity: ptr::null(),
            alpn_protocols: ptr::null(),
            alpn_protocol_count: 0,
            reserved_tail: [0; 2],
        };
        let capabilities = rumqttc_library_capabilities();
        assert_eq!(
            rumqttc_config_set_transport_tls(
                config,
                bytes_view(&[]),
                bytes_view(&[]),
                bytes_view(&[]),
                ptr::null_mut()
            ),
            if capabilities & (1 << 2) == 0 { 3 } else { 0 }
        );
        options.backend = 0;
        assert_eq!(
            rumqttc_config_set_transport_tls_with_options(config, &options, ptr::null_mut()),
            if capabilities & (1 << 2) == 0 { 3 } else { 0 }
        );
        options.backend = 1;
        let status =
            rumqttc_config_set_transport_tls_with_options(config, &options, ptr::null_mut());
        if capabilities & (1 << 3) == 0 {
            assert_eq!(status, 3);
        } else {
            assert_eq!(status, 0);
            assert_eq!(
                rumqttc_config_set_broker(config, string_view("127.0.0.1"), 9, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_config_set_client_id(
                    config,
                    string_view("native-tls-c-test"),
                    ptr::null_mut()
                ),
                0
            );
            let mut client = ptr::null_mut();
            assert_eq!(
                rumqttc_client_start(config, &mut client, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_client_close_now_timeout_ms(client, 5_000, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_client_destroy_timeout_ms(client, 5_000, ptr::null_mut()),
                0
            );
        }
        rumqttc_config_destroy(config);

        let mut malformed = rumqttc_tls_options_t {
            struct_size: u32::try_from(std::mem::size_of::<rumqttc_tls_options_t>()).unwrap(),
            backend: 1,
            root_policy: 0,
            reserved: 1,
            ca_pem: bytes_view(&[]),
            pem_identity: ptr::null(),
            pkcs12_identity: ptr::null(),
            alpn_protocols: ptr::null(),
            alpn_protocol_count: 0,
            reserved_tail: [0; 2],
        };
        let mut config = ptr::null_mut();
        assert_eq!(rumqttc_config_new(2, &mut config, ptr::null_mut()), 0);
        assert_eq!(
            rumqttc_config_set_transport_tls_with_options(config, &malformed, ptr::null_mut()),
            1
        );
        malformed.reserved = 0;
        if capabilities & (1 << 3) != 0 {
            let pkcs12 = rumqttc_tls_pkcs12_identity_t {
                struct_size: u32::try_from(std::mem::size_of::<rumqttc_tls_pkcs12_identity_t>())
                    .unwrap(),
                reserved: 0,
                identity: bytes_view(b"dummy"),
                password: bytes_view(b"secret"),
                reserved_tail: [0; 2],
            };
            malformed.pkcs12_identity = &pkcs12;
            std::hint::black_box(&malformed);
            assert_eq!(
                rumqttc_config_set_transport_tls_with_options(config, &malformed, ptr::null_mut()),
                0
            );
        }
        rumqttc_config_destroy(config);

        if capabilities & (1 << 3) != 0 && capabilities & (1 << 4) != 0 {
            let mut config = ptr::null_mut();
            assert_eq!(rumqttc_config_new(2, &mut config, ptr::null_mut()), 0);
            assert_eq!(
                rumqttc_config_set_client_id(
                    config,
                    string_view("native-wss-c-test"),
                    ptr::null_mut()
                ),
                0
            );
            assert_eq!(
                rumqttc_config_set_transport_wss_with_options(
                    config,
                    string_view("wss://127.0.0.1:9/"),
                    &options,
                    ptr::null_mut()
                ),
                0
            );
            let mut client = ptr::null_mut();
            assert_eq!(
                rumqttc_client_start(config, &mut client, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_client_close_now_timeout_ms(client, 5_000, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_client_destroy_timeout_ms(client, 5_000, ptr::null_mut()),
                0
            );
            rumqttc_config_destroy(config);
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn v4_session_present_compatibility_is_additive_and_observed_separately() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert_eq!(read_frame(&mut stream).unwrap().0, 0x10);
        stream.write_all(&[0x20, 2, 1, 0]).unwrap();
        assert_eq!(read_frame(&mut stream).unwrap().0, 0xe0);
    });
    // SAFETY: This test owns all handles and every output points to live storage.
    unsafe {
        let mut config = ptr::null_mut();
        assert_eq!(rumqttc_config_new(1, &mut config, ptr::null_mut()), 0);
        assert_eq!(
            rumqttc_config_set_v4_session_present_mismatch_policy(config, 1, ptr::null_mut()),
            0
        );
        assert_ne!(
            rumqttc_config_set_v4_session_present_mismatch_policy(config, 99, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_config_set_broker(config, string_view("127.0.0.1"), port, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_config_set_client_id(config, string_view("compatibility"), ptr::null_mut()),
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
            rumqttc_client_event_recv_timeout_ms(client, 3_000, &mut event, ptr::null_mut()),
            0
        );
        let mut raw = 0;
        assert_eq!(rumqttc_event_connected(event, ptr::null_mut(), &mut raw), 0);
        assert_eq!(raw, 1);
        rumqttc_event_destroy(event);
        let mut completion = ptr::null_mut();
        assert_eq!(
            rumqttc_client_diagnostics_tracked(client, &mut completion, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_completion_wait_timeout_ms(completion, 3_000, ptr::null_mut()),
            0
        );
        let (mut present, mut resumed, mut diagnostic) = (0, 1, 0);
        assert_eq!(
            rumqttc_completion_connack_session_diagnostics(
                completion,
                &mut present,
                &mut raw,
                &mut resumed,
                &mut diagnostic,
                ptr::null_mut()
            ),
            0
        );
        assert_eq!((present, raw, resumed, diagnostic), (1, 1, 0, 1));
        assert_ne!(
            rumqttc_completion_connack_session_diagnostics(
                completion,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut()
            ),
            0
        );
        assert_ne!(
            rumqttc_completion_connack_session_diagnostics(
                ptr::null(),
                &mut present,
                &mut raw,
                &mut resumed,
                &mut diagnostic,
                ptr::null_mut()
            ),
            0
        );
        assert_eq!((present, raw, resumed, diagnostic), (0, 0, 0, 0));
        rumqttc_completion_destroy(completion);
        assert_eq!(
            rumqttc_client_close_timeout_ms(client, 3_000, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_client_destroy_timeout_ms(client, 3_000, ptr::null_mut()),
            0
        );
        let mut v5 = ptr::null_mut();
        assert_eq!(rumqttc_config_new(2, &mut v5, ptr::null_mut()), 0);
        assert_ne!(
            rumqttc_config_set_v4_session_present_mismatch_policy(v5, 0, ptr::null_mut()),
            0
        );
        assert_ne!(
            rumqttc_config_set_v4_session_present_mismatch_policy(v5, 1, ptr::null_mut()),
            0
        );
        rumqttc_config_destroy(v5);
    }
    broker.join().unwrap();
}

#[test]
#[allow(clippy::too_many_lines)]
fn runtime_updates_copy_builders_and_retain_redacted_observations_after_client_destruction() {
    for protocol in [1, 2] {
        // SAFETY: All handles are owned here; records and borrowed input/output buffers remain live during calls.
        unsafe {
            let (port, broker) = spawn_broker(protocol);
            let mut config = ptr::null_mut();
            let mut client = ptr::null_mut();
            let mut event = ptr::null_mut();
            let mut update = ptr::null_mut();
            let mut completion = ptr::null_mut();
            let mut receipt = ptr::null_mut();
            let mut snapshot = ptr::null_mut();
            assert_eq!(rumqttc_library_capabilities() & (1 << 17), 1 << 17);
            assert_eq!(
                rumqttc_config_new(protocol, &mut config, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_config_set_broker(config, string_view("127.0.0.1"), port, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_config_set_client_id(config, string_view("c-update"), ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_config_set_keep_alive_seconds(config, 0, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_client_start(config, &mut client, ptr::null_mut()),
                0
            );
            rumqttc_config_destroy(config);
            assert_eq!(
                rumqttc_client_event_recv_timeout_ms(client, 2_000, &mut event, ptr::null_mut()),
                0
            );
            rumqttc_event_destroy(event);
            assert_eq!(rumqttc_runtime_update_new(&mut update, ptr::null_mut()), 0);
            assert_eq!(
                rumqttc_runtime_update_set_max_request_batch(update, 7, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_runtime_update_set_credentials(
                    update,
                    1,
                    string_view("user"),
                    1,
                    bytes_view(&[0, 255, 9]),
                    ptr::null_mut()
                ),
                0
            );
            let mut network: rumqttc_runtime_network_options_t = std::mem::zeroed();
            network.struct_size = u32::try_from(size_of_val(&network)).unwrap();
            network.present_fields = 4;
            network.local_address = string_view("127.0.0.1:0");
            assert_eq!(
                rumqttc_runtime_update_set_network(update, &network, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_client_update_configuration_tracked(
                    client,
                    update,
                    &mut completion,
                    ptr::null_mut()
                ),
                0
            );
            // A source-builder edit cannot change the admitted proposal.
            assert_eq!(
                rumqttc_runtime_update_set_max_request_batch(update, 99, ptr::null_mut()),
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
            assert_eq!(kind, 12);
            assert_eq!(
                rumqttc_completion_configuration_receipt(completion, &mut receipt, ptr::null_mut()),
                0
            );
            rumqttc_completion_destroy(completion);
            assert_eq!(
                rumqttc_client_configuration_snapshot(client, &mut snapshot, ptr::null_mut()),
                0
            );
            let mut tuning: rumqttc_runtime_tuning_t = std::mem::zeroed();
            tuning.struct_size = u32::try_from(size_of_val(&tuning)).unwrap();
            assert_eq!(
                rumqttc_configuration_snapshot_tuning(snapshot, 0, &mut tuning, ptr::null_mut()),
                0
            );
            assert_eq!(tuning.max_request_batch, 7);
            assert_eq!(
                rumqttc_configuration_snapshot_network(snapshot, 0, &mut network, ptr::null_mut()),
                0
            );
            assert_eq!(network.present_fields, 4);
            assert_eq!(
                rumqttc_runtime_update_field_action(update, 5, 2, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_client_update_configuration_tracked(
                    client,
                    update,
                    &mut completion,
                    ptr::null_mut()
                ),
                0
            );
            let mut error = ptr::null_mut();
            assert_eq!(
                rumqttc_completion_wait_timeout_ms(completion, 2_000, &mut error),
                3
            );
            let mut code = string_view("");
            assert_eq!(rumqttc_error_code(error, &mut code), 0);
            assert_eq!(
                std::slice::from_raw_parts(code.data.cast::<u8>(), code.len),
                b"CONFIGURATION_UPDATE_UNSUPPORTED"
            );
            rumqttc_error_destroy(error);
            rumqttc_completion_destroy(completion);
            rumqttc_runtime_update_destroy(update);
            assert_eq!(
                rumqttc_client_close_now_timeout_ms(client, 2_000, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_client_destroy_timeout_ms(client, 2_000, ptr::null_mut()),
                0
            );
            let mut activation: rumqttc_configuration_receipt_status_t = std::mem::zeroed();
            activation.struct_size = u32::try_from(size_of_val(&activation)).unwrap();
            assert_eq!(
                rumqttc_configuration_receipt_status(receipt, &mut activation, ptr::null_mut()),
                0
            );
            assert_eq!(activation.revision, 1);
            assert_eq!(activation.connection_state, 4);
            // Owned snapshots and their views retain no client/configuration owner.
            assert_eq!(
                std::slice::from_raw_parts(
                    network.local_address.data.cast::<u8>(),
                    network.local_address.len
                ),
                b"127.0.0.1:0"
            );
            let mut status: rumqttc_configuration_status_t = std::mem::zeroed();
            status.struct_size = u32::try_from(size_of_val(&status)).unwrap();
            assert_eq!(
                rumqttc_configuration_snapshot_status(snapshot, &mut status, ptr::null_mut()),
                0
            );
            assert_eq!(status.revision, 1);
            rumqttc_configuration_receipt_destroy(receipt);
            rumqttc_configuration_snapshot_destroy(snapshot);
            broker.join().unwrap();
        }
    }
}

#[test]
fn runtime_configuration_accessors_initialize_failure_outputs_and_validate_record_sizes() {
    // SAFETY: Records have valid integer/pointer representations and are live for every call.
    unsafe {
        let mut status: rumqttc_configuration_status_t = std::mem::zeroed();
        status.struct_size = u32::try_from(size_of_val(&status)).unwrap();
        status.revision = 99;
        assert_eq!(
            rumqttc_configuration_snapshot_status(ptr::null(), &mut status, ptr::null_mut()),
            1
        );
        assert_eq!(status.revision, 0);
        status.struct_size = 4;
        status.revision = 99;
        assert_eq!(
            rumqttc_configuration_snapshot_status(ptr::null(), &mut status, ptr::null_mut()),
            1
        );
        assert_eq!(status.revision, 99);
        let mut tuning: rumqttc_runtime_tuning_t = std::mem::zeroed();
        tuning.struct_size = u32::try_from(size_of_val(&tuning)).unwrap();
        tuning.read_batch_size = 99;
        assert_eq!(
            rumqttc_configuration_snapshot_tuning(ptr::null(), 44, &mut tuning, ptr::null_mut()),
            1
        );
        assert_eq!(tuning.read_batch_size, 0);
        let mut present = 99;
        let mut revision = 99;
        assert_eq!(
            rumqttc_error_configuration_revision(ptr::null(), &mut present, &mut revision),
            1
        );
        assert_eq!((present, revision), (0, 0));
        let mut builder = ptr::null_mut();
        assert_eq!(rumqttc_runtime_update_new(&mut builder, ptr::null_mut()), 0);
        assert_eq!(
            rumqttc_runtime_update_field_action(builder, 44, 2, ptr::null_mut()),
            1
        );
        let mut network: rumqttc_runtime_network_options_t = std::mem::zeroed();
        network.struct_size = u32::try_from(size_of_val(&network)).unwrap();
        network.tcp_nodelay = 2;
        assert_eq!(
            rumqttc_runtime_update_set_network(builder, &network, ptr::null_mut()),
            1
        );
        network.tcp_nodelay = 1;
        network.reserved[0] = 1;
        assert_eq!(
            rumqttc_runtime_update_set_network(builder, &network, ptr::null_mut()),
            1
        );
        rumqttc_runtime_update_destroy(builder);
    }
}
