use super::{
    AsyncClient, Client, ClientError, Duration, Request, RequestEnvelope, RequestSender,
    map_send_envelope_error, map_try_send_envelope_error,
};

impl AsyncClient {
    fn ordered_envelope(
        &self,
        disconnect: crate::Disconnect,
        timeout: Option<Duration>,
    ) -> Result<(RequestEnvelope, crate::DisconnectNotice), ClientError> {
        if matches!(self.request_tx, RequestSender::Plain(_)) {
            return Err(ClientError::TrackingUnavailable);
        }
        if let Some(timeout) = timeout {
            std::time::Instant::now()
                .checked_add(timeout)
                .ok_or(ClientError::InvalidDisconnectTimeout)?;
        }
        let request = match timeout {
            Some(timeout) => Request::DisconnectAfterQueuedWithTimeout(disconnect, timeout),
            None => Request::DisconnectAfterQueued(disconnect),
        };
        let (completion, notice) = crate::disconnect::Completion::new();
        let mut envelope = RequestEnvelope::plain(request);
        envelope.meta.completion = Some(completion);
        Ok((envelope, notice))
    }
    async fn async_send_ordered(
        &self,
        disconnect: crate::Disconnect,
        timeout: Option<Duration>,
    ) -> Result<crate::DisconnectNotice, ClientError> {
        let (envelope, notice) = self.ordered_envelope(disconnect, timeout)?;
        let RequestSender::WithNotice { requests, .. } = &self.request_tx else {
            unreachable!()
        };
        requests
            .send_async(envelope)
            .await
            .map_err(map_send_envelope_error)?;
        Ok(notice)
    }
    fn try_send_ordered(
        &self,
        disconnect: crate::Disconnect,
        timeout: Option<Duration>,
    ) -> Result<crate::DisconnectNotice, ClientError> {
        let (envelope, notice) = self.ordered_envelope(disconnect, timeout)?;
        let RequestSender::WithNotice { requests, .. } = &self.request_tx else {
            unreachable!()
        };
        requests
            .try_send(envelope)
            .map_err(map_try_send_envelope_error)?;
        Ok(notice)
    }
    fn blocking_send_ordered(
        &self,
        disconnect: crate::Disconnect,
        timeout: Option<Duration>,
    ) -> Result<crate::DisconnectNotice, ClientError> {
        let (envelope, notice) = self.ordered_envelope(disconnect, timeout)?;
        let RequestSender::WithNotice { requests, .. } = &self.request_tx else {
            unreachable!()
        };
        requests.send(envelope).map_err(map_send_envelope_error)?;
        Ok(notice)
    }
}
impl AsyncClient {
    /// Enqueue a publish-only shutdown fence and return its completion notice.
    ///
    /// Unlike `disconnect()`, includes preceding queued publishes; unlike
    /// `disconnect_now()`, finishes their `QoS` handshakes. Return means admission,
    /// not DISCONNECT flush. Clones order by successful admission; later operations
    /// return `ClientError::Closing`. Dropping the notice does not cancel shutdown.
    /// May wait indefinitely; prefer the timeout form for application shutdown.
    /// Keep polling the event loop while waiting. External senders return
    /// `ClientError::TrackingUnavailable`.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` when tracking is unavailable, the timeout is invalid, or the request cannot be admitted.
    pub async fn disconnect_after_queued(&self) -> Result<crate::DisconnectNotice, ClientError> {
        self.async_send_ordered(crate::Disconnect, None).await
    }
    /// Enqueue a publish-only shutdown fence and return its completion notice.
    ///
    /// Unlike `disconnect()`, includes preceding queued publishes; unlike
    /// `disconnect_now()`, finishes their `QoS` handshakes. Return means admission,
    /// not DISCONNECT flush. Clones order by successful admission; later operations
    /// return `ClientError::Closing`. Dropping the notice does not cancel shutdown.
    /// The total deadline starts at admission. Zero expires immediately; capacity waits are excluded.
    /// Keep polling the event loop while waiting. External senders return
    /// `ClientError::TrackingUnavailable`.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` when tracking is unavailable, the timeout is invalid, or the request cannot be admitted.
    pub async fn disconnect_after_queued_with_timeout(
        &self,
        timeout: Duration,
    ) -> Result<crate::DisconnectNotice, ClientError> {
        self.async_send_ordered(crate::Disconnect, Some(timeout))
            .await
    }
    /// Enqueue a publish-only shutdown fence and return its completion notice.
    ///
    /// Unlike `disconnect()`, includes preceding queued publishes; unlike
    /// `disconnect_now()`, finishes their `QoS` handshakes. Return means admission,
    /// not DISCONNECT flush. Clones order by successful admission; later operations
    /// return `ClientError::Closing`. Dropping the notice does not cancel shutdown.
    /// May wait indefinitely; prefer the timeout form for application shutdown.
    /// Keep polling the event loop while waiting. External senders return
    /// `ClientError::TrackingUnavailable`.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` when tracking is unavailable, the timeout is invalid, or the request cannot be admitted.
    pub fn try_disconnect_after_queued(&self) -> Result<crate::DisconnectNotice, ClientError> {
        self.try_send_ordered(crate::Disconnect, None)
    }
    /// Enqueue a publish-only shutdown fence and return its completion notice.
    ///
    /// Unlike `disconnect()`, includes preceding queued publishes; unlike
    /// `disconnect_now()`, finishes their `QoS` handshakes. Return means admission,
    /// not DISCONNECT flush. Clones order by successful admission; later operations
    /// return `ClientError::Closing`. Dropping the notice does not cancel shutdown.
    /// The total deadline starts at admission. Zero expires immediately; capacity waits are excluded.
    /// Keep polling the event loop while waiting. External senders return
    /// `ClientError::TrackingUnavailable`.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` when tracking is unavailable, the timeout is invalid, or the request cannot be admitted.
    pub fn try_disconnect_after_queued_with_timeout(
        &self,
        timeout: Duration,
    ) -> Result<crate::DisconnectNotice, ClientError> {
        self.try_send_ordered(crate::Disconnect, Some(timeout))
    }
}
impl Client {
    /// Enqueue a publish-only shutdown fence and return its completion notice.
    ///
    /// Unlike `disconnect()`, includes preceding queued publishes; unlike
    /// `disconnect_now()`, finishes their `QoS` handshakes. Return means admission,
    /// not DISCONNECT flush. Clones order by successful admission; later operations
    /// return `ClientError::Closing`. Dropping the notice does not cancel shutdown.
    /// May wait indefinitely; prefer the timeout form for application shutdown.
    /// Keep polling the event loop while waiting. External senders return
    /// `ClientError::TrackingUnavailable`.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` when tracking is unavailable, the timeout is invalid, or the request cannot be admitted.
    pub fn disconnect_after_queued(&self) -> Result<crate::DisconnectNotice, ClientError> {
        self.client.blocking_send_ordered(crate::Disconnect, None)
    }
    /// Enqueue a publish-only shutdown fence and return its completion notice.
    ///
    /// Unlike `disconnect()`, includes preceding queued publishes; unlike
    /// `disconnect_now()`, finishes their `QoS` handshakes. Return means admission,
    /// not DISCONNECT flush. Clones order by successful admission; later operations
    /// return `ClientError::Closing`. Dropping the notice does not cancel shutdown.
    /// The total deadline starts at admission. Zero expires immediately; capacity waits are excluded.
    /// Keep polling the event loop while waiting. External senders return
    /// `ClientError::TrackingUnavailable`.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` when tracking is unavailable, the timeout is invalid, or the request cannot be admitted.
    pub fn disconnect_after_queued_with_timeout(
        &self,
        timeout: Duration,
    ) -> Result<crate::DisconnectNotice, ClientError> {
        self.client
            .blocking_send_ordered(crate::Disconnect, Some(timeout))
    }
    /// Enqueue a publish-only shutdown fence and return its completion notice.
    ///
    /// Unlike `disconnect()`, includes preceding queued publishes; unlike
    /// `disconnect_now()`, finishes their `QoS` handshakes. Return means admission,
    /// not DISCONNECT flush. Clones order by successful admission; later operations
    /// return `ClientError::Closing`. Dropping the notice does not cancel shutdown.
    /// May wait indefinitely; prefer the timeout form for application shutdown.
    /// Keep polling the event loop while waiting. External senders return
    /// `ClientError::TrackingUnavailable`.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` when tracking is unavailable, the timeout is invalid, or the request cannot be admitted.
    pub fn try_disconnect_after_queued(&self) -> Result<crate::DisconnectNotice, ClientError> {
        self.client.try_send_ordered(crate::Disconnect, None)
    }
    /// Enqueue a publish-only shutdown fence and return its completion notice.
    ///
    /// Unlike `disconnect()`, includes preceding queued publishes; unlike
    /// `disconnect_now()`, finishes their `QoS` handshakes. Return means admission,
    /// not DISCONNECT flush. Clones order by successful admission; later operations
    /// return `ClientError::Closing`. Dropping the notice does not cancel shutdown.
    /// The total deadline starts at admission. Zero expires immediately; capacity waits are excluded.
    /// Keep polling the event loop while waiting. External senders return
    /// `ClientError::TrackingUnavailable`.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` when tracking is unavailable, the timeout is invalid, or the request cannot be admitted.
    pub fn try_disconnect_after_queued_with_timeout(
        &self,
        timeout: Duration,
    ) -> Result<crate::DisconnectNotice, ClientError> {
        self.client
            .try_send_ordered(crate::Disconnect, Some(timeout))
    }
}
