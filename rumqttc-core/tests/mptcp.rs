#![cfg(target_os = "linux")]

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::os::fd::AsRawFd;
use std::time::Duration;

use rumqttc_core::{NetworkOptions, default_socket_connect};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

// Linux UAPI `TCP_IS_MPTCP`; libc does not currently expose this socket option.
const TCP_IS_MPTCP: libc::c_int = 43;
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

fn is_mptcp_unavailable(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|error_code| {
        matches!(
            error_code,
            libc::EINVAL | libc::EPROTONOSUPPORT | libc::ENOPROTOOPT
        )
    })
}

fn mptcp_listener() -> io::Result<Option<TcpListener>> {
    let socket = match Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::MPTCP)) {
        Ok(socket) => socket,
        Err(error) if is_mptcp_unavailable(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::LOCALHOST, 0)).into())?;
    socket.listen(1)?;

    let listener: std::net::TcpListener = socket.into();
    TcpListener::from_std(listener).map(Some)
}

fn is_mptcp_inspection_unavailable(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|error_code| matches!(error_code, libc::EOPNOTSUPP | libc::ENOPROTOOPT))
}

/// Returns `None` when the kernel supports MPTCP sockets but predates the
/// `TCP_IS_MPTCP` inspection option.
fn is_mptcp(stream: &TcpStream) -> io::Result<Option<bool>> {
    let mut value: libc::c_int = 0;
    let mut value_len = libc::socklen_t::try_from(size_of::<libc::c_int>())
        .map_err(|_| io::Error::other("TCP_IS_MPTCP value length does not fit socklen_t"))?;

    // SAFETY: `stream` owns a valid socket descriptor for the duration of the call. The value and
    // length pointers refer to writable, correctly sized storage for the `TCP_IS_MPTCP` integer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::IPPROTO_TCP,
            TCP_IS_MPTCP,
            (&raw mut value).cast(),
            &raw mut value_len,
        )
    };
    if result == -1 {
        let error = io::Error::last_os_error();
        if is_mptcp_inspection_unavailable(&error) {
            return Ok(None);
        }
        return Err(error);
    }

    Ok(Some(value == 1))
}

fn assert_mptcp_when_inspectable(stream: &TcpStream, endpoint: &str) -> io::Result<()> {
    match is_mptcp(stream)? {
        Some(true) => Ok(()),
        Some(false) => Err(io::Error::other(format!(
            "{endpoint} connection must use MPTCP"
        ))),
        None => {
            eprintln!(
                "skipping {endpoint} MPTCP negotiation assertion: TCP_IS_MPTCP is unavailable"
            );
            Ok(())
        }
    }
}

#[test]
fn unsupported_tcp_is_mptcp_errors_disable_only_inspection() {
    for error_code in [libc::EOPNOTSUPP, libc::ENOPROTOOPT] {
        assert!(is_mptcp_inspection_unavailable(
            &io::Error::from_raw_os_error(error_code)
        ));
    }

    assert!(!is_mptcp_inspection_unavailable(
        &io::Error::from_raw_os_error(libc::EBADF)
    ));
}

#[tokio::test]
async fn default_dialer_negotiates_mptcp_and_transfers_data() -> io::Result<()> {
    let Some(listener) = mptcp_listener()? else {
        eprintln!("skipping MPTCP end-to-end test: MPTCP is unavailable or disabled");
        return Ok(());
    };
    let listener_addr = listener.local_addr()?;

    let server = tokio::spawn(async move {
        let (mut stream, _) = timeout(TEST_TIMEOUT, listener.accept())
            .await
            .map_err(io::Error::other)??;
        assert_mptcp_when_inspectable(&stream, "accepted")?;

        let mut request = [0_u8; 5];
        timeout(TEST_TIMEOUT, stream.read_exact(&mut request))
            .await
            .map_err(io::Error::other)??;
        assert_eq!(&request, b"hello");

        timeout(TEST_TIMEOUT, stream.write_all(b"world"))
            .await
            .map_err(io::Error::other)??;
        io::Result::Ok(())
    });

    let mut network_options = NetworkOptions::new();
    network_options.set_mptcp(true);
    let mut stream = timeout(
        TEST_TIMEOUT,
        default_socket_connect(listener_addr.to_string(), network_options),
    )
    .await
    .map_err(io::Error::other)??;
    assert_mptcp_when_inspectable(&stream, "client")?;

    timeout(TEST_TIMEOUT, stream.write_all(b"hello"))
        .await
        .map_err(io::Error::other)??;
    let mut response = [0_u8; 5];
    timeout(TEST_TIMEOUT, stream.read_exact(&mut response))
        .await
        .map_err(io::Error::other)??;
    assert_eq!(&response, b"world");

    server.await.map_err(io::Error::other)??;
    Ok(())
}
