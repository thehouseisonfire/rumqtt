use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rumqttc::{
    AsyncAuthChallenge, AsyncAuthContext, AsyncAuthenticator, AuthAction, AuthContext, AuthError,
    AuthFailureReason, AuthFuture, AuthProperties, Authenticator, ConnectionError, Event,
    EventLoop, MqttOptions, Packet, PersistedSession, SessionStore, SessionStoreError,
    SessionStoreKey,
};
use tokio::io::AsyncWriteExt;

// This is intentionally exhaustive: adding variants breaks pre-existing Rust consumers.
fn legacy_failure_name(reason: &AuthFailureReason) -> &'static str {
    match reason {
        AuthFailureReason::BrokerDisconnected(_) => "broker disconnected",
        AuthFailureReason::SessionReset => "session reset",
        AuthFailureReason::Redirected => "redirected",
        AuthFailureReason::ProtocolError => "protocol error",
        AuthFailureReason::AuthenticationFailed(_) => "authentication failed",
        AuthFailureReason::ConnectionClosed => "connection closed",
        AuthFailureReason::OverlappingReauth => "overlapping reauthentication",
        AuthFailureReason::MissingAuthenticationMethod => "missing method",
        AuthFailureReason::NoticeDropped => "notice dropped",
    }
}

#[test]
fn original_authentication_failure_matches_remain_exhaustive() {
    assert_eq!(
        legacy_failure_name(&AuthFailureReason::ConnectionClosed),
        "connection closed"
    );
}

type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SessionStoreError>> + Send + 'a>>;

#[derive(Debug)]
struct SlowClear {
    deadline: Arc<Mutex<Option<tokio::time::Instant>>>,
    clears: Arc<AtomicUsize>,
}

impl SessionStore for SlowClear {
    fn load<'a>(&'a self, _: &'a SessionStoreKey) -> StoreFuture<'a, Option<PersistedSession>> {
        Box::pin(async { Ok(None) })
    }

    fn save<'a>(&'a self, _: &'a SessionStoreKey, _: &'a PersistedSession) -> StoreFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn clear<'a>(&'a self, _: &'a SessionStoreKey) -> StoreFuture<'a, ()> {
        let deadline = self.deadline.lock().unwrap().expect("connector has run");
        Box::pin(async move {
            tokio::time::sleep_until(deadline + Duration::from_secs(1)).await;
            self.clears.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
    }
}

#[derive(Debug, Default)]
struct Authority {
    successes: AtomicUsize,
    failures: Mutex<Vec<AuthError>>,
}

impl AsyncAuthenticator for Authority {
    fn respond(&self, _: AsyncAuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
        if matches!(challenge, AsyncAuthChallenge::Success { .. }) {
            self.successes.fetch_add(1, Ordering::Relaxed);
        }
        Box::pin(async { Ok(AuthAction::Complete) })
    }

    fn failure(&self, _: AsyncAuthContext, error: AuthError) {
        self.failures.lock().unwrap().push(error);
    }
}

impl Authenticator for Authority {
    fn start(&mut self, _: AuthContext<'_>) -> Result<Option<AuthProperties>, AuthError> {
        Ok(None)
    }

    fn continue_auth(
        &mut self,
        _: AuthContext<'_>,
        _: Option<AuthProperties>,
    ) -> Result<AuthAction, AuthError> {
        panic!("no continuation in this exchange")
    }

    fn success(&mut self, _: AuthContext<'_>, _: Option<AuthProperties>) -> Result<(), AuthError> {
        self.successes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn failure(&mut self, _: AuthContext<'_>, error: AuthError) {
        self.failures.lock().unwrap().push(error);
    }
}

#[derive(Clone, Copy, Debug)]
enum Authentication {
    None,
    Raw,
    Synchronous,
    AsynchronousWithoutMethod,
    Asynchronous,
}

async fn connect_after_slow_store(authentication: Authentication) {
    let (socket, mut peer) = tokio::io::duplex(4096);
    let socket = Arc::new(Mutex::new(Some(socket)));
    let deadline = Arc::new(Mutex::new(None));
    let connector_deadline = deadline.clone();
    let clears = Arc::new(AtomicUsize::new(0));
    let authority = Arc::new(Authority::default());
    let synchronous = Arc::new(Mutex::new(Authority::default()));
    let mut options = MqttOptions::new("auth-store", "localhost");
    options
        .set_connect_timeout(Duration::from_secs(1))
        .set_session_store(SlowClear {
            deadline: deadline.clone(),
            clears: clears.clone(),
        })
        .set_socket_connector(move |_, network_options| {
            *connector_deadline.lock().unwrap() =
                network_options.connection_deadline().map(Into::into);
            let socket = socket.lock().unwrap().take().unwrap();
            async move { Ok::<_, std::io::Error>(socket) }
        });
    let has_method = matches!(
        authentication,
        Authentication::Raw | Authentication::Synchronous | Authentication::Asynchronous
    );
    if has_method {
        options.set_authentication_method(Some("test".into()));
        peer.write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
            .await
            .unwrap();
    } else {
        peer.write_all(b"\x20\x03\x00\x00\x00").await.unwrap();
    }
    match authentication {
        Authentication::Synchronous => {
            options.set_authenticator(synchronous.clone());
        }
        Authentication::Asynchronous | Authentication::AsynchronousWithoutMethod => {
            options.set_async_authenticator(authority.clone());
        }
        Authentication::None | Authentication::Raw => {}
    }
    let mut eventloop = EventLoop::new(options, 1);
    if matches!(authentication, Authentication::Asynchronous) {
        assert!(matches!(
            eventloop.poll().await,
            Err(ConnectionError::Timeout(_))
        ));
        assert!(!eventloop.diagnostics().connected);
        assert_eq!(authority.failures.lock().unwrap().len(), 1);
        assert!(!eventloop.state.events.iter().any(|event| matches!(
            event,
            Event::Incoming(Packet::ConnAck(_)) | Event::Auth(rumqttc::AuthEvent::Succeeded { .. })
        )));
    } else {
        loop {
            match eventloop.poll().await.unwrap() {
                Event::Incoming(Packet::ConnAck(_)) => break,
                Event::Auth(_) => {}
                event => panic!("unexpected connection event: {event:?}"),
            }
        }
        assert!(eventloop.diagnostics().connected);
        assert!(authority.failures.lock().unwrap().is_empty());
    }
    assert_eq!(authority.successes.load(Ordering::Relaxed), 0);
    assert_eq!(
        synchronous
            .lock()
            .unwrap()
            .successes
            .load(Ordering::Relaxed),
        usize::from(matches!(authentication, Authentication::Synchronous))
    );
    assert_eq!(clears.load(Ordering::Relaxed), 1);
    assert!(tokio::time::Instant::now() > deadline.lock().unwrap().unwrap());
}

#[tokio::test(start_paused = true)]
async fn slow_session_reconciliation_does_not_timeout_without_an_async_exchange() {
    for authentication in [
        Authentication::None,
        Authentication::Raw,
        Authentication::Synchronous,
        Authentication::AsynchronousWithoutMethod,
    ] {
        connect_after_slow_store(authentication).await;
    }
}

#[tokio::test(start_paused = true)]
async fn slow_session_reconciliation_cannot_extend_async_authentication_deadline() {
    connect_after_slow_store(Authentication::Asynchronous).await;
}

#[tokio::test(start_paused = true)]
async fn ordinary_connections_still_timeout_while_waiting_for_connack() {
    let (socket, _peer) = tokio::io::duplex(4096);
    let socket = Arc::new(Mutex::new(Some(socket)));
    let mut options = MqttOptions::new("timeout", "localhost");
    options
        .set_connect_timeout(Duration::from_secs(1))
        .set_socket_connector(move |_, _| {
            let socket = socket.lock().unwrap().take().unwrap();
            async move { Ok::<_, std::io::Error>(socket) }
        });
    let mut eventloop = EventLoop::new(options, 1);
    assert!(matches!(
        eventloop.poll().await,
        Err(ConnectionError::Timeout(_))
    ));
    assert!(!eventloop.diagnostics().connected);
}
