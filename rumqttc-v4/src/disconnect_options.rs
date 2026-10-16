//! Configuration for disconnect admission and ordered completion.

use std::time::Duration;

use crate::{Disconnect, Request};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisconnectPolicy {
    Graceful,
    GracefulWithTimeout(Duration),
    Immediate,
}

/// Configuration for a graceful or immediate disconnect.
///
/// Ordinary calls can keep using `client.disconnect()`,
/// `client.disconnect_with_timeout(timeout)`, or `client.disconnect_now()`.
/// Configured calls confirm request admission, not transport flush.
///
/// ```no_run
/// use rumqttc::{AsyncClient, ClientError, DisconnectOptions};
/// use std::time::Duration;
///
/// # async fn shutdown(client: &AsyncClient) -> Result<(), ClientError> {
/// client.disconnect_with(
///     DisconnectOptions::graceful_with_timeout(Duration::from_secs(5)),
/// ).await?;
/// # Ok(())
/// # }
/// ```
///
/// Immediate disconnects cannot be given a drain timeout:
///
/// ```compile_fail
/// use rumqttc::DisconnectOptions;
/// use std::time::Duration;
/// let options = DisconnectOptions::immediate().timeout(Duration::from_secs(5));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisconnectOptions {
    policy: DisconnectPolicy,
}

impl DisconnectOptions {
    /// Drains work already admitted into MQTT protocol state before DISCONNECT.
    /// Earlier queued publishes may be overtaken under flow control.
    #[must_use]
    pub const fn graceful() -> Self {
        Self {
            policy: DisconnectPolicy::Graceful,
        }
    }

    /// Drains protocol-admitted work with a timeout starting at event-loop observation.
    #[must_use]
    pub const fn graceful_with_timeout(timeout: Duration) -> Self {
        let mut options = Self::graceful();
        options.policy = DisconnectPolicy::GracefulWithTimeout(timeout);
        options
    }

    /// Sends DISCONNECT without waiting for outstanding publish handshakes.
    #[must_use]
    pub const fn immediate() -> Self {
        let mut options = Self::graceful();
        options.policy = DisconnectPolicy::Immediate;
        options
    }

    pub(crate) const fn into_request(self) -> Request {
        let packet = Disconnect;
        match self.policy {
            DisconnectPolicy::Graceful => Request::Disconnect(packet),
            DisconnectPolicy::GracefulWithTimeout(timeout) => {
                Request::DisconnectWithTimeout(packet, timeout)
            }
            DisconnectPolicy::Immediate => Request::DisconnectNow(packet),
        }
    }
}

impl Default for DisconnectOptions {
    fn default() -> Self {
        Self::graceful()
    }
}

/// Configuration for completing preceding queued/replayed publishes before DISCONNECT.
///
/// The configured method returns a separately awaited [`crate::DisconnectNotice`].
/// Its success confirms publish completion, DISCONNECT flush, and required persistence.
/// Keep driving the event loop while waiting. No timeout is configured by default.
///
/// ```no_run
/// use rumqttc::{AsyncClient, OrderedDisconnectOptions};
/// use std::time::Duration;
///
/// # async fn shutdown(client: &AsyncClient) -> Result<(), Box<dyn std::error::Error>> {
/// let notice = client.disconnect_after_queued_with(
///     OrderedDisconnectOptions::new().timeout(Duration::from_secs(5)),
/// ).await?;
/// notice.wait_async().await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "ordered-shutdown")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderedDisconnectOptions {
    timeout: Option<Duration>,
}

#[cfg(feature = "ordered-shutdown")]
impl OrderedDisconnectOptions {
    /// Completes preceding publishes with no deadline.
    #[must_use]
    pub const fn new() -> Self {
        Self { timeout: None }
    }

    /// Sets the total deadline duration, starting at successful admission.
    /// Waiting for request-channel capacity is excluded; zero expires immediately.
    /// Overflow is rejected when the configured request is submitted.
    #[must_use]
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub(crate) const fn into_parts(self) -> (Disconnect, Option<Duration>) {
        (Disconnect, self.timeout)
    }
}

#[cfg(feature = "ordered-shutdown")]
impl Default for OrderedDisconnectOptions {
    fn default() -> Self {
        Self::new()
    }
}
