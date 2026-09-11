use super::*;
impl EventLoop {
    pub(super) fn restore_ordered_fence_after_cleanup(&mut self) {
        let gate = self.requests_rx.gate();
        let Some(sequence) = gate.snapshot().0 else {
            return;
        };
        // A terminal write may already have reached the peer; never replay it.
        if self.shutdown_phase == crate::ShutdownPhase::Flushing {
            return;
        }
        let mut fences = VecDeque::new();
        let mut requests = VecDeque::new();
        for envelope in self.pending.drain(..) {
            if matches!(
                envelope.request,
                Request::DisconnectAfterQueued(_) | Request::DisconnectAfterQueuedWithTimeout(_, _)
            ) {
                fences.push_back(envelope);
            } else {
                requests.push_back(envelope);
            }
        }
        requests.extend(fences);
        self.pending = requests;
        if let Some(disconnect) = self.ordered_packet.take() {
            let mut envelope = RequestEnvelope::plain(Request::DisconnectAfterQueued(disconnect));
            envelope.meta.completion = self.ordered_completion.take();
            envelope.meta.sequence = sequence;
            envelope.meta.deadline = gate.snapshot().1;
            self.pending.push_back(envelope);
        }
        self.shutdown_phase = crate::ShutdownPhase::Approaching;
    }
    /// Drive protocol progress and the total post-admission ordered deadline.
    pub async fn poll(&mut self) -> Result<Event, ConnectionError> {
        if self.disconnect_complete {
            if let Some(checkpoint) = self.terminal_checkpoint.as_mut() {
                let result = checkpoint.await;
                self.terminal_checkpoint = None;
                result?;
            }
            return self.poll_inner().await;
        }
        let gate = Arc::clone(self.requests_rx.gate());
        if gate.has_fence()
            && let Ok(envelope) = self.immediate_disconnect_rx.try_recv()
        {
            gate.terminate();
            self.finish_ordered(Err(crate::DisconnectNoticeError::SupersededByImmediate));
            self.discard_shutdown_work(true);
            self.shutdown_phase = crate::ShutdownPhase::Failed;
            if self.network.is_some() {
                return self.handle_immediate_disconnect(envelope).await;
            }
            self.disconnect_complete = true;
            return Err(ConnectionError::RequestsDone);
        }
        if gate.has_fence() && self.shutdown_phase == crate::ShutdownPhase::Open {
            self.shutdown_phase = crate::ShutdownPhase::Approaching;
            self.record_ordered_shutdown("pending");
        }
        let result = tokio::select! {
            biased;
            () = gate.expired() => Err(ConnectionError::DisconnectTimeout),
            result = self.poll_inner() => result,
        };
        if !gate.has_fence() {
            return result;
        }
        let error = match result {
            Ok(event) => return Ok(event),
            Err(error) => error,
        };
        if self.disconnect_complete && self.shutdown_phase == crate::ShutdownPhase::Completed {
            return Err(error);
        }
        let resumable =
            !self.options.clean_start() && self.effective_session_expiry_interval.unwrap_or(0) > 0;
        let transport_error = matches!(
            &error,
            ConnectionError::Io(_)
                | ConnectionError::Timeout(_)
                | ConnectionError::MqttState(
                    StateError::Io(_)
                        | StateError::ConnectionAborted
                        | StateError::AwaitPingResp
                        | StateError::Deserialization(crate::mqttbytes::Error::Io(_))
                )
        );
        if transport_error
            && resumable
            && self.publish_ledger.result().is_ok()
            && self.shutdown_phase != crate::ShutdownPhase::Flushing
            && !self.disconnect_complete
        {
            if let Some(disconnect) = self.ordered_packet.take() {
                let mut envelope =
                    RequestEnvelope::plain(Request::DisconnectAfterQueued(disconnect));
                envelope.meta.completion = self.ordered_completion.take();
                envelope.meta.sequence = gate.snapshot().0.unwrap();
                envelope.meta.deadline = gate.snapshot().1;
                self.pending.push_back(envelope);
            }
            self.shutdown_phase = crate::ShutdownPhase::Approaching;
            return Err(error);
        }
        let reason = match error {
            ConnectionError::OrderedDisconnect(reason) => reason,
            ConnectionError::DisconnectTimeout => crate::DisconnectNoticeError::DisconnectTimeout,
            error @ ConnectionError::SessionStore(_) => {
                crate::DisconnectNoticeError::Persistence(Arc::new(error))
            }
            error @ ConnectionError::MqttState(_) if !transport_error => {
                crate::DisconnectNoticeError::Protocol(Arc::new(error))
            }
            error => crate::DisconnectNoticeError::Transport(Arc::new(error)),
        };
        gate.terminate();
        self.finish_ordered(Err(reason.clone()));
        self.shutdown_phase = if matches!(reason, crate::DisconnectNoticeError::DisconnectTimeout) {
            crate::ShutdownPhase::TimedOut
        } else {
            crate::ShutdownPhase::Failed
        };
        self.network = None;
        self.keepalive_timeout = None;
        self.pending_disconnect = None;
        self.discard_shutdown_work(false);
        self.disconnect_complete = true;
        if matches!(reason, crate::DisconnectNoticeError::DisconnectTimeout)
            && self.session_store.loaded
        {
            self.prepare_terminal_checkpoint();
            if let Some(checkpoint) = self.terminal_checkpoint.as_mut()
                && let Some(result) = futures_util::FutureExt::now_or_never(checkpoint.as_mut())
            {
                self.terminal_checkpoint = None;
                result?;
            }
        }
        Err(ConnectionError::OrderedDisconnect(reason))
    }

    fn prepare_terminal_checkpoint(&mut self) {
        if self.effective_session_expiry_interval.unwrap_or(0) == 0 {
            if let Some(store) = self.options.session_store() {
                let key = self.options.session_store_key();
                self.session_store.clear_pending = true;
                self.terminal_checkpoint = Some(Box::pin(async move {
                    store
                        .clear(&key)
                        .await
                        .map_err(ConnectionError::SessionStore)
                }));
            }
            return;
        }
        let save = self.persisted_session_save();
        self.terminal_checkpoint = Some(Box::pin(async move { save.save().await.map(|_| ()) }));
    }

    pub(super) fn ensure_ordered_deadline(&self) -> Result<(), ConnectionError> {
        if self
            .requests_rx
            .gate()
            .snapshot()
            .1
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Err(ConnectionError::DisconnectTimeout);
        }
        Ok(())
    }

    pub(super) fn observe_publish(
        &self,
        request: &Request,
        notice: Option<TrackedNoticeTx>,
    ) -> Option<TrackedNoticeTx> {
        if !matches!(request, Request::Publish(_) | Request::PubRel(_)) {
            return notice;
        }
        let mut publish_notice = match notice {
            Some(TrackedNoticeTx::Publish(notice)) => notice,
            _ => PublishNoticeTx::internal(),
        };
        publish_notice.observe(&self.publish_ledger);
        Some(TrackedNoticeTx::Publish(publish_notice))
    }

    pub(super) fn finish_ordered(&mut self, result: Result<(), crate::DisconnectNoticeError>) {
        self.shutdown_phase = match &result {
            Ok(()) => crate::ShutdownPhase::Completed,
            Err(crate::DisconnectNoticeError::DisconnectTimeout) => crate::ShutdownPhase::TimedOut,
            Err(_) => crate::ShutdownPhase::Failed,
        };
        self.record_ordered_shutdown(
            result
                .as_ref()
                .err()
                .map_or("success", crate::DisconnectNoticeError::kind),
        );
        if matches!(
            &result,
            Err(crate::DisconnectNoticeError::Superseded
                | crate::DisconnectNoticeError::SupersededByImmediate)
        ) {
            self.requests_rx.gate().clear_deadline();
        }
        if let Some(completion) = &self.ordered_completion {
            completion.finish(result.clone());
        }
        self.pending.extend(self.requests_rx.drain());
        self.pending.extend(self.control_requests_rx.drain());
        for envelope in self.pending.iter().chain(self.queued.iter()) {
            if let Some(completion) = &envelope.meta.completion {
                completion.finish(result.clone());
            }
        }
    }

    pub(super) fn discard_shutdown_notice(
        notice: TrackedNoticeTx,
        immediate: bool,
        after_fence: bool,
    ) {
        match notice {
            TrackedNoticeTx::Publish(notice) => notice.error(if immediate {
                crate::PublishNoticeError::ShutdownSupersededByImmediate
            } else if after_fence {
                crate::PublishNoticeError::DiscardedAfterDisconnectBarrier
            } else {
                crate::PublishNoticeError::ShutdownInterrupted
            }),
            TrackedNoticeTx::Subscribe(notice) => notice.error(if immediate {
                crate::SubscribeNoticeError::ShutdownSupersededByImmediate
            } else if after_fence {
                crate::SubscribeNoticeError::DiscardedAfterDisconnectBarrier
            } else {
                crate::SubscribeNoticeError::ShutdownInterrupted
            }),
            TrackedNoticeTx::Unsubscribe(notice) => notice.error(if immediate {
                crate::UnsubscribeNoticeError::ShutdownSupersededByImmediate
            } else if after_fence {
                crate::UnsubscribeNoticeError::DiscardedAfterDisconnectBarrier
            } else {
                crate::UnsubscribeNoticeError::ShutdownInterrupted
            }),
            TrackedNoticeTx::Auth(notice) => notice.error(if immediate {
                crate::AuthNoticeError::ShutdownSupersededByImmediate
            } else if after_fence {
                crate::AuthNoticeError::DiscardedAfterDisconnectBarrier
            } else {
                crate::AuthNoticeError::ShutdownInterrupted
            }),
        }
    }

    pub(super) fn record_ordered_shutdown(&self, terminal: &str) {
        let (sequence, deadline) = self.requests_rx.gate().snapshot();
        let local_queued = self.pending.len() + self.queued.len();
        #[cfg(feature = "tracing")]
        tracing::debug!(target: "rumqttc::shutdown", policy = "ordered", ?sequence, ?deadline,
            phase = ?self.shutdown_phase, local_queued, terminal,
            outbound = %self.state.outbound_drain_diagnostics(), "ordered disconnect progress");
        #[cfg(not(feature = "tracing"))]
        log::debug!(
            "ordered disconnect sequence={sequence:?} deadline={deadline:?} phase={:?} local_queued={local_queued} terminal={terminal} outbound={}",
            self.shutdown_phase,
            self.state.outbound_drain_diagnostics()
        );
    }

    pub(super) fn discard_shutdown_work(&mut self, immediate: bool) {
        for clean in self.state.clean_with_notices_for_reconnect() {
            let mut envelope =
                RequestEnvelope::from_parts_with_replay(clean.request, clean.notice, clean.replay);
            if let Some(notice) = envelope.notice.take() {
                Self::discard_shutdown_notice(notice, immediate, false);
            }
            self.pending.push_back(envelope);
        }
        self.pending.extend(self.requests_rx.drain());
        self.pending.extend(self.control_requests_rx.drain());
        self.pending.extend(self.queued.drain());
        for envelope in &mut self.pending {
            if let Some(notice) = envelope.notice.take() {
                Self::discard_shutdown_notice(notice, immediate, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn fence_admitted_during_stalled_poll_activates_deadline() {
        use std::future::Future;
        let store = MemoryStore::default();
        store.stall.store(true, std::sync::atomic::Ordering::SeqCst);
        let (client, mut eventloop) = crate::AsyncClient::builder(persistent_options(store))
            .capacity(2)
            .build();
        let (stream, _peer) = tokio::io::duplex(4096);
        eventloop.network = Some(Network::new(stream, Some(4096)));
        client
            .try_publish(
                "pending",
                "payload",
                crate::PublishOptions::new(crate::QoS::AtLeastOnce),
            )
            .unwrap();
        let mut polling = Box::pin(eventloop.poll());
        assert!(
            polling
                .as_mut()
                .poll(&mut Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        );
        let notice = client
            .try_disconnect_after_queued_with_timeout(Duration::ZERO)
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), polling)
                .await
                .unwrap()
                .is_err()
        );
        assert!(matches!(
            notice.wait_async().await,
            Err(crate::DisconnectNoticeError::DisconnectTimeout)
        ));
    }

    #[tokio::test]
    async fn timeout_clears_checkpoint_when_session_expiry_is_zero() {
        let store = MemoryStore::default();
        let (client, mut eventloop) =
            crate::AsyncClient::builder(persistent_options(store.clone()))
                .capacity(2)
                .build();
        let (stream, _peer) = tokio::io::duplex(4096);
        eventloop.network = Some(Network::new(stream, Some(4096)));
        client
            .try_publish(
                "expiry",
                "payload",
                crate::PublishOptions::new(crate::QoS::AtLeastOnce),
            )
            .unwrap();
        assert!(eventloop.poll().await.is_ok());
        assert!(store.checkpoint.lock().unwrap().is_some());
        // Model a server-negotiated zero expiry.
        eventloop.effective_session_expiry_interval = Some(0);
        let notice = client
            .try_disconnect_after_queued_with_timeout(Duration::ZERO)
            .unwrap();
        assert!(eventloop.poll().await.is_err());
        assert!(matches!(
            notice.wait_async().await,
            Err(crate::DisconnectNoticeError::DisconnectTimeout)
        ));
        assert!(store.checkpoint.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn injected_post_fence_tracked_request_is_discarded_without_execution() {
        let (client, mut eventloop) =
            crate::AsyncClient::builder(MqttOptions::new("after-fence", "localhost"))
                .capacity(2)
                .build();
        let publish = client
            .publish_tracked(
                "later",
                "payload",
                crate::PublishOptions::new(crate::QoS::AtLeastOnce),
            )
            .await
            .unwrap();
        let mut envelope = eventloop.requests_rx.try_recv().unwrap();
        let _disconnect = client.try_disconnect_after_queued().unwrap();
        envelope.meta.sequence = eventloop.requests_rx.gate().snapshot().0.unwrap() + 1;
        let mut should_flush = false;
        let mut qos0 = Vec::new();
        let mut checkpoint = SessionCheckpointAction::Save;
        eventloop
            .handle_request_internal(envelope, &mut should_flush, &mut qos0, &mut checkpoint)
            .await
            .unwrap();
        assert!(!should_flush);
        assert!(matches!(
            publish.wait_async().await,
            Err(crate::PublishNoticeError::DiscardedAfterDisconnectBarrier)
        ));
    }

    #[tokio::test]
    async fn total_deadline_bounds_disconnect_flush() {
        let (client, mut eventloop) =
            crate::AsyncClient::builder(MqttOptions::new("stalled-flush", "localhost")).build();
        let (stream, _peer) = tokio::io::duplex(4096);
        let stream = FailingFlush {
            stream,
            disconnect_written: false,
            fail_publish: false,
            stall_disconnect: true,
        };
        eventloop.network = Some(Network::new(stream, Some(4096)));
        let notice = client
            .try_disconnect_after_queued_with_timeout(Duration::from_millis(20))
            .unwrap();
        assert!(eventloop.poll().await.is_err());
        assert!(matches!(
            notice.wait_async().await,
            Err(crate::DisconnectNoticeError::DisconnectTimeout)
        ));
        assert!(eventloop.network.is_none());
        assert_eq!(eventloop.shutdown_phase, crate::ShutdownPhase::TimedOut);
    }

    #[tokio::test]
    async fn repeated_cleanup_keeps_replayed_publish_before_fence() {
        let (client, mut eventloop) =
            crate::AsyncClient::builder(persistent_options(MemoryStore::default()))
                .capacity(3)
                .build();
        let (stream, _peer) = tokio::io::duplex(4096);
        eventloop.network = Some(Network::new(stream, Some(4096)));
        client
            .try_publish(
                "replay",
                "first",
                crate::PublishOptions::new(crate::QoS::AtLeastOnce),
            )
            .unwrap();
        assert!(eventloop.poll().await.is_ok());
        client
            .try_publish(
                "replay",
                "second",
                crate::PublishOptions::new(crate::QoS::AtLeastOnce),
            )
            .unwrap();
        let _notice = client.try_disconnect_after_queued().unwrap();
        for _ in 0..3 {
            eventloop.clean();
            assert_eq!(eventloop.pending.len(), 3);
            assert!(matches!(eventloop.pending[0].request, Request::Publish(_)));
            assert!(matches!(eventloop.pending[1].request, Request::Publish(_)));
            assert!(matches!(
                eventloop.pending[2].request,
                Request::DisconnectAfterQueued(_)
            ));
        }
    }

    #[derive(Clone, Debug, Default)]
    struct MemoryStore {
        checkpoint: Arc<std::sync::Mutex<Option<crate::PersistedSession>>>,
        fail: Arc<std::sync::atomic::AtomicBool>,
        stall: Arc<std::sync::atomic::AtomicBool>,
    }
    impl crate::SessionStore for MemoryStore {
        fn load<'a>(
            &'a self,
            _: &'a crate::SessionStoreKey,
        ) -> crate::session::SessionStoreFuture<'a, Option<crate::PersistedSession>> {
            Box::pin(async { Ok(self.checkpoint.lock().unwrap().clone()) })
        }
        fn save<'a>(
            &'a self,
            _: &'a crate::SessionStoreKey,
            value: &'a crate::PersistedSession,
        ) -> crate::session::SessionStoreFuture<'a, ()> {
            Box::pin(async move {
                if self.stall.load(std::sync::atomic::Ordering::SeqCst) {
                    std::future::pending::<()>().await;
                }
                if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err(io::Error::other("injected store failure").into());
                }
                *self.checkpoint.lock().unwrap() = Some(value.clone());
                Ok(())
            })
        }
        fn clear<'a>(
            &'a self,
            _: &'a crate::SessionStoreKey,
        ) -> crate::session::SessionStoreFuture<'a, ()> {
            Box::pin(async {
                *self.checkpoint.lock().unwrap() = None;
                Ok(())
            })
        }
    }

    fn persistent_options(store: MemoryStore) -> MqttOptions {
        let mut options = MqttOptions::new("ordered-store", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60));
        options.set_session_store(store);
        options
    }

    #[tokio::test]
    async fn persistence_failure_and_stall_resolve_ordered_notice() {
        for stall in [false, true] {
            let store = MemoryStore::default();
            store
                .fail
                .store(!stall, std::sync::atomic::Ordering::SeqCst);
            store
                .stall
                .store(stall, std::sync::atomic::Ordering::SeqCst);
            let (client, mut eventloop) = crate::AsyncClient::builder(persistent_options(store))
                .capacity(2)
                .build();
            let (stream, _peer) = tokio::io::duplex(4096);
            eventloop.network = Some(Network::new(stream, Some(4096)));
            client
                .try_publish(
                    "persistent",
                    "payload",
                    crate::PublishOptions::new(crate::QoS::AtLeastOnce),
                )
                .unwrap();
            let notice = client
                .try_disconnect_after_queued_with_timeout(Duration::from_millis(20))
                .unwrap();
            loop {
                if eventloop.poll().await.is_err() {
                    break;
                }
            }
            let result = notice.wait_async().await;
            if stall {
                assert!(
                    matches!(result, Err(crate::DisconnectNoticeError::DisconnectTimeout)),
                    "{result:?}"
                );
            } else {
                assert!(
                    matches!(result, Err(crate::DisconnectNoticeError::Persistence(_))),
                    "{result:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn timeout_retains_only_protocol_replay_in_persistent_checkpoint() {
        let store = MemoryStore::default();
        let (client, mut eventloop) =
            crate::AsyncClient::builder(persistent_options(store.clone()))
                .capacity(2)
                .build();
        let (stream, _peer) = tokio::io::duplex(4096);
        eventloop.network = Some(Network::new(stream, Some(4096)));
        client
            .try_publish(
                "persistent",
                "payload",
                crate::PublishOptions::new(crate::QoS::AtLeastOnce),
            )
            .unwrap();
        assert!(eventloop.poll().await.is_ok());
        let notice = client
            .try_disconnect_after_queued_with_timeout(Duration::ZERO)
            .unwrap();
        assert!(eventloop.poll().await.is_err());
        assert!(matches!(
            notice.wait_async().await,
            Err(crate::DisconnectNoticeError::DisconnectTimeout)
        ));
        let checkpoint = store.checkpoint.lock().unwrap().clone().unwrap();
        assert!(matches!(
            checkpoint.replay.as_slice(),
            [crate::PersistedRequest::Publish(_)]
        ));
    }
    use super::*;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    struct FailingFlush {
        stream: tokio::io::DuplexStream,
        disconnect_written: bool,
        fail_publish: bool,
        stall_disconnect: bool,
    }
    impl AsyncRead for FailingFlush {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.stream).poll_read(cx, buf)
        }
    }
    impl AsyncWrite for FailingFlush {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            if bytes.first() == Some(&0xe0) {
                self.disconnect_written = true;
            }
            Pin::new(&mut self.stream).poll_write(cx, bytes)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            if self.disconnect_written && self.stall_disconnect {
                return Poll::Pending;
            }
            if self.disconnect_written || self.fail_publish {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected flush failure",
                )));
            }
            Pin::new(&mut self.stream).poll_flush(cx)
        }
        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.stream).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn flush_failure_never_completes_ordered_notice_successfully() {
        for fail_publish in [false, true] {
            let options = MqttOptions::new("flush-failure", "localhost");
            let (client, mut eventloop) = crate::AsyncClient::builder(options).capacity(2).build();
            let (stream, _peer) = tokio::io::duplex(4096);
            eventloop.network = Some(Network::new(
                FailingFlush {
                    stream,
                    disconnect_written: false,
                    fail_publish,
                    stall_disconnect: false,
                },
                Some(4096),
            ));
            if fail_publish {
                client
                    .try_publish(
                        "qos0",
                        "payload",
                        crate::PublishOptions::new(crate::QoS::AtMostOnce),
                    )
                    .unwrap();
            }
            let notice = client
                .try_disconnect_after_queued_with_timeout(Duration::from_secs(1))
                .unwrap();
            loop {
                if eventloop.poll().await.is_err() {
                    break;
                }
            }
            let result = notice.wait_async().await;
            assert!(
                matches!(result, Err(crate::DisconnectNoticeError::Transport(_))),
                "fail_publish={fail_publish}: {result:?}"
            );
            assert_eq!(eventloop.shutdown_phase, crate::ShutdownPhase::Failed);
        }
    }

    #[tokio::test]
    async fn explicit_reset_fails_a_fence_still_in_the_channel() {
        let (client, mut eventloop) =
            crate::AsyncClient::builder(MqttOptions::new("reset", "localhost"))
                .capacity(1)
                .build();
        let notice = client.try_disconnect_after_queued().unwrap();
        eventloop.reset_session_state();
        assert!(matches!(
            notice.wait_async().await,
            Err(crate::DisconnectNoticeError::SessionReset)
        ));
        assert!(matches!(
            eventloop.poll().await,
            Err(ConnectionError::RequestsDone)
        ));
    }

    #[tokio::test]
    async fn immediate_shutdown_supersedes_an_unobserved_fence_and_tracked_work() {
        let (client, mut eventloop) =
            crate::AsyncClient::builder(MqttOptions::new("immediate", "localhost"))
                .capacity(2)
                .build();
        let publish = client
            .publish_tracked(
                "queued",
                "payload",
                crate::PublishOptions::new(crate::QoS::AtLeastOnce),
            )
            .await
            .unwrap();
        let notice = client.try_disconnect_after_queued().unwrap();
        client.try_disconnect_now().unwrap();
        assert!(eventloop.poll().await.is_err());
        assert!(matches!(
            notice.wait_async().await,
            Err(crate::DisconnectNoticeError::SupersededByImmediate)
        ));
        assert!(matches!(
            publish.wait_async().await,
            Err(crate::PublishNoticeError::ShutdownSupersededByImmediate)
        ));
    }

    #[tokio::test]
    async fn terminal_disconnect_rejects_later_ordered_fences() {
        let (client, mut eventloop) =
            crate::AsyncClient::builder(MqttOptions::new("terminal-immediate", "localhost"))
                .build();
        client.try_disconnect_now().unwrap();
        assert!(matches!(
            eventloop.poll().await,
            Err(ConnectionError::RequestsDone)
        ));
        assert!(matches!(
            client.try_disconnect_after_queued_with_timeout(Duration::ZERO),
            Err(crate::ClientError::RequestChannelDisconnected(_))
        ));

        let (client, mut eventloop) =
            crate::AsyncClient::builder(MqttOptions::new("terminal-graceful", "localhost")).build();
        let (stream, _peer) = tokio::io::duplex(4096);
        eventloop.network = Some(Network::new(stream, Some(4096)));
        client.disconnect().await.unwrap();
        assert!(matches!(
            eventloop.poll().await,
            Ok(Event::Outgoing(Outgoing::Disconnect))
        ));
        assert!(matches!(
            client.try_disconnect_after_queued_with_timeout(Duration::ZERO),
            Err(crate::ClientError::RequestChannelDisconnected(_))
        ));
    }

    #[tokio::test]
    async fn dropping_notice_does_not_cancel_disconnect() {
        let (client, mut eventloop) =
            crate::AsyncClient::builder(MqttOptions::new("drop-notice", "localhost"))
                .capacity(1)
                .build();
        let (stream, mut peer) = tokio::io::duplex(4096);
        eventloop.network = Some(Network::new(stream, Some(4096)));
        drop(client.try_disconnect_after_queued().unwrap());
        assert!(matches!(
            eventloop.poll().await,
            Ok(Event::Outgoing(Outgoing::Disconnect))
        ));
        assert_eq!(eventloop.shutdown_phase, crate::ShutdownPhase::Completed);
        use tokio::io::AsyncReadExt;
        let mut header = [0u8; 2];
        peer.read_exact(&mut header).await.unwrap();
        assert_eq!(header[0], 0xe0);
    }
}
