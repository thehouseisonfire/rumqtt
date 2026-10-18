use super::*;
use crate::client::ClientObject;
use crate::event::EventObject;
use crate::ffi::*;
use bytes::Bytes;
use rumqttc_wrapper_core as wc;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Barrier, mpsc};
use std::thread;
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(3);

struct FlushControl {
    hold: AtomicBool,
    fail: AtomicBool,
    pending: AtomicBool,
    entered: mpsc::Sender<()>,
    release: tokio::sync::Semaphore,
}

struct ScriptIo {
    input: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Bytes>>>,
    output: mpsc::Sender<Bytes>,
    flush: Arc<FlushControl>,
}

impl wc::TransportIo for ScriptIo {
    fn read(&self, _: usize) -> wc::TransportIoFuture<Bytes> {
        let input = Arc::clone(&self.input);
        Box::pin(async move { Ok(input.lock().await.recv().await.unwrap_or_default()) })
    }

    fn write(&self, bytes: Bytes) -> wc::TransportIoFuture<usize> {
        let output = self.output.clone();
        let control = Arc::clone(&self.flush);
        Box::pin(async move {
            if matches!(bytes.first(), Some(0x40 | 0x50)) {
                control.pending.store(true, Ordering::Release);
            }
            let length = bytes.len();
            output
                .send(bytes)
                .map_err(|_| std::io::Error::other("script peer released"))?;
            Ok(length)
        })
    }

    fn flush(&self) -> wc::TransportIoFuture<()> {
        let control = Arc::clone(&self.flush);
        Box::pin(async move {
            if control.pending.swap(false, Ordering::AcqRel) && control.hold.load(Ordering::Acquire)
            {
                control.entered.send(()).unwrap();
                control.release.acquire().await.unwrap().forget();
                if control.fail.swap(false, Ordering::AcqRel) {
                    return Err(std::io::Error::other("script ACK flush failure"));
                }
            }
            Ok(())
        })
    }

    fn shutdown(&self) -> wc::TransportIoFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

struct Connector {
    streams: Mutex<VecDeque<Arc<ScriptIo>>>,
    dropped: Arc<AtomicUsize>,
}

impl Drop for Connector {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl wc::TransportConnector for Connector {
    fn connect(&self, _: wc::TransportRequest) -> wc::TransportFuture {
        let stream = self.streams.lock().unwrap().pop_front();
        Box::pin(async move {
            Ok(wc::TransportConnection {
                io: stream.ok_or(wc::TransportFailure::Connect)?,
                mode: wc::TransportMode::Established,
                network_handling: wc::NetworkHandling::NotApplicable,
            })
        })
    }
}

struct Peer {
    input: tokio::sync::mpsc::Sender<Bytes>,
    output: mpsc::Receiver<Bytes>,
    buffered: Vec<u8>,
    flush: Arc<FlushControl>,
    entered: mpsc::Receiver<()>,
}

impl Peer {
    fn new() -> (Arc<ScriptIo>, Self) {
        let (input_tx, input_rx) = tokio::sync::mpsc::channel(16);
        let (output_tx, output_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let flush = Arc::new(FlushControl {
            hold: AtomicBool::new(false),
            fail: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            entered: entered_tx,
            release: tokio::sync::Semaphore::new(0),
        });
        (
            Arc::new(ScriptIo {
                input: Arc::new(tokio::sync::Mutex::new(input_rx)),
                output: output_tx,
                flush: Arc::clone(&flush),
            }),
            Self {
                input: input_tx,
                output: output_rx,
                buffered: Vec::new(),
                flush,
                entered: entered_rx,
            },
        )
    }

    fn send(&self, bytes: impl Into<Bytes>) {
        self.input.try_send(bytes.into()).unwrap();
    }

    fn frame(&mut self) -> (u8, Vec<u8>) {
        loop {
            if self.buffered.len() >= 2 {
                let mut length = 0;
                let mut shift = 0;
                for offset in 1..self.buffered.len().min(5) {
                    let byte = self.buffered[offset];
                    length |= usize::from(byte & 127) << shift;
                    shift += 7;
                    if byte & 128 == 0 {
                        let end = offset + 1 + length;
                        if self.buffered.len() >= end {
                            let header = self.buffered[0];
                            let body = self.buffered[offset + 1..end].to_vec();
                            self.buffered.drain(..end);
                            return (header, body);
                        }
                        break;
                    }
                }
            }
            self.buffered
                .extend_from_slice(&self.output.recv_timeout(DEADLINE).unwrap());
        }
    }

    fn publish(&self, protocol: u32, qos: u8, duplicate: bool, id: u8) {
        let mut body = vec![0, 1, b'a'];
        if qos != 0 {
            body.extend_from_slice(&[0, id]);
        }
        if protocol == 2 {
            body.push(0);
        }
        body.push(b'x');
        let mut frame = vec![0x30 | (qos << 1) | (u8::from(duplicate) << 3)];
        frame.push(u8::try_from(body.len()).unwrap());
        frame.extend(body);
        self.send(frame);
    }
}

struct Client {
    raw: *mut rumqttc_client,
    peer: Peer,
    connector: Option<Arc<Connector>>,
    context: Option<wc::ExecutionContext>,
    dropped: Arc<AtomicUsize>,
    protocol: u32,
}

impl Client {
    fn new(protocol: u32, shared: bool, manual: bool) -> Self {
        Self::configured(protocol, shared, manual, true, |_| {})
    }

    fn configured(
        protocol: u32,
        shared: bool,
        manual: bool,
        handshake: bool,
        configure: impl FnOnce(&mut wc::ClientConfig),
    ) -> Self {
        let (io, peer) = Peer::new();
        let dropped = Arc::new(AtomicUsize::new(0));
        let connector = Arc::new(Connector {
            streams: Mutex::new(VecDeque::from([io])),
            dropped: Arc::clone(&dropped),
        });
        let mut config = if protocol == 1 {
            wc::ClientConfig::v4("detached", "script.invalid", 1883)
        } else {
            wc::ClientConfig::v5("detached", "script.invalid", 1883)
        };
        config.common.ack_mode = if manual {
            wc::AckMode::Manual
        } else {
            wc::AckMode::Automatic
        };
        config.common.request_channel_capacity = 1;
        config.common.event_buffer_capacity = 1;
        config.common.event_delivery_timeout = Duration::from_secs(5);
        config.common.connector = Some(wc::TransportConnectorConfig {
            connector: connector.clone(),
            mode: wc::TransportMode::Established,
        });
        configure(&mut config);
        let context =
            shared.then(|| wc::ExecutionContext::new(wc::ExecutionOptions::default()).unwrap());
        let raw = Box::into_raw(Box::new(rumqttc_client {
            inner: ClientObject::start(config, context.as_ref(), None).unwrap(),
        }));
        let mut client = Self {
            raw,
            peer,
            connector: Some(connector),
            context,
            dropped,
            protocol,
        };
        if handshake {
            client.handshake();
        }
        client
    }

    fn handshake(&mut self) {
        assert_eq!(self.peer.frame().0, 0x10);
        self.peer.send(if self.protocol == 1 {
            vec![0x20, 2, 0, 0]
        } else {
            vec![0x20, 3, 0, 0, 0]
        });
        let event = self.event();
        assert!(matches!(
            unsafe { &*event }.inner.event,
            wc::WrapperEvent::Connected { .. }
        ));
        unsafe { rumqttc_event_destroy(event) };
    }

    fn event(&self) -> *mut rumqttc_event {
        let mut event = ptr::null_mut();
        assert_eq!(
            unsafe {
                rumqttc_client_event_recv_timeout_ms(
                    self.raw,
                    3000,
                    &raw mut event,
                    ptr::null_mut(),
                )
            },
            0
        );
        assert!(!event.is_null());
        event
    }

    fn publish(&self, qos: u8, duplicate: bool, id: u8) -> *mut rumqttc_event {
        self.peer.publish(self.protocol, qos, duplicate, id);
        self.event()
    }

    fn reconnect(&mut self) {
        let (io, peer) = Peer::new();
        self.connector
            .as_ref()
            .unwrap()
            .streams
            .lock()
            .unwrap()
            .push_back(io);
        self.peer.send(Bytes::new());
        let event = self.event();
        assert!(matches!(
            unsafe { &*event }.inner.event,
            wc::WrapperEvent::Disconnected { .. }
        ));
        unsafe { rumqttc_event_destroy(event) };
        self.peer = peer;
        self.handshake();
    }

    fn destroy(&mut self) {
        self.peer.flush.hold.store(false, Ordering::Release);
        self.peer.flush.release.add_permits(16);
        self.connector.take();
        assert_eq!(
            unsafe { rumqttc_client_destroy_timeout_ms(self.raw, 3000, ptr::null_mut()) },
            0
        );
        self.raw = ptr::null_mut();
        assert_eq!(self.dropped.load(Ordering::SeqCst), 1);
        if let Some(context) = self.context.take() {
            context.request_shutdown();
            context.join(DEADLINE).unwrap();
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            self.destroy();
        }
    }
}

fn take(event: *mut rumqttc_event) -> *mut rumqttc_ack_token {
    let mut token = ptr::null_mut();
    assert_eq!(
        unsafe { rumqttc_event_take_ack_token(event, &raw mut token, ptr::null_mut()) },
        0
    );
    token
}

fn retain(token: *const rumqttc_ack_token) -> *mut rumqttc_ack_token {
    let mut retained = ptr::null_mut();
    assert_eq!(
        unsafe { rumqttc_ack_token_retain(token, &raw mut retained, ptr::null_mut()) },
        0
    );
    retained
}

fn wait(completion: *mut rumqttc_completion) -> u32 {
    unsafe { rumqttc_completion_wait_timeout_ms(completion, 3000, ptr::null_mut()) }
}

struct PublicationOwner(Arc<AtomicUsize>);

impl AsRef<[u8]> for PublicationOwner {
    fn as_ref(&self) -> &[u8] {
        b"owned"
    }
}

impl Drop for PublicationOwner {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep lifetime setup, public ABI calls and teardown assertions together"
)]
fn detached_tokens_release_publication_storage_and_outlive_client_execution() {
    for protocol in [1, 2] {
        for qos in [1, 2] {
            for shared in [false, true] {
                let mut client = Client::new(protocol, shared, true);
                let event = client.publish(qos, false, 7);
                let dropped = Arc::new(AtomicUsize::new(0));
                // Replace only owned observation buffers, keeping the genuine core ACK authority.
                let wc::WrapperEvent::IncomingPublish(publication) =
                    &mut unsafe { &mut *event }.inner.event
                else {
                    panic!()
                };
                publication.topic = Bytes::from_owner(PublicationOwner(Arc::clone(&dropped)));
                publication.payload = Bytes::from_owner(PublicationOwner(Arc::clone(&dropped)));
                if protocol == 2 {
                    publication.v5_properties = Some(wc::V5IncomingPublishProperties {
                        correlation_data: Some(Bytes::from_owner(PublicationOwner(Arc::clone(
                            &dropped,
                        )))),
                        ..Default::default()
                    });
                }
                unsafe {
                    assert_eq!(
                        rumqttc_event_take_ack_token(event, ptr::null_mut(), ptr::null_mut()),
                        1
                    );
                    let token = take(event);
                    let alias = retain(token);
                    let mut rejected = ptr::dangling_mut();
                    assert_eq!(
                        rumqttc_event_take_ack_token(event, &raw mut rejected, ptr::null_mut()),
                        2
                    );
                    assert!(rejected.is_null());
                    let mut payload = rumqttc_bytes_view_t {
                        data: ptr::null(),
                        len: 0,
                    };
                    let mut available = 1;
                    assert_eq!(
                        rumqttc_event_publish(
                            event,
                            ptr::null_mut(),
                            &raw mut payload,
                            ptr::null_mut(),
                            ptr::null_mut(),
                            ptr::null_mut(),
                            &raw mut available
                        ),
                        0
                    );
                    assert_eq!(
                        std::slice::from_raw_parts(payload.data, payload.len),
                        b"owned"
                    );
                    assert_eq!(available, 0);
                    let mut operation = 99;
                    assert_eq!(
                        rumqttc_client_try_acknowledge(
                            client.raw,
                            event,
                            &raw mut operation,
                            ptr::null_mut()
                        ),
                        2
                    );
                    assert_eq!(operation, 0);
                    rumqttc_event_destroy(event);
                    assert_eq!(
                        dropped.load(Ordering::SeqCst),
                        if protocol == 2 { 3 } else { 2 }
                    );
                    rumqttc_ack_token_release(token);
                    let mut completion = ptr::null_mut();
                    assert_eq!(
                        rumqttc_client_acknowledge_token_tracked(
                            client.raw,
                            alias,
                            &raw mut completion,
                            ptr::null_mut()
                        ),
                        0
                    );
                    assert_eq!(
                        client.peer.frame(),
                        (if qos == 1 { 0x40 } else { 0x50 }, vec![0, 7])
                    );
                    assert_eq!(wait(completion), 0);
                    rumqttc_completion_destroy(completion);
                    assert_eq!(
                        rumqttc_client_try_acknowledge_token(
                            client.raw,
                            alias,
                            &raw mut operation,
                            ptr::null_mut()
                        ),
                        2
                    );
                    client.destroy();
                    let after_destroy = retain(alias);
                    rumqttc_ack_token_release(alias);
                    rumqttc_ack_token_release(after_destroy);
                }
            }
        }
    }
}

#[test]
fn absent_authority_and_null_arguments_initialize_outputs() {
    unsafe {
        let mut out = ptr::dangling_mut();
        let mut error = ptr::dangling_mut();
        assert_eq!(
            rumqttc_event_take_ack_token(ptr::null_mut(), &raw mut out, &raw mut error),
            1
        );
        assert!(out.is_null() && !error.is_null());
        rumqttc_error_destroy(error);
        assert_eq!(
            rumqttc_ack_token_retain(ptr::null(), &raw mut out, ptr::null_mut()),
            1
        );
        assert!(out.is_null());
        assert_eq!(
            rumqttc_ack_token_retain(ptr::null(), ptr::null_mut(), ptr::null_mut()),
            1
        );
        rumqttc_ack_token_release(ptr::null_mut());
        let wrong = Box::into_raw(Box::new(rumqttc_event {
            inner: EventObject::new(wc::WrapperEvent::GracefulShutdownCompleted),
        }));
        assert_eq!(
            rumqttc_event_take_ack_token(wrong, &raw mut out, ptr::null_mut()),
            2
        );
        rumqttc_event_destroy(wrong);
        for protocol in [1, 2] {
            for (manual, qos) in [(true, 0), (false, 1)] {
                let client = Client::new(protocol, false, manual);
                let event = client.publish(qos, false, 7);
                assert_eq!(
                    rumqttc_event_take_ack_token(event, &raw mut out, ptr::null_mut()),
                    2
                );
                assert!(out.is_null());
                rumqttc_event_destroy(event);
                let mut operation = 99;
                assert_eq!(
                    rumqttc_client_try_acknowledge_token(
                        client.raw,
                        ptr::null_mut(),
                        &raw mut operation,
                        ptr::null_mut()
                    ),
                    1
                );
                assert_eq!(operation, 0);
                let mut completion = ptr::dangling_mut();
                assert_eq!(
                    rumqttc_client_acknowledge_token_with_options_tracked(
                        client.raw,
                        ptr::null_mut(),
                        ptr::null(),
                        &raw mut completion,
                        ptr::null_mut()
                    ),
                    1
                );
                assert!(completion.is_null());
            }
        }
    }
}

fn custom_options(reason: u32) -> rumqttc_v5_acknowledgement_options_t {
    rumqttc_v5_acknowledgement_options_t {
        struct_size: u32::try_from(size_of::<rumqttc_v5_acknowledgement_options_t>()).unwrap(),
        reason_code: reason,
        reason_string_present: 0,
        reserved: [0; 7],
        reason_string: rumqttc_string_view_t {
            data: ptr::null(),
            len: 0,
        },
        user_properties: ptr::null(),
        user_property_count: 0,
    }
}

fn options(content: &rumqttc_v5_acknowledgement_options_t) -> rumqttc_acknowledgement_options_t {
    rumqttc_acknowledgement_options_t {
        struct_size: u32::try_from(size_of::<rumqttc_acknowledgement_options_t>()).unwrap(),
        protocol_options: 5,
        v5_options: content,
        reserved: [0; 2],
    }
}

#[test]
fn invalid_options_leave_detached_authority_retryable() {
    for protocol in [1, 2] {
        let mut client = Client::new(protocol, false, true);
        let event = client.publish(1, false, 7);
        let token = take(event);
        unsafe {
            rumqttc_event_destroy(event);
            for reason in if protocol == 1 {
                vec![0, 0x99]
            } else {
                vec![0x10, 0xff]
            } {
                let content = custom_options(reason);
                let mut operation = 99;
                assert_ne!(
                    rumqttc_client_try_acknowledge_token_with_options(
                        client.raw,
                        token,
                        &options(&content),
                        &raw mut operation,
                        ptr::null_mut()
                    ),
                    0
                );
                assert_eq!(operation, 0);
                let mut completion = ptr::dangling_mut();
                assert_ne!(
                    rumqttc_client_acknowledge_token_with_options_tracked(
                        client.raw,
                        token,
                        &options(&content),
                        &raw mut completion,
                        ptr::null_mut()
                    ),
                    0
                );
                assert!(completion.is_null());
            }
            let mut operation = 0;
            assert_eq!(
                rumqttc_client_try_acknowledge_token(
                    client.raw,
                    token,
                    &raw mut operation,
                    ptr::null_mut()
                ),
                0
            );
            assert_ne!(operation, 0);
            assert_eq!(client.peer.frame(), (0x40, vec![0, 7]));
            rumqttc_ack_token_release(token);
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the controlled flush and queue admission scenario together"
)]
fn backpressure_and_pending_or_failed_flush_preserve_admission_boundary() {
    for shared in [false, true] {
        for fail in [false, true] {
            let mut client = Client::new(2, shared, true);
            let first_event = client.publish(1, false, 7);
            let first = take(first_event);
            let second_event = client.publish(1, false, 8);
            let second = take(second_event);
            unsafe {
                rumqttc_event_destroy(first_event);
                rumqttc_event_destroy(second_event);
                client.peer.flush.hold.store(true, Ordering::Release);
                client.peer.flush.fail.store(fail, Ordering::Release);
                let alias = retain(first);
                let mut completion = ptr::null_mut();
                assert_eq!(
                    rumqttc_client_acknowledge_token_tracked(
                        client.raw,
                        first,
                        &raw mut completion,
                        ptr::null_mut()
                    ),
                    0
                );
                client.peer.entered.recv_timeout(DEADLINE).unwrap();
                assert_eq!(
                    rumqttc_completion_wait_timeout_ms(completion, 0, ptr::null_mut()),
                    5
                );
                let mut operation = 99;
                assert_eq!(
                    rumqttc_client_try_acknowledge_token(
                        client.raw,
                        alias,
                        &raw mut operation,
                        ptr::null_mut()
                    ),
                    2
                );
                // ACKs use the native control queue, separately from publish admission.
                // The active poll is held in flush, so this request fills that queue.
                (*client.raw)
                    .inner
                    .handle
                    .try_admit(wc::Command::Unsubscribe(wc::UnsubscribeCommand {
                        filters: vec!["a".into()],
                        protocol: wc::UnsubscribeProtocolOptions::VersionNeutral,
                    }))
                    .unwrap();
                let content = custom_options(0x99);
                assert_eq!(
                    rumqttc_client_try_acknowledge_token_with_options(
                        client.raw,
                        second,
                        &options(&content),
                        &raw mut operation,
                        ptr::null_mut()
                    ),
                    4
                );
                assert_eq!(operation, 0);
                client.peer.flush.hold.store(false, Ordering::Release);
                client.peer.flush.release.add_permits(1);
                if fail {
                    assert_ne!(wait(completion), 0);
                    let mut delivery = 0;
                    let mut error = ptr::null_mut();
                    assert_ne!(
                        rumqttc_completion_wait_timeout_ms(completion, 3000, &raw mut error),
                        0
                    );
                    assert_eq!(
                        rumqttc_error_context(
                            error,
                            ptr::null_mut(),
                            ptr::null_mut(),
                            ptr::null_mut(),
                            ptr::null_mut(),
                            &raw mut delivery
                        ),
                        0
                    );
                    assert_eq!(delivery, 3); // Ambiguous.
                    rumqttc_error_destroy(error);
                } else {
                    assert_eq!(wait(completion), 0);
                    assert_eq!(client.peer.frame(), (0x40, vec![0, 7]));
                    assert_eq!(client.peer.frame().0, 0xa2);
                    let mut admitted = ptr::null_mut();
                    assert_eq!(
                        rumqttc_client_acknowledge_token_tracked(
                            client.raw,
                            second,
                            &raw mut admitted,
                            ptr::null_mut()
                        ),
                        0
                    );
                    // Observer destruction cannot cancel an admitted ACK.
                    rumqttc_completion_destroy(admitted);
                    assert_eq!(client.peer.frame(), (0x40, vec![0, 8]));
                }
                assert_eq!(
                    rumqttc_client_try_acknowledge_token(
                        client.raw,
                        alias,
                        &raw mut operation,
                        ptr::null_mut()
                    ),
                    2
                );
                rumqttc_completion_destroy(completion);
                rumqttc_ack_token_release(first);
                rumqttc_ack_token_release(alias);
                rumqttc_ack_token_release(second);
            }
        }
    }
}

#[test]
fn dropping_observers_while_flush_is_pending_does_not_cancel_admission() {
    for shared in [false, true] {
        let mut client = Client::new(2, shared, true);
        let event = client.publish(1, false, 7);
        let token = take(event);
        unsafe {
            rumqttc_event_destroy(event);
            client.peer.flush.hold.store(true, Ordering::Release);
            let mut completion = ptr::null_mut();
            assert_eq!(
                rumqttc_client_acknowledge_token_tracked(
                    client.raw,
                    token,
                    &raw mut completion,
                    ptr::null_mut()
                ),
                0
            );
            client.peer.entered.recv_timeout(DEADLINE).unwrap();
            assert_eq!(
                rumqttc_completion_wait_timeout_ms(completion, 0, ptr::null_mut()),
                5
            );
            rumqttc_completion_destroy(completion);
            rumqttc_ack_token_release(token);
            client.peer.flush.hold.store(false, Ordering::Release);
            client.peer.flush.release.add_permits(1);
            assert_eq!(client.peer.frame(), (0x40, vec![0, 7]));
            // A subsequent operation's flush proves the abandoned observer did not stop progress.
            let event = client.publish(1, false, 8);
            let token = take(event);
            assert_eq!(
                rumqttc_client_acknowledge_token_tracked(
                    client.raw,
                    token,
                    &raw mut completion,
                    ptr::null_mut()
                ),
                0
            );
            assert_eq!(wait(completion), 0);
            assert_eq!(client.peer.frame(), (0x40, vec![0, 8]));
            rumqttc_event_destroy(event);
            rumqttc_completion_destroy(completion);
            rumqttc_ack_token_release(token);
        }
    }
}

#[test]
fn stale_and_foreign_tokens_do_not_retain_execution_or_acknowledge_new_flows() {
    for protocol in [1, 2] {
        for shared in [false, true] {
            let mut client = Client::new(protocol, shared, true);
            let event = client.publish(1, false, 7);
            let stale = take(event);
            unsafe { rumqttc_event_destroy(event) };
            let foreign = Client::new(protocol, shared, true);
            let mut operation = 99;
            unsafe {
                assert_ne!(
                    rumqttc_client_try_acknowledge_token(
                        foreign.raw,
                        stale,
                        &raw mut operation,
                        ptr::null_mut()
                    ),
                    0
                );
                assert_eq!(operation, 0);
                client.reconnect();
                assert_ne!(
                    rumqttc_client_try_acknowledge_token(
                        client.raw,
                        stale,
                        &raw mut operation,
                        ptr::null_mut()
                    ),
                    0
                );
                let event = client.publish(1, false, 7);
                let current = take(event);
                rumqttc_event_destroy(event);
                assert_eq!(
                    rumqttc_client_try_acknowledge_token(
                        client.raw,
                        current,
                        &raw mut operation,
                        ptr::null_mut()
                    ),
                    0
                );
                assert_eq!(client.peer.frame(), (0x40, vec![0, 7]));
                let event = client.publish(1, false, 8);
                let unconsumed = take(event);
                rumqttc_event_destroy(event);
                assert_eq!(
                    rumqttc_client_close_now_timeout_ms(client.raw, 3000, ptr::null_mut()),
                    0
                );
                assert_ne!(
                    rumqttc_client_try_acknowledge_token(
                        client.raw,
                        unconsumed,
                        &raw mut operation,
                        ptr::null_mut()
                    ),
                    0
                );
                client.destroy();
                let alias = retain(unconsumed);
                assert_ne!(
                    rumqttc_client_try_acknowledge_token(
                        foreign.raw,
                        alias,
                        &raw mut operation,
                        ptr::null_mut()
                    ),
                    0
                );
                for token in [stale, current, unconsumed, alias] {
                    rumqttc_ack_token_release(token);
                }
            }
        }
    }
}

#[test]
fn acknowledgement_racing_reconnect_never_enters_the_replacement_connection() {
    for (protocol, shared) in [(1, false), (1, true), (2, false), (2, true)] {
        let mut client = Client::new(protocol, shared, true);
        let event = client.publish(1, false, 7);
        let token = take(event);
        unsafe { rumqttc_event_destroy(event) };
        let barrier = Arc::new(Barrier::new(2));
        let worker = {
            let barrier = Arc::clone(&barrier);
            let raw = client.raw as usize;
            let token = token as usize;
            thread::spawn(move || {
                barrier.wait();
                let mut completion = ptr::null_mut();
                let status = unsafe {
                    rumqttc_client_acknowledge_token_tracked(
                        raw as *mut _,
                        token as *mut _,
                        &raw mut completion,
                        ptr::null_mut(),
                    )
                };
                (status, completion as usize)
            })
        };
        barrier.wait();
        client.reconnect();
        let (status, completion) = worker.join().unwrap();
        unsafe {
            if status == 0 {
                assert_ne!(wait(completion as *mut _), 5);
                rumqttc_completion_destroy(completion as *mut _);
            } else {
                assert_eq!(completion, 0);
            }
            let mut operation = 99;
            assert_ne!(
                rumqttc_client_try_acknowledge_token(
                    client.raw,
                    token,
                    &raw mut operation,
                    ptr::null_mut()
                ),
                0
            );
            assert_eq!(operation, 0);
            rumqttc_ack_token_release(token);
            // A different packet ID makes a leaked old ACK distinguishable on the new stream.
            let event = client.publish(1, false, 8);
            let current = take(event);
            rumqttc_event_destroy(event);
            let mut completion = ptr::null_mut();
            assert_eq!(
                rumqttc_client_acknowledge_token_tracked(
                    client.raw,
                    current,
                    &raw mut completion,
                    ptr::null_mut()
                ),
                0
            );
            assert_eq!(wait(completion), 0);
            assert_eq!(client.peer.frame(), (0x40, vec![0, 8]));
            assert!(client.peer.buffered.is_empty());
            assert!(client.peer.output.try_recv().is_err());
            rumqttc_completion_destroy(completion);
            rumqttc_ack_token_release(current);
        }
    }
}

#[test]
fn retained_and_retransmission_aliases_race_event_acknowledgement_and_close() {
    for (protocol, qos) in [(1, 1), (1, 2), (2, 1), (2, 2)] {
        for shared in [false, true] {
            for close in [false, true] {
                let mut client = Client::new(protocol, shared, true);
                let original = client.publish(qos, false, 7);
                let token = take(original);
                let alias = retain(token);
                unsafe { rumqttc_event_destroy(original) };
                let retransmission = client.publish(qos, true, 7);
                let barrier = Arc::new(Barrier::new(if close { 5 } else { 4 }));
                let mut workers = Vec::new();
                for source in [
                    AcknowledgementSource::Token(token),
                    AcknowledgementSource::Token(alias),
                    AcknowledgementSource::Event(retransmission),
                ] {
                    let (is_event, pointer) = match source {
                        AcknowledgementSource::Event(event) => (true, event as usize),
                        AcknowledgementSource::Token(token) => (false, token as usize),
                    };
                    let raw = client.raw as usize;
                    let barrier = Arc::clone(&barrier);
                    workers.push(thread::spawn(move || {
                        barrier.wait();
                        let mut completion = ptr::dangling_mut();
                        let status = unsafe {
                            if is_event {
                                rumqttc_client_acknowledge_tracked(
                                    raw as *mut _,
                                    pointer as *mut _,
                                    &raw mut completion,
                                    ptr::null_mut(),
                                )
                            } else {
                                rumqttc_client_acknowledge_token_tracked(
                                    raw as *mut _,
                                    pointer as *mut _,
                                    &raw mut completion,
                                    ptr::null_mut(),
                                )
                            }
                        };
                        assert!(status == 0 || completion.is_null());
                        (status, completion as usize)
                    }));
                }
                let closer = close.then(|| {
                    let barrier = Arc::clone(&barrier);
                    let raw = client.raw as usize;
                    thread::spawn(move || {
                        barrier.wait();
                        assert_eq!(
                            unsafe {
                                rumqttc_client_close_now_timeout_ms(
                                    raw as *mut _,
                                    3000,
                                    ptr::null_mut(),
                                )
                            },
                            0
                        );
                    })
                });
                barrier.wait();
                let results: Vec<_> = workers
                    .into_iter()
                    .map(|worker| worker.join().unwrap())
                    .collect();
                if let Some(closer) = closer {
                    closer.join().unwrap();
                }
                let admitted = results.iter().filter(|(status, _)| *status == 0).count();
                assert!(admitted <= 1);
                if !close {
                    assert_eq!(admitted, 1);
                    assert_eq!(
                        client.peer.frame(),
                        (if qos == 1 { 0x40 } else { 0x50 }, vec![0, 7])
                    );
                }
                unsafe {
                    for (status, completion) in results {
                        if status == 0 {
                            assert_ne!(wait(completion as *mut _), 5);
                            rumqttc_completion_destroy(completion as *mut _);
                        }
                    }
                    rumqttc_event_destroy(retransmission);
                    rumqttc_ack_token_release(token);
                    rumqttc_ack_token_release(alias);
                }
            }
        }
    }
}

#[test]
fn concurrent_extraction_and_event_ack_have_one_owner() {
    for protocol in [1, 2] {
        let mut client = Client::new(protocol, false, true);
        let event = client.publish(1, false, 7);
        let barrier = Arc::new(Barrier::new(4));
        let mut workers = Vec::new();
        for extract in [true, true, false] {
            let raw = client.raw as usize;
            let event = event as usize;
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                let mut token = ptr::null_mut();
                let mut operation = 0;
                barrier.wait();
                let status = unsafe {
                    if extract {
                        rumqttc_event_take_ack_token(
                            event as *mut _,
                            &raw mut token,
                            ptr::null_mut(),
                        )
                    } else {
                        rumqttc_client_try_acknowledge(
                            raw as *mut _,
                            event as *mut _,
                            &raw mut operation,
                            ptr::null_mut(),
                        )
                    }
                };
                (status, token as usize)
            }));
        }
        barrier.wait();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|(status, _)| *status == 0).count(), 1);
        unsafe {
            rumqttc_event_destroy(event);
            for (_, token) in results {
                if token != 0 {
                    let mut operation = 0;
                    assert_eq!(
                        rumqttc_client_try_acknowledge_token(
                            client.raw,
                            token as *mut _,
                            &raw mut operation,
                            ptr::null_mut()
                        ),
                        0
                    );
                    rumqttc_ack_token_release(token as *mut _);
                }
            }
        }
        assert_eq!(client.peer.frame(), (0x40, vec![0, 7]));
    }
}

#[test]
fn releasing_unconsumed_tokens_does_not_acknowledge() {
    let mut client = Client::new(2, false, true);
    let event = client.publish(1, false, 7);
    let token = take(event);
    unsafe {
        rumqttc_event_destroy(event);
        rumqttc_ack_token_release(token);
    }
    // A later ACK is a wire-order barrier: no ACK for packet 7 may precede it.
    let event = client.publish(1, false, 8);
    let token = take(event);
    unsafe {
        let mut operation = 0;
        assert_eq!(
            rumqttc_client_try_acknowledge_token(
                client.raw,
                token,
                &raw mut operation,
                ptr::null_mut()
            ),
            0
        );
        rumqttc_event_destroy(event);
        rumqttc_ack_token_release(token);
    }
    assert_eq!(client.peer.frame(), (0x40, vec![0, 8]));
}

#[path = "../pending_admission/tests.rs"]
mod pending_admission_tests;

#[path = "../first_connection/controlled_tests.rs"]
mod first_connection_tests;
