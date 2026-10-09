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
        let observation = self.recovery_observation()?;
        if observation.snapshot().phase != RecoveryPhase::Quiescing {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        if !self.recovery.identity_matches(
            &self.mqtt_options.client_id(),
            self.mqtt_options.session_store_scope(),
        ) {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        observation.phase(RecoveryPhase::Abandoning);
        // An already-running attempt may have established a candidate. Retire it without
        // polling application requests or exposing it as a usable connection.
        self.recovery_close_candidate().await?;
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
        self.reset_session_state();
        self.state.events.clear();
        self.recovery_clean_finished = false;
        observation.abandoned();
        observation.phase(RecoveryPhase::ClearingCheckpoint);
        self.clear_persisted_session().await?;
        observation.cleared();
        observation.phase(RecoveryPhase::EstablishingFresh);
        Ok(())
    }

    async fn recovery_close_candidate(&mut self) -> Result<(), ConnectionError> {
        self.keepalive_timeout = None;
        self.connack_session = None;
        if let Some(mut network) = self.network.take() {
            time::timeout(
                Duration::from_secs(self.network_options.connection_timeout()),
                async {
                    network.write(Packet::Disconnect).await?;
                    network.flush().await
                },
            )
            .await
            .map_err(|_| ConnectionError::FlushTimeout)??;
        }
        self.recovery.disconnected(
            self.mqtt_options.client_id(),
            self.mqtt_options.session_store_scope().to_owned(),
        );
        Ok(())
    }

    /// Retires a candidate when committed shutdown wins the final owner handoff.
    #[doc(hidden)]
    pub async fn retire_session_recovery_transport(&mut self) -> Result<(), ConnectionError> {
        self.recovery_observation()?;
        self.recovery_close_candidate().await
    }

    /// Establishes recovery traffic only; no application queue is processed.
    /// `still_running` must observe committed shutdown under the owner's admission barrier.
    #[doc(hidden)]
    pub async fn establish_session_for_recovery(
        &mut self,
        still_running: impl Fn() -> bool,
    ) -> Result<Event, ConnectionError> {
        let observation = self.recovery_observation()?;
        let progress = observation.snapshot();
        if !progress.abandonment_committed || !progress.checkpoint_cleared {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        if !self.recovery.identity_matches(
            &self.mqtt_options.client_id(),
            self.mqtt_options.session_store_scope(),
        ) {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        if !still_running() {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        let persistent = !self.mqtt_options.clean_session();
        if !self.recovery_clean_finished {
            observation.phase(RecoveryPhase::EstablishingFresh);
            self.mqtt_options.set_clean_session(true);
            let result = self.establish_connection().await;
            self.mqtt_options.set_clean_session(!persistent);
            let event = result?;
            if !still_running() {
                self.recovery_close_candidate().await?;
                return Err(ConnectionError::SessionRecoveryInvalid);
            }
            if !persistent {
                let Event::Incoming(Packet::ConnAck(connack)) = &event else {
                    unreachable!()
                };
                observation.established(connack.session_present);
                return Ok(event);
            }
            observation.phase(RecoveryPhase::CleanDisconnect);
            self.recovery_close_candidate().await?;
            self.state.reset_session_state();
            self.state.events.clear();
            self.recovery_clean_finished = true;
        }
        if !still_running() {
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        observation.phase(RecoveryPhase::EstablishingPersistent);
        let event = self.establish_connection().await?;
        if !still_running() {
            self.recovery_close_candidate().await?;
            return Err(ConnectionError::SessionRecoveryInvalid);
        }
        let Event::Incoming(Packet::ConnAck(connack)) = &event else {
            unreachable!()
        };
        if connack.session_present {
            self.network = None;
            return Err(ConnectionError::SessionStateMismatch {
                clean_session: false,
                session_present: true,
            });
        }
        observation.established(connack.session_present);
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn abandonment_retires_every_request_source_and_protocol_ownership() {
        let options = MqttOptions::new("recovery", "localhost");
        let mut eventloop = EventLoop::new(options, 8);
        let mut notices = Vec::new();
        for source in 0..5 {
            let (tx, notice) = PublishNoticeTx::new();
            notices.push(notice);
            let publish = Publish::new("discarded", crate::mqttbytes::QoS::AtLeastOnce, "payload");
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
        let mut incoming = Publish::new("discarded", crate::mqttbytes::QoS::AtLeastOnce, "payload");
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
