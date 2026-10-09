use super::*;
use rumqttc_core::session_recovery::{RecoveryPhase, SessionRecoveryGate};

impl EventLoop {
    /// Retained connection-owner coordination. The owner must serialize every producer and
    /// shutdown admission with `request`, and hold admission until recovery is committed.
    #[doc(hidden)]
    pub fn session_recovery_gate(&self) -> Arc<SessionRecoveryGate> {
        self.recovery.clone()
    }

    fn recovery_observation(
        &self,
    ) -> Result<Arc<rumqttc_core::session_recovery::RecoveryObservation>, ConnectionError> {
        self.recovery
            .observation()
            .ok_or(ConnectionError::SessionRecoveryInvalid)
    }

    /// Abandons all unfinished MQTT work after the owner's active poll has completed.
    /// No application callback runs under the owner's admission lock. Failure is terminal for
    /// managed owners: a partially cleared checkpoint must not be reloaded.
    #[doc(hidden)]
    pub async fn abandon_session_for_recovery(&mut self) -> Result<(), ConnectionError> {
        self.abandon_session_for_recovery_with_shutdown(|| None)
            .await
    }

    /// Abandons the session while honoring shutdown already committed by the owner.
    /// The callback has the same admission-barrier contract as fresh establishment.
    #[doc(hidden)]
    pub async fn abandon_session_for_recovery_with_shutdown(
        &mut self,
        shutdown: impl Fn() -> Option<Disconnect>,
    ) -> Result<(), ConnectionError> {
        let observation = self.recovery_observation()?;
        if observation.snapshot().phase != RecoveryPhase::Quiescing {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        if !self.recovery.identity_matches(
            &self.options.client_id(),
            self.options.session_store_scope(),
        ) {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        observation.phase(RecoveryPhase::Abandoning);
        // An already-running attempt may have established a candidate. Retire it without
        // polling application requests or exposing it as a usable connection.
        let disconnect = shutdown()
            .unwrap_or_else(|| Disconnect::new(DisconnectReasonCode::NormalDisconnection));
        self.recovery_close_candidate(disconnect).await?;
        self.pending_disconnect = None;
        for envelope in self
            .requests_rx
            .drain()
            .chain(self.control_requests_rx.drain())
        {
            if let Some(notice) = envelope.notice {
                Self::fail_tracked_notice(notice, NoticeFailureReason::SessionReset);
            }
        }
        self.begin_publish_recovery();
        {
            let admission = self.publish_admission.clone();
            let _cleanup = admission
                .as_ref()
                .map(|admission| admission.begin_connection_cleanup());
            self.reset_session_state();
        }
        self.reconnect_topic_aliases.clear();
        self.pending_server_redirect = None;
        self.pending_connection_error = None;
        self.pending_redirect_shutdown = false;
        self.state.events.clear();
        observation.abandoned();
        observation.phase(RecoveryPhase::ClearingCheckpoint);
        self.clear_persisted_session().await?;
        self.finish_publish_recovery();
        observation.cleared();
        observation.phase(RecoveryPhase::EstablishingFresh);
        Ok(())
    }

    async fn recovery_close_candidate(
        &mut self,
        disconnect: Disconnect,
    ) -> Result<(), ConnectionError> {
        self.keepalive_timeout = None;
        self.connack_session = None;
        let expiry = self.disconnect_session_expiry_interval(&disconnect);
        if let Some(mut network) = self.network.take() {
            time::timeout(self.options.connect_timeout(), async {
                network.write(Packet::Disconnect(disconnect)).await?;
                network.flush().await
            })
            .await??;
            self.effective_session_expiry_interval = expiry;
        }
        self.recovery.disconnected(
            self.options.client_id(),
            self.options.session_store_scope().to_owned(),
        );
        Ok(())
    }

    /// Retires a candidate when committed shutdown wins the final owner handoff.
    #[doc(hidden)]
    pub async fn retire_session_recovery_transport(&mut self) -> Result<(), ConnectionError> {
        self.retire_session_recovery_transport_with_disconnect(Disconnect::new(
            DisconnectReasonCode::NormalDisconnection,
        ))
        .await
    }

    /// Retires a candidate with the owner's committed shutdown packet. The owner must
    /// validate the packet and observe shutdown commitment under its admission barrier.
    #[doc(hidden)]
    pub async fn retire_session_recovery_transport_with_disconnect(
        &mut self,
        disconnect: Disconnect,
    ) -> Result<(), ConnectionError> {
        self.recovery_observation()?;
        let had_network = self.network.is_some();
        self.recovery_close_candidate(disconnect).await?;
        if had_network {
            // Retain the same clear/save obligations as an ordinary successful shutdown.
            self.checkpoint_after_connection_loss().await?;
        }
        Ok(())
    }

    /// Establishes recovery traffic only; no application queue is processed.
    /// `still_running` must observe committed shutdown under the owner's admission barrier.
    #[doc(hidden)]
    pub async fn establish_session_for_recovery(
        &mut self,
        still_running: impl Fn() -> bool,
    ) -> Result<Event, ConnectionError> {
        self.establish_session_for_recovery_with_shutdown(|| {
            if still_running() {
                None
            } else {
                Some(Disconnect::new(DisconnectReasonCode::NormalDisconnection))
            }
        })
        .await
    }

    /// Establishes recovery traffic with an atomic owner lifecycle observation. The callback
    /// returns `None` while running, or the committed, validated shutdown packet for retirement.
    #[doc(hidden)]
    pub async fn establish_session_for_recovery_with_shutdown(
        &mut self,
        shutdown: impl Fn() -> Option<Disconnect>,
    ) -> Result<Event, ConnectionError> {
        let observation = self.recovery_observation()?;
        let progress = observation.snapshot();
        if !progress.abandonment_committed || !progress.checkpoint_cleared {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        if !self.recovery.identity_matches(
            &self.options.client_id(),
            self.options.session_store_scope(),
        ) {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        if shutdown().is_some() {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        observation.phase(RecoveryPhase::EstablishingFresh);
        // An accepted SRV redirect applies its credentials before selecting a broker.
        // Prepare that existing target without enabling ordinary redirect following or
        // fallback during the pinned recovery transaction.
        self.resolve_active_srv().await?;
        if !self.recovery.identity_matches(
            &self.options.client_id(),
            self.options.session_store_scope(),
        ) || shutdown().is_some()
        {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        let clean_start = self.options.clean_start();
        self.options.set_clean_start(true);
        // Do not follow a redirect to another session/store identity in this transaction.
        let result = self.establish_connection_attempt().await;
        self.options.set_clean_start(clean_start);
        if result?.is_some() {
            self.state.events.clear();
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        if let Some(disconnect) = shutdown() {
            self.retire_session_recovery_transport_with_disconnect(disconnect)
                .await?;
            self.state.events.clear();
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        let connack = self
            .state
            .events
            .drain(..)
            .find_map(|event| match event {
                Event::Incoming(Packet::ConnAck(connack)) => Some(connack),
                _ => None,
            })
            .ok_or(ConnectionError::SessionRecoveryInvalid)?;
        self.state.events.clear();
        observation.established(connack.session_present);
        Ok(Event::Incoming(Packet::ConnAck(connack)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn redirect_profile_application_preserves_an_admitted_recovery_identity() {
        let mut options = MqttOptions::new("origin-client", "origin.example");
        options.set_session_store_scope("origin-scope");
        let mut eventloop = EventLoop::new(options, 8);
        let gate = eventloop.session_recovery_gate();
        let observation = gate.request().unwrap();
        let reference = parse_server_references(Some("temporary.example"))
            .unwrap()
            .remove(0);
        let profile = RedirectTargetProfile::isolated(reference, Transport::tcp())
            .unwrap()
            .client_id(RedirectClientId::Replace("target-client".to_owned()))
            .session(RedirectSession::Reuse {
                store_scope: "target-scope".to_owned(),
            });
        eventloop.apply_redirect_profile(&profile);

        assert!(gate.identity_matches("origin-client", "origin-scope"));
        assert_eq!(eventloop.options.client_id(), "target-client");
        assert_eq!(eventloop.options.session_store_scope(), "target-scope");
        assert!(matches!(
            eventloop.abandon_session_for_recovery().await,
            Err(ConnectionError::SessionRecoveryInvalid)
        ));
        assert!(!observation.snapshot().abandonment_committed);
    }

    #[tokio::test]
    async fn origin_restoration_preserves_recovery_admitted_during_disconnect() {
        let policy =
            crate::RedirectPolicy::new(std::num::NonZeroUsize::new(1).unwrap(), |context| {
                RedirectDecision::follow(
                    RedirectTargetProfile::isolated(
                        context.references[0].clone(),
                        Transport::tcp(),
                    )
                    .unwrap()
                    .client_id(RedirectClientId::Replace("target-client".to_owned()))
                    .session(RedirectSession::Reuse {
                        store_scope: "target-scope".to_owned(),
                    }),
                )
            });
        let mut options = MqttOptions::new("origin-client", "origin.example");
        options
            .set_session_store_scope("origin-scope")
            .set_redirect_policy(policy);
        let mut eventloop = EventLoop::new(options, 8);
        eventloop
            .handle_redirect_outcome(RedirectOutcome {
                reason: RedirectReason::UseAnotherServer,
                server_reference: Some("temporary.example".to_owned()),
                source: RedirectSource::ConnAck,
            })
            .unwrap();
        eventloop.active_redirect.as_mut().unwrap().established = true;
        let gate = eventloop.session_recovery_gate();
        gate.establish("target-client".to_owned(), "target-scope".to_owned());
        assert!(!gate.can_request());
        eventloop.clean_with_notice_reason(NoticeFailureReason::Redirected);
        let observation = gate.request().unwrap();
        eventloop.restore_redirect_origin().unwrap();

        assert!(gate.identity_matches("target-client", "target-scope"));
        assert_eq!(eventloop.options.client_id(), "origin-client");
        assert_eq!(eventloop.options.session_store_scope(), "origin-scope");
        assert!(matches!(
            eventloop.abandon_session_for_recovery().await,
            Err(ConnectionError::SessionRecoveryInvalid)
        ));
        assert!(!observation.snapshot().abandonment_committed);
    }

    #[tokio::test]
    async fn abandonment_retires_every_request_source_and_protocol_ownership() {
        let options = MqttOptions::new("recovery", "localhost");
        let mut eventloop = EventLoop::new(options, 8);
        let mut notices = Vec::new();
        for source in 0..5 {
            let (tx, notice) = PublishNoticeTx::new();
            notices.push(notice);
            let publish = Publish::new(
                "discarded",
                crate::mqttbytes::QoS::AtLeastOnce,
                "payload",
                None,
            );
            match source {
                0 => eventloop
                    .pending
                    .push_back(RequestEnvelope::tracked_publish(publish, tx)),
                1 => eventloop
                    .queued
                    .push_back(RequestEnvelope::tracked_publish(publish, tx)),
                2 => eventloop
                    ._requests_tx
                    .as_ref()
                    .unwrap()
                    .try_send(RequestEnvelope::tracked_publish(publish, tx))
                    .unwrap(),
                3 => eventloop
                    ._control_requests_tx
                    .as_ref()
                    .unwrap()
                    .try_send(RequestEnvelope::tracked_publish(publish, tx))
                    .unwrap(),
                _ => {
                    eventloop
                        .state
                        .handle_outgoing_packet_with_notice(
                            Request::Publish(publish),
                            Some(TrackedNoticeTx::Publish(tx)),
                        )
                        .unwrap();
                }
            }
        }
        let mut incoming = Publish::new(
            "discarded",
            crate::mqttbytes::QoS::AtLeastOnce,
            "payload",
            None,
        );
        incoming.qos = crate::mqttbytes::QoS::ExactlyOnce;
        incoming.pkid = 42;
        eventloop
            .state
            .handle_incoming_packet(Incoming::Publish(incoming))
            .unwrap();
        let observation = eventloop.recovery.request().unwrap();
        eventloop.abandon_session_for_recovery().await.unwrap();
        for notice in notices {
            assert_eq!(
                notice.wait_async().await.unwrap_err(),
                PublishNoticeError::SessionReset
            );
        }
        assert_eq!(eventloop.pending_len(), 0);
        assert!(eventloop.requests_rx.is_empty() && eventloop.control_requests_rx.is_empty());
        assert!(eventloop.state.outbound_requests_drained());
        assert!(eventloop.state.events.is_empty());
        assert!(eventloop.network.is_none());
        assert!(!eventloop.session_store.clear_pending);
        let snapshot = observation.snapshot();
        assert!(snapshot.abandonment_committed && snapshot.checkpoint_cleared);
        assert!(!snapshot.fresh_established);
    }
}
