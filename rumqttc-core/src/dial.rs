use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::net::{TcpStream, lookup_host};
use tokio::time::{Instant, sleep};

use crate::{NetworkOptions, connect_socket_addr};

const CONNECTION_ATTEMPT_DELAY: Duration = Duration::from_millis(100);

/// Default TCP socket connection logic used by both MQTT clients.
///
/// Resolves the host using the system resolver, then alternates IPv4 and IPv6
/// candidates, preserving the resolver's first address family and the order
/// within each family. Connection attempts start 100 ms apart and may overlap.
/// The first successful TCP connection wins; all other attempts are dropped
/// before returning, including when this future is cancelled.
///
/// Each socket receives the supplied [`NetworkOptions`]. A fixed nonzero local
/// bind port disables racing and preserves sequential resolver order to avoid
/// overlapping binds. Custom connectors can delegate to this helper to retain
/// the clients' default dialing policy.
///
/// This is a Happy Eyeballs style TCP race, not a complete RFC 8305 resolver:
/// it waits for the system lookup to finish before connecting. TLS, proxy and
/// MQTT handshakes run only on the winning stream. The caller must enforce an
/// overall deadline; the MQTT event loops include this helper in their connect
/// deadline.
///
/// # Errors
///
/// Returns a DNS lookup error, or the last completed socket construction,
/// configuration or connection error if all candidates fail. An empty address
/// list returns [`io::ErrorKind::InvalidInput`].
pub async fn default_socket_connect(
    host: String,
    network_options: NetworkOptions,
) -> io::Result<TcpStream> {
    let addrs = lookup_host(host).await?.collect();
    connect_resolved_addrs(addrs, &network_options, |addr| {
        connect_socket_addr(addr, network_options.clone())
    })
    .await
}

fn interleave_addresses(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let Some(first) = addrs.first() else {
        return addrs;
    };
    let prefer_ipv6 = first.is_ipv6();
    if addrs.iter().all(|addr| addr.is_ipv6() == prefer_ipv6) {
        return addrs;
    }

    let mut interleaved = Vec::with_capacity(addrs.len());
    let (preferred, alternate): (Vec<_>, Vec<_>) = addrs
        .into_iter()
        .partition(|addr| addr.is_ipv6() == prefer_ipv6);
    let mut preferred = preferred.into_iter();
    let mut alternate = alternate.into_iter();
    loop {
        match (preferred.next(), alternate.next()) {
            (None, None) => break,
            (first, second) => interleaved.extend(first.into_iter().chain(second)),
        }
    }
    interleaved
}

// Keep pending connections in this future rather than spawning tasks. Returning
// or cancelling drops their sockets synchronously, before protocol negotiation.
async fn connect_resolved_addrs<T, F, Fut>(
    addrs: Vec<SocketAddr>,
    network_options: &NetworkOptions,
    mut connect: F,
) -> io::Result<T>
where
    F: FnMut(SocketAddr) -> Fut,
    Fut: Future<Output = io::Result<T>>,
{
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve to any address",
        ));
    }
    let mut last_err = None;
    if network_options
        .bind_addr()
        .is_some_and(|addr| addr.port() != 0)
    {
        for addr in addrs {
            match connect(addr).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last_err = Some(error),
            }
        }
        return Err(last_err.expect("all connection attempts failed"));
    }

    let mut addrs = interleave_addresses(addrs).into_iter();
    let first = addrs.next().expect("nonempty address list");
    let mut pending = FuturesUnordered::new();
    pending.push(connect(first));
    let next_attempt = sleep(CONNECTION_ATTEMPT_DELAY);
    tokio::pin!(next_attempt);

    while !pending.is_empty() || addrs.len() != 0 {
        tokio::select! {
            // Prefer an established connection to starting another candidate.
            biased;
            Some(result) = pending.next(), if !pending.is_empty() => {
                match result {
                    Ok(stream) => return Ok(stream),
                    Err(error) => last_err = Some(error),
                }
            }
            () = &mut next_attempt, if addrs.len() != 0 => {
                pending.push(connect(addrs.next().expect("remaining candidate")));
                next_attempt.as_mut().reset(Instant::now() + CONNECTION_ATTEMPT_DELAY);
            }
        }
    }
    Err(last_err.expect("all connection attempts failed"))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::future::pending;

    use super::*;

    fn v4(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn v6(port: u16) -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port))
    }

    struct Cancelled<'a>(&'a Cell<usize>);

    impl Drop for Cancelled<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn interleaving_preserves_family_preference_and_order_with_uneven_lists() {
        for (input, expected) in [
            (vec![], vec![]),
            (vec![v4(1)], vec![v4(1)]),
            (vec![v6(1), v6(2)], vec![v6(1), v6(2)]),
            (vec![v4(1), v4(2)], vec![v4(1), v4(2)]),
            (
                vec![v6(1), v6(2), v6(3), v4(4)],
                vec![v6(1), v4(4), v6(2), v6(3)],
            ),
            (
                vec![v4(1), v6(2), v6(3), v6(4)],
                vec![v4(1), v6(2), v6(3), v6(4)],
            ),
            (
                vec![v4(1), v4(2), v6(3), v6(4)],
                vec![v4(1), v6(3), v4(2), v6(4)],
            ),
            (
                vec![v6(1), v4(2), v4(3), v6(4), v4(5)],
                vec![v6(1), v4(2), v6(4), v4(3), v4(5)],
            ),
        ] {
            assert_eq!(interleave_addresses(input), expected);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_preferred_family_allows_alternate_to_win_and_cancels_loser() {
        // Exercise both resolver preferences without requiring routable IPv6.
        for (preferred, alternate) in [(v6(1), v4(4)), (v4(1), v6(4))] {
            let options = NetworkOptions::new();
            let started = RefCell::new(Vec::new());
            let cancelled = Cell::new(0);
            let begin = Instant::now();
            let winner = connect_resolved_addrs(
                vec![preferred, SocketAddr::new(preferred.ip(), 2), alternate],
                &options,
                |addr| {
                    let started = &started;
                    let cancelled = &cancelled;
                    async move {
                        started.borrow_mut().push((addr, Instant::now()));
                        if addr == alternate {
                            Ok(addr)
                        } else {
                            let _cancelled = Cancelled(cancelled);
                            pending::<io::Result<SocketAddr>>().await
                        }
                    }
                },
            )
            .await
            .unwrap();
            assert_eq!(winner, alternate);
            assert_eq!(
                *started.borrow(),
                vec![
                    (preferred, begin),
                    (alternate, begin + CONNECTION_ATTEMPT_DELAY)
                ]
            );
            assert_eq!(cancelled.get(), 1, "loser must be dropped before returning");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ephemeral_bind_port_allows_overlapping_same_family_attempts() {
        let mut options = NetworkOptions::new();
        options.set_bind_addr(v4(0));
        let cancelled = Cell::new(0);
        let begin = Instant::now();
        let winner = connect_resolved_addrs(vec![v4(1), v4(2)], &options, |addr| {
            let cancelled = &cancelled;
            async move {
                if addr == v4(1) {
                    let _cancelled = Cancelled(cancelled);
                    pending::<io::Result<SocketAddr>>().await
                } else {
                    Ok(addr)
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(winner, v4(2));
        assert_eq!(Instant::now() - begin, CONNECTION_ATTEMPT_DELAY);
        assert_eq!(cancelled.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn first_success_does_not_start_later_candidates() {
        let started = RefCell::new(Vec::new());
        let begin = Instant::now();
        let winner = connect_resolved_addrs(vec![v6(1), v4(2)], &NetworkOptions::new(), |addr| {
            started.borrow_mut().push(addr);
            std::future::ready(Ok(addr))
        })
        .await
        .unwrap();
        assert_eq!(winner, v6(1));
        assert_eq!(*started.borrow(), vec![v6(1)]);
        assert_eq!(Instant::now(), begin);
    }

    #[tokio::test(start_paused = true)]
    async fn same_family_failures_try_later_addresses_at_the_stagger_interval() {
        let started = RefCell::new(Vec::new());
        let begin = Instant::now();
        let winner = connect_resolved_addrs(vec![v4(1), v4(2)], &NetworkOptions::new(), |addr| {
            started.borrow_mut().push((addr, Instant::now()));
            std::future::ready(if addr == v4(1) {
                Err(io::Error::from(io::ErrorKind::ConnectionRefused))
            } else {
                Ok(addr)
            })
        })
        .await
        .unwrap();
        assert_eq!(winner, v4(2));
        assert_eq!(
            *started.borrow(),
            vec![(v4(1), begin), (v4(2), begin + CONNECTION_ATTEMPT_DELAY)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn all_failed_candidates_return_the_last_completed_error() {
        let error = connect_resolved_addrs(
            vec![v6(1), v4(2)],
            &NetworkOptions::new(),
            |addr| async move {
                if addr == v6(1) {
                    sleep(CONNECTION_ATTEMPT_DELAY * 3).await;
                    Err::<(), _>(io::Error::from(io::ErrorKind::ConnectionRefused))
                } else {
                    Err(io::Error::from(io::ErrorKind::NetworkUnreachable))
                }
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
    }

    #[tokio::test]
    async fn empty_candidates_return_invalid_input_without_connecting() {
        let called = Cell::new(false);
        let error = connect_resolved_addrs(Vec::new(), &NetworkOptions::new(), |_| {
            called.set(true);
            std::future::ready(Ok::<(), io::Error>(()))
        })
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "could not resolve to any address");
        assert!(!called.get());
    }

    #[tokio::test(start_paused = true)]
    async fn outer_timeout_drops_every_active_attempt_and_leaves_later_candidates_unstarted() {
        let started = RefCell::new(Vec::new());
        let cancelled = Cell::new(0);
        let options = NetworkOptions::new();
        let connect = connect_resolved_addrs(vec![v6(1), v6(2), v4(3), v4(4)], &options, |addr| {
            let started = &started;
            let cancelled = &cancelled;
            async move {
                started.borrow_mut().push(addr);
                let _cancelled = Cancelled(cancelled);
                pending::<io::Result<()>>().await
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(250), connect)
                .await
                .is_err()
        );
        assert_eq!(*started.borrow(), vec![v6(1), v4(3), v6(2)]);
        assert_eq!(cancelled.get(), 3);
        sleep(Duration::from_secs(1)).await;
        assert_eq!(started.borrow().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn fixed_bind_port_keeps_stalled_candidate_alive_without_overlapping_attempts() {
        let mut options = NetworkOptions::new();
        options.set_bind_addr(v4(12345));
        let started = RefCell::new(Vec::new());
        let cancelled = Cell::new(0);
        let connect = connect_resolved_addrs(vec![v4(1), v4(2)], &options, |addr| {
            let started = &started;
            let cancelled = &cancelled;
            async move {
                started.borrow_mut().push(addr);
                let _cancelled = Cancelled(cancelled);
                pending::<io::Result<()>>().await
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_secs(1), connect)
                .await
                .is_err()
        );
        assert_eq!(*started.borrow(), vec![v4(1)]);
        assert_eq!(cancelled.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn fixed_bind_port_retries_sequentially_in_resolver_order() {
        let mut options = NetworkOptions::new();
        options.set_bind_addr(v4(12345));
        let started = RefCell::new(Vec::new());
        let winner = connect_resolved_addrs(vec![v4(1), v4(2), v6(3)], &options, |addr| {
            let started = &started;
            async move {
                started.borrow_mut().push(addr);
                sleep(CONNECTION_ATTEMPT_DELAY * 2).await;
                if addr == v4(1) {
                    Err(io::Error::from(io::ErrorKind::ConnectionRefused))
                } else {
                    Ok(addr)
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(winner, v4(2));
        assert_eq!(*started.borrow(), vec![v4(1), v4(2)]);
    }

    #[tokio::test]
    async fn refused_socket_allows_later_resolved_address_to_connect() {
        let listener = tokio::net::TcpListener::bind(v4(0)).await.unwrap();
        let good = listener.local_addr().unwrap();
        let unused = tokio::net::TcpListener::bind(v4(0)).await.unwrap();
        let bad = unused.local_addr().unwrap();
        drop(unused);
        let options = NetworkOptions::new();
        let stream = connect_resolved_addrs(vec![bad, good], &options, |addr| {
            connect_socket_addr(addr, options.clone())
        })
        .await
        .unwrap();
        assert_eq!(stream.peer_addr().unwrap(), good);
    }

    #[tokio::test]
    async fn fixed_bind_port_socket_retry_preserves_source_address() {
        let listener = tokio::net::TcpListener::bind(v4(0)).await.unwrap();
        let good = listener.local_addr().unwrap();
        let reserved = tokio::net::TcpListener::bind(v4(0)).await.unwrap();
        let source = reserved.local_addr().unwrap();
        drop(reserved);
        let mut options = NetworkOptions::new();
        options.set_bind_addr(source);
        // Fail before binding. A refused connection can retain a fixed source
        // port briefly on Windows, so use an incompatible address family.
        let stream = connect_resolved_addrs(vec![v6(9), good], &options, |addr| {
            connect_socket_addr(addr, options.clone())
        })
        .await
        .unwrap();
        assert_eq!(stream.local_addr().unwrap(), source);
        assert_eq!(stream.peer_addr().unwrap(), good);
        let (_, peer) = listener.accept().await.unwrap();
        assert_eq!(peer, source);
    }
}
