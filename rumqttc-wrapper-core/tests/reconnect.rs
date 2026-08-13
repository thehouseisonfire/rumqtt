use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use rumqttc_wrapper_core::{ClientConfig, Command, NativeClient, WrapperEvent};

fn read_frame(stream: &mut TcpStream) -> Option<u8> {
    let mut header = [0];
    stream.read_exact(&mut header).ok()?;
    let mut remaining = 0_usize;
    let mut multiplier = 1_usize;
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).ok()?;
        remaining += usize::from(byte[0] & 0x7f) * multiplier;
        if byte[0] & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0; remaining];
    stream.read_exact(&mut body).ok()?;
    Some(header[0])
}

#[test]
fn recoverable_poll_error_is_reported_and_reconnects() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut first, _) = listener.accept().unwrap();
        assert_eq!(read_frame(&mut first).unwrap() >> 4, 1);
        first.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
        drop(first);

        let (mut second, _) = listener.accept().unwrap();
        assert_eq!(read_frame(&mut second).unwrap() >> 4, 1);
        second.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
        while let Some(header) = read_frame(&mut second) {
            if header >> 4 == 14 {
                break;
            }
        }
    });

    let mut native = NativeClient::start(ClientConfig::v4("reconnect", "127.0.0.1", port)).unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut connected = 0;
    let mut disconnected = false;
    while Instant::now() < deadline && connected < 2 {
        match events.recv_timeout(Duration::from_millis(200)).unwrap() {
            Some(WrapperEvent::Connected { .. }) => connected += 1,
            Some(WrapperEvent::Disconnected { .. }) => disconnected = true,
            Some(_) | None => {}
        }
    }
    assert_eq!(connected, 2);
    assert!(disconnected);

    handle
        .try_admit(Command::GracefulDisconnect {
            timeout: Some(Duration::from_secs(1)),
        })
        .unwrap();
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}
