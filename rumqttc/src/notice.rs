use tokio::sync::oneshot;

use crate::mqttbytes::v4::SubscribeReasonCode as V4SubscribeReasonCode;
use crate::v5::mqttbytes::v5::{PubAckReason, PubCompReason, PubRecReason};
use crate::v5::mqttbytes::v5::{
    SubscribeReasonCode as V5SubscribeReasonCode, UnsubAckReason as V5UnsubAckReason,
};

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum PublishNoticeError {
    #[error("event loop dropped notice sender")]
    Recv,
    #[error("message dropped due to session reset")]
    SessionReset,
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

type PublishNoticeResult = Result<(), PublishNoticeError>;
type RequestNoticeResult = Result<(), RequestNoticeError>;

/// Wait handle returned by tracked publish APIs.
#[derive(Debug)]
pub struct PublishNotice(pub(crate) oneshot::Receiver<PublishNoticeResult>);

impl PublishNotice {
    /// Wait for publish completion by blocking the current thread.
    ///
    /// # Panics
    /// Panics if called in an async context.
    pub fn wait(self) -> PublishNoticeResult {
        self.0.blocking_recv()?
    }

    /// Wait for publish completion asynchronously.
    pub async fn wait_async(self) -> PublishNoticeResult {
        self.0.await?
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RequestNoticeError {
    #[error("event loop dropped notice sender")]
    Recv,
    #[error("message dropped due to session reset")]
    SessionReset,
    #[error("v4 suback returned failing reason codes: {0:?}")]
    V4SubAckFailure(Vec<V4SubscribeReasonCode>),
    #[error("v5 suback returned failing reason codes: {0:?}")]
    V5SubAckFailure(Vec<V5SubscribeReasonCode>),
    #[error("v5 unsuback returned failing reason codes: {0:?}")]
    V5UnsubAckFailure(Vec<V5UnsubAckReason>),
}

impl From<oneshot::error::RecvError> for RequestNoticeError {
    fn from(_: oneshot::error::RecvError) -> Self {
        Self::Recv
    }
}

/// Wait handle returned by tracked subscribe/unsubscribe APIs.
#[derive(Debug)]
pub struct RequestNotice(pub(crate) oneshot::Receiver<RequestNoticeResult>);

impl RequestNotice {
    /// Wait for request completion by blocking the current thread.
    ///
    /// # Panics
    /// Panics if called in an async context.
    pub fn wait(self) -> RequestNoticeResult {
        self.0.blocking_recv()?
    }

    /// Wait for request completion asynchronously.
    pub async fn wait_async(self) -> RequestNoticeResult {
        self.0.await?
    }
}

#[derive(Debug)]
pub(crate) struct PublishNoticeTx(pub(crate) oneshot::Sender<PublishNoticeResult>);

impl PublishNoticeTx {
    pub(crate) fn new() -> (Self, PublishNotice) {
        let (tx, rx) = oneshot::channel();
        (Self(tx), PublishNotice(rx))
    }

    pub(crate) fn success(self) {
        _ = self.0.send(Ok(()));
    }

    pub(crate) fn error(self, err: PublishNoticeError) {
        _ = self.0.send(Err(err));
    }
}

#[derive(Debug)]
pub(crate) struct RequestNoticeTx(pub(crate) oneshot::Sender<RequestNoticeResult>);

impl RequestNoticeTx {
    pub(crate) fn new() -> (Self, RequestNotice) {
        let (tx, rx) = oneshot::channel();
        (Self(tx), RequestNotice(rx))
    }

    pub(crate) fn success(self) {
        _ = self.0.send(Ok(()));
    }

    pub(crate) fn error(self, err: RequestNoticeError) {
        _ = self.0.send(Err(err));
    }
}

#[derive(Debug)]
pub(crate) enum TrackedNoticeTx {
    Publish(PublishNoticeTx),
    Request(RequestNoticeTx),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_wait_returns_success() {
        let (tx, notice) = PublishNoticeTx::new();
        tx.success();
        assert!(notice.wait().is_ok());
    }

    #[tokio::test]
    async fn async_wait_returns_error() {
        let (tx, notice) = PublishNoticeTx::new();
        tx.error(PublishNoticeError::SessionReset);
        let err = notice.wait_async().await.unwrap_err();
        assert_eq!(err, PublishNoticeError::SessionReset);
    }

    #[test]
    fn blocking_request_wait_returns_success() {
        let (tx, notice) = RequestNoticeTx::new();
        tx.success();
        assert!(notice.wait().is_ok());
    }

    #[tokio::test]
    async fn async_request_wait_returns_error() {
        let (tx, notice) = RequestNoticeTx::new();
        tx.error(RequestNoticeError::SessionReset);
        let err = notice.wait_async().await.unwrap_err();
        assert_eq!(err, RequestNoticeError::SessionReset);
    }
}
