use tokio::sync::oneshot;

use crate::mqttbytes::QoS;
use crate::mqttbytes::v5::{
    PubAck, PubAckReason, PubComp, PubCompReason, PubRec, PubRecReason, SubAck,
    SubscribeReasonCode as V5SubscribeReasonCode, UnsubAck, UnsubAckReason as V5UnsubAckReason,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeFailureReason {
    /// Message dropped due to session reset.
    SessionReset,
}

impl NoticeFailureReason {
    pub(crate) const fn publish_error(self) -> PublishNoticeError {
        match self {
            Self::SessionReset => PublishNoticeError::SessionReset,
        }
    }

    pub(crate) const fn subscribe_error(self) -> SubscribeNoticeError {
        match self {
            Self::SessionReset => SubscribeNoticeError::SessionReset,
        }
    }

    pub(crate) const fn unsubscribe_error(self) -> UnsubscribeNoticeError {
        match self {
            Self::SessionReset => UnsubscribeNoticeError::SessionReset,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublishResult {
    Qos0Flushed,
    Qos1(PubAck),
    Qos2Completed(PubComp),
    Qos2PubRecRejected(PubRec),
}

impl PublishResult {
    #[must_use]
    pub const fn qos(&self) -> QoS {
        match self {
            Self::Qos0Flushed => QoS::AtMostOnce,
            Self::Qos1(_) => QoS::AtLeastOnce,
            Self::Qos2Completed(_) | Self::Qos2PubRecRejected(_) => QoS::ExactlyOnce,
        }
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum PublishNoticeError {
    #[error("event loop dropped notice sender")]
    Recv,
    #[error("message dropped due to session reset")]
    SessionReset,
    #[error("publish with topic alias {0} cannot be replayed after reconnect")]
    TopicAliasReplayUnavailable(u16),
    #[error("qos0 publish was not flushed to the network")]
    Qos0NotFlushed,
    #[error("v5 puback returned non-success reason: {0:?}")]
    V5PubAck(PubAckReason),
    #[error("v5 pubrec returned non-success reason: {0:?}")]
    V5PubRec(PubRecReason),
    #[error("v5 pubcomp returned non-success reason: {0:?}")]
    V5PubComp(PubCompReason),
}

impl From<oneshot::error::RecvError> for PublishNoticeError {
    fn from(_: oneshot::error::RecvError) -> Self {
        Self::Recv
    }
}

type PublishNoticeResult = Result<PublishResult, PublishNoticeError>;
type SubscribeNoticeResult = Result<SubAck, SubscribeNoticeError>;
type UnsubscribeNoticeResult = Result<UnsubAck, UnsubscribeNoticeError>;

/// Wait handle returned by tracked publish APIs.
#[derive(Debug)]
pub struct PublishNotice(pub(crate) oneshot::Receiver<PublishNoticeResult>);

impl PublishNotice {
    /// Wait for the publish protocol result by blocking the current thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop drops the notice sender or if the
    /// publish fails before a protocol result is available.
    ///
    /// # Panics
    ///
    /// Panics if called in an async context.
    pub fn wait(self) -> PublishNoticeResult {
        self.0.blocking_recv()?
    }

    /// Wait for the publish protocol result asynchronously.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop drops the notice sender or if the
    /// publish fails before a protocol result is available.
    pub async fn wait_async(self) -> PublishNoticeResult {
        self.0.await?
    }

    /// Wait for publish completion and map broker rejection reasons to
    /// completion errors.
    ///
    /// # Errors
    ///
    /// Returns an error if the publish fails before a protocol result, or if the
    /// terminal protocol result reports a failing reason.
    ///
    /// # Panics
    ///
    /// Panics if called in an async context.
    pub fn wait_completion(self) -> Result<(), PublishNoticeError> {
        validate_v5_publish_completion(&self.wait()?)
    }

    /// Wait asynchronously for publish completion and map broker rejection
    /// reasons to completion errors.
    ///
    /// # Errors
    ///
    /// Returns an error if the publish fails before a protocol result, or if the
    /// terminal protocol result reports a failing reason.
    pub async fn wait_completion_async(self) -> Result<(), PublishNoticeError> {
        validate_v5_publish_completion(&self.wait_async().await?)
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum SubscribeNoticeError {
    #[error("event loop dropped notice sender")]
    Recv,
    #[error("message dropped due to session reset")]
    SessionReset,
    #[error("v5 suback returned failing reason codes: {0:?}")]
    V5SubAckFailure(Vec<V5SubscribeReasonCode>),
}

impl From<oneshot::error::RecvError> for SubscribeNoticeError {
    fn from(_: oneshot::error::RecvError) -> Self {
        Self::Recv
    }
}

/// Wait handle returned by tracked subscribe APIs.
#[derive(Debug)]
pub struct SubscribeNotice(pub(crate) oneshot::Receiver<SubscribeNoticeResult>);

impl SubscribeNotice {
    /// Wait for `SubAck` by blocking the current thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop drops the notice sender or if the
    /// subscribe fails before a `SubAck` is available.
    ///
    /// # Panics
    ///
    /// Panics if called in an async context.
    pub fn wait(self) -> SubscribeNoticeResult {
        self.0.blocking_recv()?
    }

    /// Wait for `SubAck` asynchronously.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop drops the notice sender or if the
    /// subscribe fails before a `SubAck` is available.
    pub async fn wait_async(self) -> SubscribeNoticeResult {
        self.0.await?
    }

    /// Wait for subscribe completion and treat failing `SubAck` return codes as
    /// completion errors.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscribe fails before `SubAck`, or if `SubAck`
    /// contains failing return codes.
    ///
    /// # Panics
    ///
    /// Panics if called in an async context.
    pub fn wait_completion(self) -> Result<(), SubscribeNoticeError> {
        validate_v5_suback_completion(&self.wait()?)
    }

    /// Wait asynchronously for subscribe completion and treat failing `SubAck`
    /// return codes as completion errors.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscribe fails before `SubAck`, or if `SubAck`
    /// contains failing return codes.
    pub async fn wait_completion_async(self) -> Result<(), SubscribeNoticeError> {
        validate_v5_suback_completion(&self.wait_async().await?)
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum UnsubscribeNoticeError {
    #[error("event loop dropped notice sender")]
    Recv,
    #[error("message dropped due to session reset")]
    SessionReset,
    #[error("v5 unsuback returned failing reason codes: {0:?}")]
    V5UnsubAckFailure(Vec<V5UnsubAckReason>),
}

impl From<oneshot::error::RecvError> for UnsubscribeNoticeError {
    fn from(_: oneshot::error::RecvError) -> Self {
        Self::Recv
    }
}

/// Wait handle returned by tracked unsubscribe APIs.
#[derive(Debug)]
pub struct UnsubscribeNotice(pub(crate) oneshot::Receiver<UnsubscribeNoticeResult>);

impl UnsubscribeNotice {
    /// Wait for `UnsubAck` by blocking the current thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop drops the notice sender or if the
    /// unsubscribe fails before an `UnsubAck` is available.
    ///
    /// # Panics
    ///
    /// Panics if called in an async context.
    pub fn wait(self) -> UnsubscribeNoticeResult {
        self.0.blocking_recv()?
    }

    /// Wait for `UnsubAck` asynchronously.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop drops the notice sender or if the
    /// unsubscribe fails before an `UnsubAck` is available.
    pub async fn wait_async(self) -> UnsubscribeNoticeResult {
        self.0.await?
    }

    /// Wait for unsubscribe completion and treat failing `UnsubAck` reasons as
    /// completion errors.
    ///
    /// # Errors
    ///
    /// Returns an error if the unsubscribe fails before `UnsubAck`, or if
    /// `UnsubAck` contains failing reasons.
    ///
    /// # Panics
    ///
    /// Panics if called in an async context.
    pub fn wait_completion(self) -> Result<(), UnsubscribeNoticeError> {
        validate_v5_unsuback_completion(&self.wait()?)
    }

    /// Wait asynchronously for unsubscribe completion and treat failing
    /// `UnsubAck` reasons as completion errors.
    ///
    /// # Errors
    ///
    /// Returns an error if the unsubscribe fails before `UnsubAck`, or if
    /// `UnsubAck` contains failing reasons.
    pub async fn wait_completion_async(self) -> Result<(), UnsubscribeNoticeError> {
        validate_v5_unsuback_completion(&self.wait_async().await?)
    }
}

#[derive(Debug)]
pub struct PublishNoticeTx(pub(crate) oneshot::Sender<PublishNoticeResult>);

impl PublishNoticeTx {
    pub(crate) fn new() -> (Self, PublishNotice) {
        let (tx, rx) = oneshot::channel();
        (Self(tx), PublishNotice(rx))
    }

    pub(crate) fn success(self, result: PublishResult) {
        _ = self.0.send(Ok(result));
    }

    pub(crate) fn error(self, err: PublishNoticeError) {
        _ = self.0.send(Err(err));
    }
}

#[derive(Debug)]
pub struct SubscribeNoticeTx(pub(crate) oneshot::Sender<SubscribeNoticeResult>);

impl SubscribeNoticeTx {
    pub(crate) fn new() -> (Self, SubscribeNotice) {
        let (tx, rx) = oneshot::channel();
        (Self(tx), SubscribeNotice(rx))
    }

    pub(crate) fn success(self, suback: SubAck) {
        _ = self.0.send(Ok(suback));
    }

    pub(crate) fn error(self, err: SubscribeNoticeError) {
        _ = self.0.send(Err(err));
    }
}

#[derive(Debug)]
pub struct UnsubscribeNoticeTx(pub(crate) oneshot::Sender<UnsubscribeNoticeResult>);

impl UnsubscribeNoticeTx {
    pub(crate) fn new() -> (Self, UnsubscribeNotice) {
        let (tx, rx) = oneshot::channel();
        (Self(tx), UnsubscribeNotice(rx))
    }

    pub(crate) fn success(self, unsuback: UnsubAck) {
        _ = self.0.send(Ok(unsuback));
    }

    pub(crate) fn error(self, err: UnsubscribeNoticeError) {
        _ = self.0.send(Err(err));
    }
}

#[derive(Debug)]
pub enum TrackedNoticeTx {
    Publish(PublishNoticeTx),
    Subscribe(SubscribeNoticeTx),
    Unsubscribe(UnsubscribeNoticeTx),
}

fn validate_v5_publish_completion(result: &PublishResult) -> Result<(), PublishNoticeError> {
    match result {
        PublishResult::Qos0Flushed => Ok(()),
        PublishResult::Qos1(puback)
            if puback.reason == PubAckReason::Success
                || puback.reason == PubAckReason::NoMatchingSubscribers =>
        {
            Ok(())
        }
        PublishResult::Qos1(puback) => Err(PublishNoticeError::V5PubAck(puback.reason)),
        PublishResult::Qos2Completed(pubcomp) if pubcomp.reason == PubCompReason::Success => Ok(()),
        PublishResult::Qos2Completed(pubcomp) => Err(PublishNoticeError::V5PubComp(pubcomp.reason)),
        PublishResult::Qos2PubRecRejected(pubrec) => {
            Err(PublishNoticeError::V5PubRec(pubrec.reason))
        }
    }
}

fn validate_v5_suback_completion(suback: &SubAck) -> Result<(), SubscribeNoticeError> {
    let failures: Vec<_> = suback
        .return_codes
        .iter()
        .filter(|reason| !matches!(reason, V5SubscribeReasonCode::Success(_)))
        .copied()
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(SubscribeNoticeError::V5SubAckFailure(failures))
    }
}

fn validate_v5_unsuback_completion(unsuback: &UnsubAck) -> Result<(), UnsubscribeNoticeError> {
    let failures: Vec<_> = unsuback
        .reasons
        .iter()
        .filter(|reason| {
            **reason != V5UnsubAckReason::Success
                && **reason != V5UnsubAckReason::NoSubscriptionExisted
        })
        .copied()
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(UnsubscribeNoticeError::V5UnsubAckFailure(failures))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_publish_wait_returns_result() {
        let (tx, notice) = PublishNoticeTx::new();
        tx.success(PublishResult::Qos0Flushed);
        assert_eq!(notice.wait(), Ok(PublishResult::Qos0Flushed));
    }

    #[tokio::test]
    async fn async_publish_wait_returns_error() {
        let (tx, notice) = PublishNoticeTx::new();
        tx.error(PublishNoticeError::SessionReset);
        let err = notice.wait_async().await.unwrap_err();
        assert_eq!(err, PublishNoticeError::SessionReset);
    }

    #[test]
    fn publish_completion_fails_on_rejected_puback() {
        let (tx, notice) = PublishNoticeTx::new();
        let mut puback = PubAck::new(1, None);
        puback.reason = PubAckReason::ImplementationSpecificError;
        tx.success(PublishResult::Qos1(puback));
        assert_eq!(
            notice.wait_completion(),
            Err(PublishNoticeError::V5PubAck(
                PubAckReason::ImplementationSpecificError
            ))
        );
    }

    #[test]
    fn blocking_subscribe_wait_returns_suback() {
        let (tx, notice) = SubscribeNoticeTx::new();
        let suback = SubAck {
            pkid: 1,
            return_codes: vec![V5SubscribeReasonCode::Success(QoS::AtLeastOnce)],
            properties: None,
        };
        tx.success(suback.clone());
        assert_eq!(notice.wait(), Ok(suback));
    }

    #[test]
    fn subscribe_completion_fails_on_failure_return_code() {
        let (tx, notice) = SubscribeNoticeTx::new();
        tx.success(SubAck {
            pkid: 1,
            return_codes: vec![V5SubscribeReasonCode::Unspecified],
            properties: None,
        });
        assert_eq!(
            notice.wait_completion(),
            Err(SubscribeNoticeError::V5SubAckFailure(vec![
                V5SubscribeReasonCode::Unspecified
            ]))
        );
    }

    #[tokio::test]
    async fn async_unsubscribe_wait_returns_error() {
        let (tx, notice) = UnsubscribeNoticeTx::new();
        tx.error(UnsubscribeNoticeError::SessionReset);
        let err = notice.wait_async().await.unwrap_err();
        assert_eq!(err, UnsubscribeNoticeError::SessionReset);
    }
}
