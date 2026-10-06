use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use crate::{Error, ErrorKind, OperationId, ProtocolVersion, QoS, Result};

/// The terminal broker packet retained by a tracked operation, not handshake history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AcknowledgementKind {
    PubAck = 4,
    PubRec = 5,
    PubComp = 7,
    SubAck = 9,
    UnsubAck = 11,
}

/// Packet-level MQTT 5 diagnostic properties. Debug deliberately omits their contents.
#[derive(PartialEq, Eq)]
pub struct AcknowledgementProperties {
    pub reason_string: Option<String>,
    pub user_properties: Vec<(String, String)>,
}

impl std::fmt::Debug for AcknowledgementProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcknowledgementProperties")
            .field("reason_string_present", &self.reason_string.is_some())
            .field("user_property_count", &self.user_properties.len())
            .finish_non_exhaustive()
    }
}

/// Immutable terminal ACK contents. MQTT 3.1.1 has no scalar reason or properties;
/// its SUBACK return codes are available, while UNSUBACK has no per-filter results.
#[derive(Debug, PartialEq, Eq)]
pub struct BrokerAcknowledgement {
    protocol: ProtocolVersion,
    kind: AcknowledgementKind,
    packet_id: u16,
    reason_code: Option<u8>,
    filter_reason_codes: Option<Box<[u8]>>,
    properties: Option<AcknowledgementProperties>,
    recovered: bool,
}

impl BrokerAcknowledgement {
    pub(crate) const fn new(
        protocol: ProtocolVersion,
        kind: AcknowledgementKind,
        packet_id: u16,
        reason_code: Option<u8>,
        filter_reason_codes: Option<Box<[u8]>>,
        properties: Option<AcknowledgementProperties>,
        recovered: bool,
    ) -> Self {
        Self {
            protocol,
            kind,
            packet_id,
            reason_code,
            filter_reason_codes,
            properties,
            recovered,
        }
    }

    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }
    #[must_use]
    pub const fn kind(&self) -> AcknowledgementKind {
        self.kind
    }
    #[must_use]
    pub const fn packet_id(&self) -> u16 {
        self.packet_id
    }
    #[must_use]
    pub const fn reason_code(&self) -> Option<u8> {
        self.reason_code
    }
    #[must_use]
    pub fn filter_reason_codes(&self) -> Option<&[u8]> {
        self.filter_reason_codes.as_deref()
    }
    #[must_use]
    pub const fn properties(&self) -> Option<&AcknowledgementProperties> {
        self.properties.as_ref()
    }
    /// True only for the native recovered `QoS` 2 terminal outcome.
    #[must_use]
    pub const fn recovered(&self) -> bool {
        self.recovered
    }
}

/// One immutable operation outcome, including an ACK even when the operation failed.
/// Observations share this object; legacy waits project its existing result.
#[derive(Debug)]
pub struct TerminalOutcome {
    result: Result<Completion>,
    acknowledgement: Option<BrokerAcknowledgement>,
}

impl TerminalOutcome {
    pub(crate) const fn with_acknowledgement(
        result: Result<Completion>,
        acknowledgement: BrokerAcknowledgement,
    ) -> Self {
        Self {
            result,
            acknowledgement: Some(acknowledgement),
        }
    }
    /// The legacy result, including its original error classification.
    ///
    /// # Errors
    ///
    /// The contained error is the operation's terminal failure, not an observation failure.
    pub const fn result(&self) -> &Result<Completion> {
        &self.result
    }
    #[must_use]
    pub const fn acknowledgement(&self) -> Option<&BrokerAcknowledgement> {
        self.acknowledgement.as_ref()
    }
    pub(crate) fn map_error(mut self, map: impl FnOnce(Error) -> Error) -> Self {
        self.result = self.result.map_err(map);
        self
    }
}

impl From<Result<Completion>> for TerminalOutcome {
    fn from(result: Result<Completion>) -> Self {
        Self {
            result,
            acknowledgement: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishCompletion {
    Qos0Flushed,
    Qos1Acknowledged,
    Qos2Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrokerReason {
    pub code: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscribeResult {
    Granted(QoS),
    Rejected(BrokerReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribeCompletion {
    pub results: Vec<SubscribeResult>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnsubscribeResult {
    Success,
    NoSubscriptionExisted,
    Rejected(BrokerReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsubscribeCompletion {
    /// MQTT 3.1.1 has no per-filter UNSUBACK reasons.
    pub results: Option<Vec<UnsubscribeResult>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completion {
    Publish(PublishCompletion),
    Subscribe(SubscribeCompletion),
    Unsubscribe(UnsubscribeCompletion),
    /// The selected manual PUBACK/PUBREC flushed locally, including a negative acknowledgement.
    /// This does not prove broker receipt, application processing, or `QoS` 2 handshake completion.
    Acknowledged,
    Authenticated,
    Diagnostics(crate::DiagnosticsSnapshot),
    /// Preceding publishes completed, DISCONNECT flushed, and required persistence finished.
    OrderedShutdown,
    GracefulShutdown,
    ImmediateShutdown,
}

impl Completion {
    /// Successful authentication has no MQTT acknowledgement payload. Broker
    /// rejection is an Error with structured authentication and reason fields.
    #[must_use]
    pub const fn authentication_outcome(&self) -> Option<crate::AuthOutcome> {
        if matches!(self, Self::Authenticated) {
            Some(crate::AuthOutcome::Success)
        } else {
            None
        }
    }
}

/// Result of waiting for an operation until a caller-supplied deadline.
#[derive(Clone, Debug)]
pub enum CompletionWaitOutcome {
    /// The operation reached a terminal state, successfully or with an error.
    Completed(Result<Completion>),
    /// The operation was still pending when the wait deadline elapsed.
    DeadlineElapsed,
    /// Blocking observation was rejected; this says nothing about the operation's terminal state.
    ObservationRejected(Error),
}

#[derive(Debug)]
pub struct CompletionCell {
    operation_id: OperationId,
    result: Mutex<Option<Arc<TerminalOutcome>>>,
    completed: Condvar,
    notified: Notify,
}

impl CompletionCell {
    pub(crate) fn new(operation_id: OperationId) -> Arc<Self> {
        Arc::new(Self {
            operation_id,
            result: Mutex::new(None),
            completed: Condvar::new(),
            notified: Notify::new(),
        })
    }

    pub(crate) fn complete(&self, result: Result<Completion>) -> bool {
        self.complete_outcome(result.into())
    }

    pub(crate) fn complete_outcome(&self, outcome: TerminalOutcome) -> bool {
        let mut state = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.is_some() {
            return false;
        }
        *state = Some(Arc::new(outcome));
        drop(state);
        self.completed.notify_all();
        self.notified.notify_waiters();
        true
    }

    fn observe(&self) -> Option<Arc<TerminalOutcome>> {
        self.result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[derive(Clone, Debug)]
pub struct CompletionHandle {
    cell: Arc<CompletionCell>,
}

impl CompletionHandle {
    pub(crate) const fn new(cell: Arc<CompletionCell>) -> Self {
        Self { cell }
    }

    #[must_use]
    pub fn operation_id(&self) -> OperationId {
        self.cell.operation_id
    }

    /// Observes the retained terminal outcome without propagating its operation error.
    /// `None` means pending. Snapshots and their borrowed contents may outlive the client.
    /// Dropping a snapshot or handle never cancels admitted work.
    #[must_use]
    pub fn try_outcome(&self) -> Option<Arc<TerminalOutcome>> {
        self.cell.observe()
    }

    /// Attempts to retrieve the terminal result without blocking.
    ///
    /// A successful `None` means that the operation is still pending. Like the
    /// other wait methods, observing or dropping this waiter never cancels work
    /// that has already been admitted.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver terminates before reporting completion
    /// or when the operation itself fails.
    pub fn try_wait(&self) -> Result<Option<Completion>> {
        self.cell
            .observe()
            .map_or_else(|| Ok(None), |outcome| outcome.result.clone().map(Some))
    }

    /// Waits asynchronously for the MQTT operation to finish.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver terminates before reporting completion or the operation
    /// itself fails.
    pub async fn wait_async(&self) -> Result<Completion> {
        loop {
            let notified = self.cell.notified.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(outcome) = self.cell.observe() {
                return outcome.result.clone();
            }
            notified.await;
        }
    }

    /// Blocks until the MQTT operation finishes.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver terminates before reporting completion or the operation
    /// itself fails, or when called on an execution worker or in a host callback.
    pub fn wait(&self) -> Result<Completion> {
        crate::execution::check_wait(Duration::from_secs(1))?;
        let mut state = self
            .cell
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(outcome) = state.as_ref() {
                let outcome = outcome.clone();
                drop(state);
                return outcome.result.clone();
            }
            state = self
                .cell
                .completed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Blocks for at most `timeout` while waiting for the MQTT operation to finish.
    ///
    /// # Errors
    ///
    /// Returns an error on timeout, when the driver terminates before reporting completion, or
    /// when the operation itself fails.
    pub fn wait_timeout(&self, timeout: Duration) -> Result<Completion> {
        match self.wait_timeout_outcome(timeout) {
            CompletionWaitOutcome::Completed(result) => result,
            CompletionWaitOutcome::ObservationRejected(error) => Err(error),
            CompletionWaitOutcome::DeadlineElapsed => Err(Error::new(
                ErrorKind::Timeout,
                format!(
                    "operation {} did not complete before timeout",
                    self.operation_id().get()
                ),
            )
            .with_delivery(crate::DeliveryStatus::Ambiguous)),
        }
    }

    /// Blocks for at most `timeout`, preserving whether a timeout came from the wait deadline or
    /// from the operation's terminal result. A forbidden blocking observation returns
    /// `ObservationRejected(_)` without modifying the stored operation result.
    #[must_use]
    pub fn wait_timeout_outcome(&self, timeout: Duration) -> CompletionWaitOutcome {
        if let Err(error) = crate::execution::check_wait(timeout) {
            return CompletionWaitOutcome::ObservationRejected(error);
        }
        let started = Instant::now();
        let mut remaining = timeout;
        let mut state = self
            .cell
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(result) = state.as_ref() {
                let result = result.clone();
                drop(state);
                return CompletionWaitOutcome::Completed(result.result.clone());
            }
            let (next, wait) = self
                .cell
                .completed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if wait.timed_out() && state.is_none() {
                drop(state);
                return CompletionWaitOutcome::DeadlineElapsed;
            }
            remaining = timeout.saturating_sub(started.elapsed());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    fn handle() -> CompletionHandle {
        CompletionHandle::new(CompletionCell::new(OperationId(
            NonZeroU64::new(1).unwrap(),
        )))
    }

    #[test]
    fn rejected_callback_observation_does_not_complete_or_cancel_an_operation() {
        let handle = handle();
        crate::runtime::with_host_callback(|| {
            let CompletionWaitOutcome::ObservationRejected(error) =
                handle.wait_timeout_outcome(Duration::from_millis(1))
            else {
                panic!("blocking observation was not rejected");
            };
            assert_eq!(error.kind(), ErrorKind::Shutdown);
            assert_eq!(
                error.delivery_status(),
                crate::DeliveryStatus::NotApplicable
            );
            assert!(handle.wait().is_err());
            assert!(matches!(
                handle.wait_timeout_outcome(Duration::ZERO),
                CompletionWaitOutcome::DeadlineElapsed
            ));
            assert!(handle.try_outcome().is_none());
        });
        handle.cell.complete(Ok(Completion::Acknowledged));
        assert_eq!(
            handle.wait_timeout(Duration::from_secs(1)).unwrap(),
            Completion::Acknowledged
        );
    }

    #[test]
    fn rejected_ack_snapshots_share_storage_and_release_the_final_owner() {
        let handle = handle();
        assert!(handle.try_outcome().is_none());
        assert!(matches!(
            handle.wait_timeout_outcome(Duration::ZERO),
            CompletionWaitOutcome::DeadlineElapsed
        ));
        let outcome = crate::backend::v5::map_publish_notice(Ok(rumqttc_v5::PublishResult::Qos1(
            rumqttc_v5::PubAck {
                pkid: 37,
                reason: rumqttc_v5::PubAckReason::NotAuthorized,
                properties: Some(rumqttc_v5::PubAckProperties {
                    reason_string: Some("private-reason".into()),
                    user_properties: vec![
                        ("private-key".into(), "private-value".into()),
                        ("private-key".into(), String::new()),
                    ],
                }),
            },
        )));
        assert!(handle.cell.complete_outcome(outcome));
        assert!(!handle.cell.complete(Ok(Completion::Acknowledged)));
        let error = handle.try_wait().unwrap_err();
        assert_eq!(error.broker_reason(), Some(0x87));
        let first = handle.try_outcome().unwrap();
        let cloned_handle = handle.clone();
        let second = cloned_handle.try_outcome().unwrap();
        drop(cloned_handle);
        assert!(Arc::ptr_eq(&first, &second));
        let ack = first.acknowledgement().unwrap();
        assert_eq!(ack.packet_id(), 37);
        assert_eq!(ack.reason_code(), Some(0x87));
        let properties = ack.properties().unwrap();
        assert_eq!(properties.reason_string.as_deref(), Some("private-reason"));
        assert_eq!(
            properties.user_properties[1],
            ("private-key".into(), String::new())
        );
        for formatted in [
            format!("{first:?}"),
            format!("{handle:?}"),
            format!("{error:?}"),
            error.to_string(),
        ] {
            for secret in ["private-reason", "private-key", "private-value"] {
                assert!(!formatted.contains(secret));
            }
        }
        let weak = Arc::downgrade(&first);
        drop(handle);
        drop(first);
        assert!(weak.upgrade().is_some());
        drop(second);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn publish_ack_mapping_preserves_success_rejection_and_native_recovery() {
        use crate::backend::v5::map_publish_notice;
        use rumqttc_v5::{
            PubAck, PubAckReason, PubComp, PubCompReason, PubRec, PubRecReason, PublishResult,
        };
        for (reason, success) in [
            (PubAckReason::Success, true),
            (PubAckReason::NoMatchingSubscribers, true),
            (PubAckReason::QuotaExceeded, false),
        ] {
            let outcome = map_publish_notice(Ok(PublishResult::Qos1(PubAck {
                pkid: 5,
                reason,
                properties: None,
            })));
            assert_eq!(outcome.result().is_ok(), success);
            assert_eq!(
                outcome.acknowledgement().unwrap().reason_code(),
                Some(reason as u8)
            );
            assert!(outcome.acknowledgement().unwrap().properties().is_none());
        }
        let rejected = map_publish_notice(Ok(PublishResult::Qos2PubRecRejected(PubRec {
            pkid: 7,
            reason: PubRecReason::NotAuthorized,
            properties: Some(rumqttc_v5::PubRecProperties {
                reason_string: Some(String::new()),
                user_properties: vec![],
            }),
        })));
        assert!(rejected.result().is_err());
        let ack = rejected.acknowledgement().unwrap();
        assert_eq!(ack.kind(), AcknowledgementKind::PubRec);
        assert_eq!(ack.properties().unwrap().reason_string.as_deref(), Some(""));
        let success = map_publish_notice(Ok(PublishResult::Qos2Completed(PubComp {
            pkid: 7,
            reason: PubCompReason::Success,
            properties: Some(rumqttc_v5::PubCompProperties {
                reason_string: None,
                user_properties: vec![("k".into(), "v".into())],
            }),
        })));
        assert_eq!(
            success.result().as_ref().unwrap(),
            &Completion::Publish(PublishCompletion::Qos2Completed)
        );
        assert_eq!(success.acknowledgement().unwrap().reason_code(), Some(0));
        assert!(!success.acknowledgement().unwrap().recovered());
        assert_eq!(
            success
                .acknowledgement()
                .unwrap()
                .properties()
                .unwrap()
                .user_properties,
            [("k".into(), "v".into())]
        );
        for recovered in [false, true] {
            let packet = PubComp {
                pkid: 7,
                reason: PubCompReason::PacketIdentifierNotFound,
                properties: Some(rumqttc_v5::PubCompProperties {
                    reason_string: Some("done".into()),
                    user_properties: vec![],
                }),
            };
            let result = if recovered {
                PublishResult::Qos2Recovered(packet)
            } else {
                PublishResult::Qos2Completed(packet)
            };
            let outcome = map_publish_notice(Ok(result));
            assert_eq!(outcome.result().is_ok(), recovered);
            let ack = outcome.acknowledgement().unwrap();
            assert_eq!(ack.recovered(), recovered);
            assert_eq!(ack.reason_code(), Some(0x92));
            assert_eq!(
                ack.properties().unwrap().reason_string.as_deref(),
                Some("done")
            );
        }
        for outcome in [
            map_publish_notice(Ok(PublishResult::Qos0Flushed)),
            map_publish_notice(Err(rumqttc_v5::PublishNoticeError::SessionReset)),
        ] {
            assert!(outcome.acknowledgement().is_none());
        }
    }

    #[test]
    fn filter_ack_mapping_retains_packet_properties_and_order_for_both_protocols() {
        let outcome = crate::backend::v5::map_subscribe_notice(Ok(rumqttc_v5::SubAck {
            pkid: 17,
            return_codes: vec![
                rumqttc_v5::SubscribeReasonCode::Success(rumqttc_v5::QoS::ExactlyOnce),
                rumqttc_v5::SubscribeReasonCode::NotAuthorized,
            ],
            properties: Some(rumqttc_v5::SubAckProperties {
                reason_string: Some("packet".into()),
                user_properties: vec![("k".into(), "a".into()), ("k".into(), "b".into())],
            }),
        }));
        assert!(outcome.result().is_ok());
        let ack = outcome.acknowledgement().unwrap();
        assert_eq!(ack.filter_reason_codes(), Some([2, 0x87].as_slice()));
        assert_eq!(ack.reason_code(), None);
        assert_eq!(ack.properties().unwrap().user_properties.len(), 2);
        let outcome = crate::backend::v5::map_unsubscribe_notice(Ok(rumqttc_v5::UnsubAck {
            pkid: 18,
            reasons: vec![
                rumqttc_v5::UnsubAckReason::Success,
                rumqttc_v5::UnsubAckReason::NoSubscriptionExisted,
                rumqttc_v5::UnsubAckReason::NotAuthorized,
            ],
            properties: Some(rumqttc_v5::UnsubAckProperties {
                reason_string: None,
                user_properties: vec![],
            }),
        }));
        assert_eq!(
            outcome.acknowledgement().unwrap().filter_reason_codes(),
            Some([0, 0x11, 0x87].as_slice())
        );
        assert!(outcome.result().is_ok());
        let outcome = crate::backend::v4::map_subscribe_notice(Ok(rumqttc_v4::SubAck::new(
            19,
            vec![
                rumqttc_v4::SubscribeReasonCode::Success(rumqttc_v4::QoS::AtLeastOnce),
                rumqttc_v4::SubscribeReasonCode::Failure,
            ],
        )));
        let ack = outcome.acknowledgement().unwrap();
        assert_eq!(ack.filter_reason_codes(), Some([1, 0x80].as_slice()));
        assert_eq!(ack.protocol(), ProtocolVersion::V4);
        assert!(ack.reason_code().is_none());
        assert!(ack.properties().is_none());
        let outcome = crate::backend::v4::map_unsubscribe_notice(Ok(rumqttc_v4::UnsubAck::new(20)));
        assert!(
            outcome
                .acknowledgement()
                .unwrap()
                .filter_reason_codes()
                .is_none()
        );
    }

    #[test]
    fn timeout_outcome_distinguishes_wait_deadline_from_terminal_error() {
        assert!(matches!(
            handle().wait_timeout_outcome(Duration::ZERO),
            CompletionWaitOutcome::DeadlineElapsed
        ));

        let handle = handle();
        handle
            .cell
            .complete(Err(Error::new(ErrorKind::Timeout, "disconnect timed out")));
        let CompletionWaitOutcome::Completed(Err(error)) =
            handle.wait_timeout_outcome(Duration::ZERO)
        else {
            panic!("terminal timeout was not preserved");
        };
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(error.message(), "disconnect timed out");
    }

    #[test]
    fn completion_is_repeatable_for_clones_and_blocking_waiters() {
        let handle = handle();
        let first = handle.clone();
        let second = handle.clone();
        let first_waiter = std::thread::spawn(move || first.wait());
        let second_waiter = std::thread::spawn(move || second.wait());
        handle.cell.complete(Ok(Completion::Acknowledged));

        assert_eq!(
            first_waiter.join().unwrap().unwrap(),
            Completion::Acknowledged
        );
        assert_eq!(
            second_waiter.join().unwrap().unwrap(),
            Completion::Acknowledged
        );
        assert_eq!(handle.wait().unwrap(), Completion::Acknowledged);
        assert_eq!(handle.try_wait().unwrap(), Some(Completion::Acknowledged));
    }

    #[tokio::test]
    async fn async_waiter_cancellation_and_deadline_do_not_change_terminal_result() {
        let handle = handle();
        let cancelled = handle.clone();
        let task = tokio::spawn(async move { cancelled.wait_async().await });
        task.abort();
        assert!(matches!(
            handle.wait_timeout_outcome(Duration::ZERO),
            CompletionWaitOutcome::DeadlineElapsed
        ));

        handle
            .cell
            .complete(Err(Error::new(ErrorKind::Protocol, "rejected")));
        for observer in [handle.clone(), handle] {
            let error = observer.wait_async().await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Protocol);
            assert_eq!(error.message(), "rejected");
        }
    }

    #[tokio::test]
    async fn mixed_waiters_and_dropped_clones_share_one_result() {
        let handle = handle();
        drop(handle.clone());

        let blocking = handle.clone();
        let blocking = std::thread::spawn(move || blocking.wait());
        let async_first = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.wait_async().await })
        };
        let async_second = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.wait_async().await })
        };

        assert!(handle.cell.complete(Ok(Completion::Acknowledged)));
        assert!(!handle.cell.complete(Ok(Completion::ImmediateShutdown)));
        assert_eq!(blocking.join().unwrap().unwrap(), Completion::Acknowledged);
        assert_eq!(
            async_first.await.unwrap().unwrap(),
            Completion::Acknowledged
        );
        assert_eq!(
            async_second.await.unwrap().unwrap(),
            Completion::Acknowledged
        );

        let last = handle.clone();
        drop(handle);
        assert_eq!(last.wait().unwrap(), Completion::Acknowledged);
    }
}

#[derive(Clone, Debug)]
pub struct Admission {
    pub operation_id: OperationId,
    pub completion: CompletionHandle,
}
