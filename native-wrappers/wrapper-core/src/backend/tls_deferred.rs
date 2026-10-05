//! Bridge Rustls's synchronous hooks to cancellable work on the driver.
use super::{Connector, Reason, Stage, State, handshake_timeout};
use crate::{
    AsyncTlsExternalIdentityConfig, AsyncTlsIdentityRequest, AsyncTlsSigningRequest,
    AsyncTlsVerificationRequest, AsyncTlsVerifierConfig, TlsCallbackFuture,
    TlsExternalIdentityConfig, TlsIdentityProvider, TlsIdentityRequest, TlsSigningRequest,
    TlsVerificationRequest, TlsVerifier, TlsVerifierConfig,
};
use bytes::Bytes;
use std::{
    future::Future,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};

// A handshake invokes at most one hook at a time. Both directions are bounded.
enum Request {
    Verify(AsyncTlsVerificationRequest),
    Select(AsyncTlsIdentityRequest),
    Sign(AsyncTlsSigningRequest),
}
impl Request {
    const fn stage(&self) -> Stage {
        match self {
            Self::Verify(_) => Stage::Verification,
            Self::Select(_) => Stage::IdentitySelection,
            Self::Sign(_) => Stage::Signing,
        }
    }
}
enum Reply {
    Verified,
    Selected(Option<usize>),
    Signed(Vec<u8>),
}
type Response = Result<Reply, Reason>;
struct Work {
    request: Request,
    reply: flume::Sender<Response>,
}
struct Relay {
    requests: flume::Sender<Work>,
    state: Arc<State>,
}
struct Clear<'a>(&'a State);
impl Drop for Clear<'_> {
    fn drop(&mut self) {
        *self.0.pending.lock() = None;
    }
}
impl Relay {
    fn call(&self, request: Request) -> Response {
        *self.state.pending.lock() = Some(request.stage());
        let _clear = Clear(&self.state);
        let (reply, receive) = flume::bounded(1);
        self.requests
            .send(Work { request, reply })
            .map_err(|_| Reason::Failed)?;
        receive.recv().unwrap_or(Err(Reason::Failed))
    }
}
impl TlsVerifier for Relay {
    fn verify(&self, r: &TlsVerificationRequest<'_>) -> Result<(), Reason> {
        match self.call(Request::Verify(AsyncTlsVerificationRequest {
            server_name: r.server_name.to_owned(),
            layer: r.layer,
            certificates: r
                .certificates
                .iter()
                .map(|v| Bytes::copy_from_slice(v))
                .collect(),
            ocsp_response: Bytes::copy_from_slice(r.ocsp_response),
            unix_time: r.unix_time,
            deadline: r.deadline,
        }))? {
            Reply::Verified => Ok(()),
            _ => Err(Reason::InvalidResponse),
        }
    }
}
impl TlsIdentityProvider for Relay {
    fn select(&self, r: &TlsIdentityRequest<'_>) -> Result<Option<usize>, Reason> {
        match self.call(Request::Select(AsyncTlsIdentityRequest {
            server_name: r.server_name.to_owned(),
            layer: r.layer,
            issuer_hints: r
                .issuer_hints
                .iter()
                .map(|v| Bytes::copy_from_slice(v))
                .collect(),
            signature_schemes: r.signature_schemes.to_vec(),
            deadline: r.deadline,
        }))? {
            Reply::Selected(v) => Ok(v),
            _ => Err(Reason::InvalidResponse),
        }
    }
    fn sign(&self, r: &TlsSigningRequest<'_>) -> Result<Vec<u8>, Reason> {
        match self.call(Request::Sign(AsyncTlsSigningRequest {
            server_name: r.server_name.to_owned(),
            layer: r.layer,
            identity_index: r.identity_index,
            key_id: Bytes::copy_from_slice(r.key_id),
            signature_scheme: r.signature_scheme,
            message: Bytes::copy_from_slice(r.message),
            deadline: r.deadline,
        }))? {
            Reply::Signed(v) => Ok(v),
            _ => Err(Reason::InvalidResponse),
        }
    }
}

pub(super) struct Dispatch {
    relay: Arc<Relay>,
    receive: flume::Receiver<Work>,
    verifier: Option<TlsVerifierConfig>,
    async_verifier: Option<AsyncTlsVerifierConfig>,
    external: Option<TlsExternalIdentityConfig>,
    async_external: Option<AsyncTlsExternalIdentityConfig>,
}
struct Stop(flume::Sender<()>);
impl Drop for Stop {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}
impl Dispatch {
    pub(super) fn new(config: &Connector, state: Arc<State>) -> Self {
        let (requests, receive) = flume::bounded(1);
        Self {
            relay: Arc::new(Relay { requests, state }),
            receive,
            verifier: config.verifier.clone(),
            async_verifier: config.async_verifier.clone(),
            external: config.external.clone(),
            async_external: config.async_external.clone(),
        }
    }
    pub(super) fn verifier(&self) -> Option<TlsVerifierConfig> {
        (self.verifier.is_some() || self.async_verifier.is_some())
            .then(|| TlsVerifierConfig(self.relay.clone()))
    }
    pub(super) fn external(&self) -> Option<TlsExternalIdentityConfig> {
        self.external
            .as_ref()
            .map(|v| v.with_provider(self.relay.clone()))
            .or_else(|| {
                self.async_external
                    .as_ref()
                    .map(|v| v.synchronous(self.relay.clone()))
            })
    }
    fn future(&self, request: Request) -> TlsCallbackFuture<Reply> {
        match request {
            Request::Verify(r) => {
                if let Some(v) = &self.async_verifier {
                    let work = v.0.verify(r);
                    Box::pin(async move { work.await.map(|()| Reply::Verified) })
                } else {
                    let certificates: Vec<&[u8]> =
                        r.certificates.iter().map(AsRef::as_ref).collect();
                    let result = self
                        .verifier
                        .as_ref()
                        .ok_or(Reason::InvalidResponse)
                        .and_then(|v| {
                            v.0.verify(&TlsVerificationRequest {
                                server_name: &r.server_name,
                                layer: r.layer,
                                certificates: &certificates,
                                ocsp_response: &r.ocsp_response,
                                unix_time: r.unix_time,
                                deadline: r.deadline,
                            })
                        })
                        .map(|()| Reply::Verified);
                    Box::pin(std::future::ready(result))
                }
            }
            Request::Select(r) => {
                if let Some(v) = &self.async_external {
                    let work = v.provider.select(r);
                    Box::pin(async move { work.await.map(Reply::Selected) })
                } else {
                    let issuers: Vec<&[u8]> = r.issuer_hints.iter().map(AsRef::as_ref).collect();
                    let result = self
                        .external
                        .as_ref()
                        .ok_or(Reason::InvalidResponse)
                        .and_then(|v| {
                            v.provider.select(&TlsIdentityRequest {
                                server_name: &r.server_name,
                                layer: r.layer,
                                issuer_hints: &issuers,
                                signature_schemes: &r.signature_schemes,
                                deadline: r.deadline,
                            })
                        })
                        .map(Reply::Selected);
                    Box::pin(std::future::ready(result))
                }
            }
            Request::Sign(r) => {
                if let Some(v) = &self.async_external {
                    let work = v.provider.sign(r);
                    Box::pin(async move { work.await.map(Reply::Signed) })
                } else {
                    let result = self
                        .external
                        .as_ref()
                        .ok_or(Reason::InvalidResponse)
                        .and_then(|v| {
                            v.provider.sign(&TlsSigningRequest {
                                server_name: &r.server_name,
                                layer: r.layer,
                                identity_index: r.identity_index,
                                key_id: &r.key_id,
                                signature_scheme: r.signature_scheme,
                                message: &r.message,
                                deadline: r.deadline,
                            })
                        })
                        .map(Reply::Signed);
                    Box::pin(std::future::ready(result))
                }
            }
        }
    }
    async fn respond(&self, request: Request) -> Response {
        let stage = request.stage();
        let handshake_state = &self.relay.state;
        if expired(handshake_state.deadline) {
            return Err(Reason::Timeout);
        }
        let work = host(|| self.future(request))?;
        let mut work = HostFuture {
            inner: Some(work),
            state: handshake_state.clone(),
            stage,
        };
        let result = (&mut work).await;
        let result = work.destroy().and(result);
        if expired(handshake_state.deadline) && !matches!(result, Err(Reason::Panic)) {
            Err(Reason::Timeout)
        } else {
            result
        }
    }
    pub(super) async fn connect<F, T>(
        self,
        handshake: F,
    ) -> io::Result<rumqttc_core::DynAsyncReadWrite>
    where
        F: Future<Output = io::Result<T>> + Send + 'static,
        T: rumqttc_core::AsyncReadWrite + 'static,
    {
        let state = self.relay.state.clone();
        let (stop, cancelled) = flume::bounded(1);
        let _stop = Stop(stop);
        let runtime = tokio::runtime::Handle::current();
        let mut worker = tokio::task::spawn_blocking(move || {
            let _entered = runtime.enter();
            futures_executor::block_on(async move {
                match futures_util::future::select(
                    Box::pin(handshake),
                    Box::pin(cancelled.recv_async()),
                )
                .await
                {
                    futures_util::future::Either::Left((result, _)) => result,
                    futures_util::future::Either::Right(_) => Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "TLS handshake cancelled",
                    )),
                }
            })
        });
        let mut run = Box::pin(async {
            loop {
                tokio::select! {
                    result = &mut worker => {
                        break result.unwrap_or_else(|_| Err(io::Error::other("TLS handshake worker failed")));
                    }
                    work = self.receive.recv_async() => {
                        let work = work.map_err(|_| io::Error::other("TLS callback channel closed"))?;
                        let result = self.respond(work.request).await;
                        let _ = work.reply.send(result);
                    }
                }
            }
        });
        let result = if let Some(deadline) = state.deadline {
            if let Ok(result) = tokio::time::timeout_at(deadline.into(), &mut run).await {
                result
            } else {
                let pending_hook = *state.pending.lock();
                if let Some(pending_hook) = pending_hook {
                    state.fail(pending_hook, Reason::Timeout);
                }
                drop(run);
                return Err(state.io_failure().unwrap_or_else(handshake_timeout));
            }
        } else {
            (&mut run).await
        };
        drop(run);
        if let Some(error) = state.io_failure() {
            return Err(error);
        }
        if expired(state.deadline) {
            return Err(handshake_timeout());
        }
        result.map(|v| Box::new(v) as rumqttc_core::DynAsyncReadWrite)
    }
}
fn expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|d| Instant::now() >= d)
}
fn host<T>(call: impl FnOnce() -> T) -> Result<T, Reason> {
    catch_unwind(AssertUnwindSafe(|| {
        crate::runtime::with_host_callback(call)
    }))
    .map_err(|payload| {
        std::mem::forget(payload);
        Reason::Panic
    })
}
struct HostFuture<T> {
    inner: Option<TlsCallbackFuture<T>>,
    state: Arc<State>,
    stage: Stage,
}
impl<T> HostFuture<T> {
    fn destroy(&mut self) -> Result<(), Reason> {
        if let Some(work) = self.inner.take() {
            host(|| drop(work))
                .inspect_err(|reason| self.state.fail_destruction(self.stage, *reason))?;
        }
        Ok(())
    }
}
impl<T> Future for HostFuture<T> {
    type Output = Result<T, Reason>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match host(|| {
            this.inner
                .as_mut()
                .expect("present host future")
                .as_mut()
                .poll(cx)
        }) {
            Ok(v) => v,
            Err(reason) => Poll::Ready(Err(reason)),
        }
    }
}
impl<T> Drop for HostFuture<T> {
    fn drop(&mut self) {
        let _ = self.destroy();
    }
}
