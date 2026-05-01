use tokio::sync::oneshot;

use crate::mqttbytes::QoS;
use crate::mqttbytes::v4::{
    PubAck, PubComp, SubAck, SubscribeReasonCode as V4SubscribeReasonCode, UnsubAck,
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
}

impl PublishResult {
    #[must_use]
    pub const fn qos(&self) -> QoS {
        match self {
            Self::Qos0Flushed => QoS::AtMostOnce,
            Self::Qos1(_) => QoS::AtLeastOnce,
            Self::Qos2Completed(_) => QoS::ExactlyOnce,
        }
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum PublishNoticeError {
    #[error("event loop dropped notice sender")]
    Recv,
    #[error("message dropped due to session reset")]
    SessionReset,
    #[error("qos0 publish was not flushed to the network")]
    Qos0NotFlushed,
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

    /// Wait for publish completion while discarding the detailed protocol result.
    ///
    /// # Errors
    ///
    /// Returns an error if the publish fails before completion.
    ///
    /// # Panics
    ///
    /// Panics if called in an async context.
    pub fn wait_completion(self) -> Result<(), PublishNoticeError> {
        self.wait().map(drop)
    }

    /// Wait asynchronously for publish completion while discarding the detailed
    /// protocol result.
    ///
    /// # Errors
    ///
    /// Returns an error if the publish fails before completion.
    pub async fn wait_completion_async(self) -> Result<(), PublishNoticeError> {
        self.wait_async().await.map(drop)
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum SubscribeNoticeError {
    #[error("event loop dropped notice sender")]
    Recv,
    #[error("message dropped due to session reset")]
    SessionReset,
    #[error("v4 suback returned failing reason codes: {0:?}")]
    V4SubAckFailure(Vec<V4SubscribeReasonCode>),
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
        validate_v4_suback_completion(&self.wait()?)
    }

    /// Wait asynchronously for subscribe completion and treat failing `SubAck`
    /// return codes as completion errors.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscribe fails before `SubAck`, or if `SubAck`
    /// contains failing return codes.
    pub async fn wait_completion_async(self) -> Result<(), SubscribeNoticeError> {
        validate_v4_suback_completion(&self.wait_async().await?)
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum UnsubscribeNoticeError {
    #[error("event loop dropped notice sender")]
    Recv,
    #[error("message dropped due to session reset")]
    SessionReset,
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

    /// Wait for unsubscribe completion while discarding the `UnsubAck`.
    ///
    /// # Errors
    ///
    /// Returns an error if the unsubscribe fails before `UnsubAck`.
    ///
    /// # Panics
    ///
    /// Panics if called in an async context.
    pub fn wait_completion(self) -> Result<(), UnsubscribeNoticeError> {
        self.wait().map(drop)
    }

    /// Wait asynchronously for unsubscribe completion while discarding the
    /// `UnsubAck`.
    ///
    /// # Errors
    ///
    /// Returns an error if the unsubscribe fails before `UnsubAck`.
    pub async fn wait_completion_async(self) -> Result<(), UnsubscribeNoticeError> {
        self.wait_async().await.map(drop)
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

fn validate_v4_suback_completion(suback: &SubAck) -> Result<(), SubscribeNoticeError> {
    let failures: Vec<_> = suback
        .return_codes
        .iter()
        .copied()
        .filter(|code| matches!(code, V4SubscribeReasonCode::Failure))
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(SubscribeNoticeError::V4SubAckFailure(failures))
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
    fn blocking_subscribe_wait_returns_suback() {
        let (tx, notice) = SubscribeNoticeTx::new();
        let suback = SubAck::new(1, vec![V4SubscribeReasonCode::Success(QoS::AtLeastOnce)]);
        tx.success(suback.clone());
        assert_eq!(notice.wait(), Ok(suback));
    }

    #[test]
    fn subscribe_completion_fails_on_failure_return_code() {
        let (tx, notice) = SubscribeNoticeTx::new();
        tx.success(SubAck::new(1, vec![V4SubscribeReasonCode::Failure]));
        assert_eq!(
            notice.wait_completion(),
            Err(SubscribeNoticeError::V4SubAckFailure(vec![
                V4SubscribeReasonCode::Failure
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
