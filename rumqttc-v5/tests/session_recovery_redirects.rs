use bytes::BytesMut;
use rumqttc::mqttbytes::v5::{Connect, Packet};
use rumqttc::{
    ConnectAuth, ConnectionError, Event, EventLoop, MqttOptions, RedirectClientId,
    RedirectDecision, RedirectFailure, RedirectPolicy, RedirectSession, RedirectTargetProfile,
    SrvLookupError, SrvRecord, SrvResolver, Transport,
};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc;
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(3);
const ORIGIN: &str = "origin.example:1883";
const OWNER: &str = "_mqtt._tcp.redirect.example";
type Peers = mpsc::UnboundedReceiver<(String, DuplexStream)>;

fn options(policy: RedirectPolicy) -> (MqttOptions, Peers) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut options = MqttOptions::new("origin-client", "origin.example");
    options
        .set_clean_start(false)
        .set_session_expiry_interval(Some(300))
        .set_session_store_scope("origin-scope")
        .set_connect_timeout(DEADLINE)
        .set_redirect_policy(policy)
        .set_socket_connector(move |endpoint, _| {
            let (client, peer) = tokio::io::duplex(4096);
            tx.send((endpoint, peer)).unwrap();
            async move { Ok(client) }
        });
    (options, rx)
}

fn reuse_policy() -> RedirectPolicy {
    RedirectPolicy::new(NonZeroUsize::new(1).unwrap(), |context| {
        RedirectDecision::follow(
            RedirectTargetProfile::isolated(context.references[0].clone(), Transport::tcp())
                .unwrap()
                .client_id(RedirectClientId::Reuse)
                .session(RedirectSession::Reuse {
                    store_scope: context.store_scope.to_owned(),
                })
                .authentication(ConnectAuth::Username {
                    username: "target-user".to_owned(),
                }),
        )
    })
}

async fn frame(peer: &mut DuplexStream) -> BytesMut {
    let mut data = vec![peer.read_u8().await.unwrap()];
    let mut length = 0;
    for shift in [0, 7, 14, 21] {
        let byte = peer.read_u8().await.unwrap();
        data.push(byte);
        length |= usize::from(byte & 127) << shift;
        if byte < 128 {
            let start = data.len();
            data.resize(start + length, 0);
            peer.read_exact(&mut data[start..]).await.unwrap();
            return BytesMut::from(data.as_slice());
        }
    }
    panic!("invalid remaining length");
}

async fn handshake(peers: &mut Peers) -> (String, DuplexStream, Connect, ConnectAuth) {
    let (endpoint, mut peer) = peers.recv().await.unwrap();
    let Packet::Connect(connect, None, auth) =
        Packet::read(&mut frame(&mut peer).await, None).unwrap()
    else {
        panic!("expected CONNECT without Will");
    };
    (endpoint, peer, connect, auth)
}

async fn accept_redirect(eventloop: &mut EventLoop, peers: &mut Peers, reference: &str) {
    let (event, endpoint) = timeout(DEADLINE, async {
        tokio::join!(eventloop.poll(), async {
            let (endpoint, mut peer, _, _) = handshake(peers).await;
            // CONNACK: Use Another Server, with one Server Reference property.
            let len = u8::try_from(reference.len()).unwrap();
            assert!(len < 100);
            let mut response = vec![0x20, 6 + len, 0, 0x9c, 3 + len, 0x1c, 0, len];
            response.extend_from_slice(reference.as_bytes());
            peer.write_all(&response).await.unwrap();
            endpoint
        })
    })
    .await
    .unwrap();
    assert!(matches!(event.unwrap(), Event::Redirect(_)));
    assert_eq!(endpoint, ORIGIN);
}

async fn successful_handshake(peers: &mut Peers) -> (String, DuplexStream, Connect, ConnectAuth) {
    let (endpoint, mut peer, connect, auth) = handshake(peers).await;
    peer.write_all(&[0x20, 3, 0, 0, 0]).await.unwrap();
    (endpoint, peer, connect, auth)
}

fn srv_record() -> Vec<SrvRecord> {
    vec![SrvRecord {
        priority: 0,
        weight: 0,
        port: 2883,
        target: "selected.example.".to_owned(),
    }]
}

#[tokio::test]
async fn recovery_resolves_an_accepted_srv_redirect_before_sending_connect() {
    let lookups = Arc::new(AtomicUsize::new(0));
    let calls = lookups.clone();
    let (mut options, mut peers) = options(reuse_policy());
    options.set_srv_resolver(SrvResolver::new(move |owner| {
        assert_eq!(owner, format!("{OWNER}."));
        calls.fetch_add(1, Ordering::SeqCst);
        async { Ok(srv_record()) }
    }));
    let mut eventloop = EventLoop::new(options, 8);
    accept_redirect(&mut eventloop, &mut peers, OWNER).await;
    assert_eq!(lookups.load(Ordering::SeqCst), 0);
    assert_eq!(
        eventloop.options.broker().tcp_address(),
        Some(("origin.example", 1883))
    );

    let gate = eventloop.session_recovery_gate();
    let observation = gate.request().unwrap();
    eventloop.abandon_session_for_recovery().await.unwrap();
    let (result, (endpoint, _peer, connect, auth)) = timeout(DEADLINE, async {
        tokio::join!(
            eventloop.establish_session_for_recovery(|| true),
            successful_handshake(&mut peers)
        )
    })
    .await
    .unwrap();
    assert!(matches!(
        result.unwrap(),
        Event::Incoming(Packet::ConnAck(_))
    ));
    assert_eq!(endpoint, "selected.example:2883");
    assert_eq!(lookups.load(Ordering::SeqCst), 1);
    assert_eq!(connect.client_id, "origin-client");
    assert!(connect.clean_start);
    assert_eq!(
        connect.properties.unwrap().session_expiry_interval,
        Some(300)
    );
    assert_eq!(
        auth,
        ConnectAuth::Username {
            username: "target-user".to_owned()
        }
    );
    assert!(!eventloop.options.clean_start());
    assert!(observation.snapshot().fresh_established);
    assert!(peers.try_recv().is_err());
}

#[tokio::test]
async fn recovery_srv_lookup_failure_does_not_dial_the_origin() {
    let (mut options, mut peers) = options(reuse_policy());
    options.set_srv_resolver(SrvResolver::new(|_| async {
        Err(SrvLookupError::custom(std::io::Error::other(
            "lookup failed",
        )))
    }));
    let mut eventloop = EventLoop::new(options, 8);
    accept_redirect(&mut eventloop, &mut peers, OWNER).await;
    let observation = eventloop.session_recovery_gate().request().unwrap();
    eventloop.abandon_session_for_recovery().await.unwrap();

    let error = timeout(DEADLINE, eventloop.establish_session_for_recovery(|| true))
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(error, ConnectionError::Redirect(ref redirect)
        if matches!(redirect.failure, RedirectFailure::SrvLookup { .. })));
    assert!(peers.try_recv().is_err());
    let snapshot = observation.snapshot();
    assert!(snapshot.abandonment_committed && snapshot.checkpoint_cleared);
    assert!(!snapshot.fresh_established);
    assert!(!eventloop.options.clean_start());
}

#[tokio::test]
async fn recovery_rechecks_shutdown_after_srv_preparation() {
    let running = Arc::new(AtomicBool::new(true));
    let shutdown = running.clone();
    let (mut options, mut peers) = options(reuse_policy());
    options.set_srv_resolver(SrvResolver::new(move |_| {
        let shutdown = shutdown.clone();
        async move {
            shutdown.store(false, Ordering::SeqCst);
            Ok(srv_record())
        }
    }));
    let mut eventloop = EventLoop::new(options, 8);
    accept_redirect(&mut eventloop, &mut peers, OWNER).await;
    eventloop.session_recovery_gate().request().unwrap();
    eventloop.abandon_session_for_recovery().await.unwrap();

    assert!(matches!(
        timeout(
            DEADLINE,
            eventloop.establish_session_for_recovery(|| running.load(Ordering::SeqCst))
        )
        .await
        .unwrap(),
        Err(ConnectionError::SessionRecoveryInvalid)
    ));
    assert!(peers.try_recv().is_err());
    assert!(!eventloop.options.clean_start());
}

#[tokio::test]
async fn recovery_before_redirect_handshake_uses_the_accepted_identity() {
    for (client_id, scope) in [
        ("target-client", "origin-scope"),
        ("origin-client", "target-scope"),
        ("target-client", "target-scope"),
    ] {
        for srv in [false, true] {
            let policy = RedirectPolicy::new(NonZeroUsize::new(1).unwrap(), move |context| {
                let client_id = if client_id == context.client_id {
                    RedirectClientId::Reuse
                } else {
                    RedirectClientId::Replace(client_id.to_owned())
                };
                RedirectDecision::follow(
                    RedirectTargetProfile::isolated(
                        context.references[0].clone(),
                        Transport::tcp(),
                    )
                    .unwrap()
                    .client_id(client_id)
                    .session(RedirectSession::Reuse {
                        store_scope: scope.to_owned(),
                    }),
                )
            });
            let (mut options, mut peers) = options(policy);
            options.set_srv_resolver(SrvResolver::new(|_| async { Ok(srv_record()) }));
            let mut eventloop = EventLoop::new(options, 8);
            let reference = if srv { OWNER } else { "temporary.example" };
            accept_redirect(&mut eventloop, &mut peers, reference).await;
            assert_eq!(eventloop.options.client_id(), client_id);
            assert_eq!(eventloop.options.session_store_scope(), scope);
            // Admit recovery at the redirect event boundary, before polling the target.
            let gate = eventloop.session_recovery_gate();
            let observation = gate.request().unwrap();
            eventloop.abandon_session_for_recovery().await.unwrap();
            let (result, (endpoint, _peer, connect, _)) = timeout(DEADLINE, async {
                tokio::join!(
                    eventloop.establish_session_for_recovery(|| true),
                    successful_handshake(&mut peers)
                )
            })
            .await
            .unwrap();
            assert!(matches!(
                result.unwrap(),
                Event::Incoming(Packet::ConnAck(_))
            ));
            assert_eq!(
                endpoint,
                if srv {
                    "selected.example:2883"
                } else {
                    "temporary.example:1883"
                }
            );
            assert_eq!(connect.client_id, client_id);
            assert!(connect.clean_start);
            assert_eq!(
                connect.properties.unwrap().session_expiry_interval,
                Some(300)
            );
            assert_eq!(eventloop.options.session_store_scope(), scope);
            assert!(!eventloop.options.clean_start());
            gate.complete();
            let snapshot = observation.snapshot();
            assert!(
                snapshot.abandonment_committed
                    && snapshot.checkpoint_cleared
                    && snapshot.fresh_established
            );
            assert!(!gate.can_request());
            assert!(peers.try_recv().is_err());
        }
    }
}

#[tokio::test]
async fn recovery_after_temporary_redirect_restoration_uses_the_origin_identity() {
    for (client_id, scope) in [
        ("target-client", "origin-scope"),
        ("origin-client", "target-scope"),
        ("target-client", "target-scope"),
    ] {
        for public_cleanup in [false, true] {
            let policy = RedirectPolicy::new(NonZeroUsize::new(1).unwrap(), move |context| {
                RedirectDecision::follow(
                    RedirectTargetProfile::isolated(
                        context.references[0].clone(),
                        Transport::tcp(),
                    )
                    .unwrap()
                    .client_id(RedirectClientId::Replace(client_id.to_owned()))
                    .session(RedirectSession::Reuse {
                        store_scope: scope.to_owned(),
                    }),
                )
            });
            let (options, mut peers) = options(policy);
            let mut eventloop = EventLoop::new(options, 8);
            accept_redirect(&mut eventloop, &mut peers, "temporary.example").await;
            let (result, (endpoint, peer, connect, _)) = timeout(DEADLINE, async {
                tokio::join!(eventloop.poll(), successful_handshake(&mut peers))
            })
            .await
            .unwrap();
            assert!(matches!(
                result.unwrap(),
                Event::Incoming(Packet::ConnAck(_))
            ));
            assert_eq!(endpoint, "temporary.example:1883");
            assert_eq!(connect.client_id, client_id);
            assert_eq!(eventloop.options.session_store_scope(), scope);
            let gate = eventloop.session_recovery_gate();
            assert!(!gate.can_request());

            if public_cleanup {
                eventloop.clean();
            } else {
                drop(peer);
                timeout(DEADLINE, async { while eventloop.poll().await.is_ok() {} })
                    .await
                    .unwrap();
            }
            assert_eq!(eventloop.options.client_id(), "origin-client");
            assert_eq!(eventloop.options.session_store_scope(), "origin-scope");
            assert!(gate.can_request());
            let observation = gate.request().unwrap();
            eventloop.abandon_session_for_recovery().await.unwrap();
            let (result, (endpoint, _peer, connect, _)) = timeout(DEADLINE, async {
                tokio::join!(
                    eventloop.establish_session_for_recovery(|| true),
                    successful_handshake(&mut peers)
                )
            })
            .await
            .unwrap();
            assert!(matches!(
                result.unwrap(),
                Event::Incoming(Packet::ConnAck(_))
            ));
            assert_eq!(endpoint, ORIGIN);
            assert_eq!(connect.client_id, "origin-client");
            assert!(connect.clean_start);
            assert_eq!(
                connect.properties.unwrap().session_expiry_interval,
                Some(300)
            );
            assert!(!eventloop.options.clean_start());
            assert!(observation.snapshot().fresh_established);
        }
    }
}
