//! Configuration for disconnect admission and ordered completion.

use std::time::Duration;

use crate::{Disconnect, DisconnectProperties, DisconnectReasonCode, Request};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectOptions {
    policy: DisconnectPolicy,
    reason: DisconnectReasonCode,
    properties: Option<DisconnectProperties>,
}

impl DisconnectOptions {
    /// Drains work already admitted into MQTT protocol state before DISCONNECT.
    /// Earlier queued publishes may be overtaken under flow control.
    #[must_use]
    pub const fn graceful() -> Self {
        Self {
            policy: DisconnectPolicy::Graceful,
            reason: DisconnectReasonCode::NormalDisconnection,
            properties: None,
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

    /// Sets the MQTT 5 reason code without changing the properties.
    #[must_use]
    pub const fn reason(mut self, reason: DisconnectReasonCode) -> Self {
        self.reason = reason;
        self
    }

    /// Sets the MQTT 5 properties without changing the reason code.
    #[must_use]
    pub fn properties(mut self, properties: DisconnectProperties) -> Self {
        self.properties = Some(properties);
        self
    }

    pub(crate) fn into_request(self) -> Request {
        let packet = disconnect_packet(self.reason, self.properties);
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedDisconnectOptions {
    timeout: Option<Duration>,
    reason: DisconnectReasonCode,
    properties: Option<DisconnectProperties>,
}

#[cfg(feature = "ordered-shutdown")]
impl OrderedDisconnectOptions {
    /// Completes preceding publishes with no deadline.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            timeout: None,
            reason: DisconnectReasonCode::NormalDisconnection,
            properties: None,
        }
    }

    /// Sets the total deadline duration, starting at successful admission.
    /// Waiting for request-channel capacity is excluded; zero expires immediately.
    /// Overflow is rejected when the configured request is submitted.
    #[must_use]
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets the MQTT 5 reason code without changing the properties.
    #[must_use]
    pub const fn reason(mut self, reason: DisconnectReasonCode) -> Self {
        self.reason = reason;
        self
    }

    /// Sets the MQTT 5 properties without changing the reason code.
    #[must_use]
    pub fn properties(mut self, properties: DisconnectProperties) -> Self {
        self.properties = Some(properties);
        self
    }

    pub(crate) fn into_parts(self) -> (Disconnect, Option<Duration>) {
        (
            disconnect_packet(self.reason, self.properties),
            self.timeout,
        )
    }
}

#[cfg(feature = "ordered-shutdown")]
impl Default for OrderedDisconnectOptions {
    fn default() -> Self {
        Self::new()
    }
}

fn disconnect_packet(
    reason: DisconnectReasonCode,
    properties: Option<DisconnectProperties>,
) -> Disconnect {
    properties.map_or_else(
        || Disconnect::new(reason),
        |properties| Disconnect::new_with_properties(reason, properties),
    )
}
