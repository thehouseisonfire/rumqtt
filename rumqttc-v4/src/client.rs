//! This module offers a high level synchronous and asynchronous abstraction to
//! async eventloop.
use std::borrow::Cow;
use std::time::Duration;

use crate::eventloop::{RequestChannelCapacity, RequestEnvelope};
use crate::mqttbytes::{
    QoS,
    v4::{Disconnect, PubAck, PubRec, Publish, Subscribe, SubscribeFilter, Unsubscribe},
};
use crate::notice::{PublishNoticeTx, SubscribeNoticeTx, UnsubscribeNoticeTx};
use crate::{
    ConfigError, ConnectionError, Event, EventLoop, MqttOptions, PublishNotice, Request,
    SubscribeNotice, UnsubscribeNotice, valid_filter, valid_topic,
};

use bytes::Bytes;
use flume::{SendError, Sender, TrySendError};
use futures_util::FutureExt;
use tokio::runtime::{self, Runtime};
use tokio::time::timeout;

/// An error returned when a topic string fails validation against the MQTT specification.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("Invalid MQTT topic: '{0}'")]
pub struct InvalidTopic(String);

/// A newtype wrapper that guarantees its inner `String` is a valid MQTT topic.
///
/// This type prevents the cost of repeated validation for topics that are used
/// frequently. It can only be constructed via [`ValidatedTopic::new`], which
/// performs a one-time validation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedTopic(String);

impl ValidatedTopic {
    /// Constructs a new `ValidatedTopic` after validating the input string.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidTopic`] if the topic string does not conform to the MQTT specification.
    pub fn new<S: Into<String>>(topic: S) -> Result<Self, InvalidTopic> {
        let topic_string = topic.into();
        if valid_publish_topic(&topic_string) {
            Ok(Self(topic_string))
        } else {
            Err(InvalidTopic(topic_string))
        }
    }
}

impl From<ValidatedTopic> for String {
    fn from(topic: ValidatedTopic) -> Self {
        topic.0
    }
}

/// Topic argument accepted by publish APIs.
///
/// `ValidatedTopic` variants skip per-call validation while string variants are
/// validated for MQTT topic correctness.
pub enum PublishTopic {
    /// Raw topic input that must be validated before publishing.
    Unvalidated(String),
    /// Topic that has already been validated once.
    Validated(ValidatedTopic),
}

impl PublishTopic {
    fn into_string_and_validation(self) -> (String, bool) {
        match self {
            Self::Unvalidated(topic) => (topic, true),
            Self::Validated(topic) => (topic.0, false),
        }
    }
}

impl From<ValidatedTopic> for PublishTopic {
    fn from(topic: ValidatedTopic) -> Self {
        Self::Validated(topic)
    }
}

impl From<String> for PublishTopic {
    fn from(topic: String) -> Self {
        Self::Unvalidated(topic)
    }
}

impl From<&str> for PublishTopic {
    fn from(topic: &str) -> Self {
        Self::Unvalidated(topic.to_owned())
    }
}

impl From<&String> for PublishTopic {
    fn from(topic: &String) -> Self {
        Self::Unvalidated(topic.clone())
    }
}

impl From<Cow<'_, str>> for PublishTopic {
    fn from(topic: Cow<'_, str>) -> Self {
        Self::Unvalidated(topic.into_owned())
    }
}

/// An error returned when a topic filter string fails validation against the MQTT specification.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("Invalid MQTT topic filter: '{0}'")]
pub struct InvalidTopicFilter(String);

/// A newtype wrapper that guarantees its inner `String` is a valid MQTT topic filter.
///
/// This type prevents the cost of repeated validation for topic filters that are used
/// frequently. It can only be constructed via [`ValidatedTopicFilter::new`], which
/// performs a one-time validation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedTopicFilter(String);

impl ValidatedTopicFilter {
    /// Constructs a new `ValidatedTopicFilter` after validating the input string.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidTopicFilter`] if the topic filter string does not conform to the MQTT specification.
    pub fn new<S: Into<String>>(filter: S) -> Result<Self, InvalidTopicFilter> {
        let filter_string = filter.into();
        if valid_topic_filter(&filter_string) {
            Ok(Self(filter_string))
        } else {
            Err(InvalidTopicFilter(filter_string))
        }
    }
}

impl From<ValidatedTopicFilter> for String {
    fn from(filter: ValidatedTopicFilter) -> Self {
        filter.0
    }
}

/// Topic filter argument accepted by subscribe and unsubscribe APIs.
///
/// `ValidatedTopicFilter` variants skip per-call validation while string variants are
/// validated for MQTT topic filter correctness.
pub enum TopicFilter {
    /// Raw topic filter input that must be validated before subscribing or unsubscribing.
    Unvalidated(String),
    /// Topic filter that has already been validated once.
    Validated(ValidatedTopicFilter),
}

impl TopicFilter {
    fn into_string_and_validation(self) -> (String, bool) {
        match self {
            Self::Unvalidated(filter) => (filter, true),
            Self::Validated(filter) => (filter.0, false),
        }
    }
}

impl From<ValidatedTopicFilter> for TopicFilter {
    fn from(filter: ValidatedTopicFilter) -> Self {
        Self::Validated(filter)
    }
}

impl From<String> for TopicFilter {
    fn from(filter: String) -> Self {
        Self::Unvalidated(filter)
    }
}

impl From<&str> for TopicFilter {
    fn from(filter: &str) -> Self {
        Self::Unvalidated(filter.to_owned())
    }
}

impl From<&String> for TopicFilter {
    fn from(filter: &String) -> Self {
        Self::Unvalidated(filter.clone())
    }
}

impl From<Cow<'_, str>> for TopicFilter {
    fn from(filter: Cow<'_, str>) -> Self {
        Self::Unvalidated(filter.into_owned())
    }
}

/// Topic filter argument accepted by multi-subscribe APIs.
///
/// Raw `SubscribeFilter` values are validated before use. Filters built from
/// `ValidatedTopicFilter` skip repeated topic-filter validation.
pub struct SubscribeFilterInput {
    inner: SubscribeFilterInputKind,
}

enum SubscribeFilterInputKind {
    Unvalidated(SubscribeFilter),
    Validated(SubscribeFilter),
}

impl SubscribeFilterInput {
    /// Creates an unvalidated subscribe filter input.
    #[must_use]
    pub fn new<S: Into<String>>(filter: S, qos: QoS) -> Self {
        Self {
            inner: SubscribeFilterInputKind::Unvalidated(SubscribeFilter::new(filter.into(), qos)),
        }
    }

    /// Creates a subscribe filter input from an already validated topic filter.
    #[must_use]
    pub fn validated(filter: ValidatedTopicFilter, qos: QoS) -> Self {
        Self {
            inner: SubscribeFilterInputKind::Validated(SubscribeFilter::new(filter.0, qos)),
        }
    }

    fn into_filter_and_validation(self) -> (SubscribeFilter, bool) {
        match self.inner {
            SubscribeFilterInputKind::Unvalidated(filter) => (filter, true),
            SubscribeFilterInputKind::Validated(filter) => (filter, false),
        }
    }
}

impl From<SubscribeFilter> for SubscribeFilterInput {
    fn from(filter: SubscribeFilter) -> Self {
        Self {
            inner: SubscribeFilterInputKind::Unvalidated(filter),
        }
    }
}

/// Options for publishing an MQTT message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishOptions {
    qos: QoS,
    retain: bool,
}

impl PublishOptions {
    /// Creates publish options with the given `QoS` and a non-retained message.
    #[must_use]
    pub const fn new(qos: QoS) -> Self {
        Self { qos, retain: false }
    }

    /// Creates publish options for a `QoS` 0 message.
    #[must_use]
    pub const fn at_most_once() -> Self {
        Self::new(QoS::AtMostOnce)
    }

    /// Creates publish options for a `QoS` 1 message.
    #[must_use]
    pub const fn at_least_once() -> Self {
        Self::new(QoS::AtLeastOnce)
    }

    /// Creates publish options for a `QoS` 2 message.
    #[must_use]
    pub const fn exactly_once() -> Self {
        Self::new(QoS::ExactlyOnce)
    }

    /// Marks the publish as retained by the broker.
    #[must_use]
    pub const fn retained(mut self) -> Self {
        self.retain = true;
        self
    }

    /// Marks the publish as not retained by the broker.
    #[must_use]
    pub const fn not_retained(mut self) -> Self {
        self.retain = false;
        self
    }

    const fn qos(self) -> QoS {
        self.qos
    }

    const fn retain(self) -> bool {
        self.retain
    }
}

/// Payload argument accepted by publish APIs.
///
/// Owned payloads are converted without copying where possible. Borrowed
/// payloads are copied into the queued publish packet.
pub trait IntoPublishPayload {
    /// Converts the payload into bytes owned by the publish packet.
    fn into_publish_payload(self) -> Bytes;
}

impl IntoPublishPayload for Bytes {
    fn into_publish_payload(self) -> Bytes {
        self
    }
}

impl IntoPublishPayload for Vec<u8> {
    fn into_publish_payload(self) -> Bytes {
        Bytes::from(self)
    }
}

impl IntoPublishPayload for String {
    fn into_publish_payload(self) -> Bytes {
        Bytes::from(self)
    }
}

impl IntoPublishPayload for &'_ [u8] {
    fn into_publish_payload(self) -> Bytes {
        Bytes::copy_from_slice(self)
    }
}

impl<const N: usize> IntoPublishPayload for &'_ [u8; N] {
    fn into_publish_payload(self) -> Bytes {
        Bytes::copy_from_slice(self)
    }
}

impl<const N: usize> IntoPublishPayload for [u8; N] {
    fn into_publish_payload(self) -> Bytes {
        Bytes::from(Vec::from(self))
    }
}

impl IntoPublishPayload for &'_ str {
    fn into_publish_payload(self) -> Bytes {
        Bytes::copy_from_slice(self.as_bytes())
    }
}

/// Client Error
#[non_exhaustive]
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClientError {
    #[error("Request channel is full")]
    RequestChannelFull(Box<Request>),
    #[error("Request channel is disconnected")]
    RequestChannelDisconnected(Box<Request>),
    #[error("Invalid MQTT request")]
    InvalidRequest(Box<Request>),
    #[error("Tracked request API is unavailable for this client instance")]
    TrackingUnavailable,
}

/// Error returned by fallible client builders.
#[derive(Debug, thiserror::Error)]
pub enum ClientBuildError {
    #[error("Invalid MQTT client configuration: {0}")]
    Config(#[from] ConfigError),
    #[error("Failed to create Tokio runtime: {0}")]
    Runtime(#[source] std::io::Error),
}

fn map_plain_send_error(error: SendError<Request>) -> ClientError {
    ClientError::RequestChannelDisconnected(Box::new(error.into_inner()))
}

fn map_plain_try_send_error(error: TrySendError<Request>) -> ClientError {
    match error {
        TrySendError::Full(request) => ClientError::RequestChannelFull(Box::new(request)),
        TrySendError::Disconnected(request) => {
            ClientError::RequestChannelDisconnected(Box::new(request))
        }
    }
}

#[derive(Clone, Debug)]
enum RequestSender {
    Plain(Sender<Request>),
    WithNotice {
        requests: Sender<RequestEnvelope>,
        control_requests: Sender<RequestEnvelope>,
        immediate_disconnect: Sender<RequestEnvelope>,
    },
}

fn into_request(envelope: RequestEnvelope) -> Request {
    envelope.request
}

fn map_send_envelope_error(err: SendError<RequestEnvelope>) -> ClientError {
    ClientError::RequestChannelDisconnected(Box::new(into_request(err.into_inner())))
}

fn map_try_send_envelope_error(err: TrySendError<RequestEnvelope>) -> ClientError {
    match err {
        TrySendError::Full(envelope) => {
            ClientError::RequestChannelFull(Box::new(into_request(envelope)))
        }
        TrySendError::Disconnected(envelope) => {
            ClientError::RequestChannelDisconnected(Box::new(into_request(envelope)))
        }
    }
}

const fn is_publish_request(request: &Request) -> bool {
    matches!(request, Request::Publish(_))
}

/// Prepared acknowledgement packet for manual acknowledgement mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManualAck {
    PubAck(PubAck),
    PubRec(PubRec),
}

impl ManualAck {
    const fn into_request(self) -> Request {
        match self {
            Self::PubAck(ack) => Request::PubAck(ack),
            Self::PubRec(rec) => Request::PubRec(rec),
        }
    }
}

/// An asynchronous client, communicates with MQTT `EventLoop`.
///
/// This is cloneable and can be used to asynchronously [`publish`](`AsyncClient::publish`),
/// [`subscribe`](`AsyncClient::subscribe`) through the `EventLoop`, which is to be polled parallelly.
///
/// **NOTE**: The `EventLoop` must be regularly polled in order to send, receive and process packets
/// from the broker, i.e. move ahead.
///
/// Bounded clients apply backpressure through the client request channel. If the
/// same task that drives [`EventLoop::poll`](crate::EventLoop::poll) awaits
/// request-sending APIs such as [`publish`](Self::publish),
/// [`subscribe`](Self::subscribe), [`unsubscribe`](Self::unsubscribe), or
/// [`ack`](Self::ack) while that channel is full, it can self-block: the send is
/// waiting for the event loop to read a request, but the event loop cannot make
/// progress until that same task polls it again. For bounded async clients,
/// prefer driving the event loop in a dedicated task. Use [`try_publish`](Self::try_publish)
/// when dropping outgoing publishes under overload is intended.
///
/// The request channel is an admission queue, not a strict global wire FIFO
/// guarantee. Under publish flow-control pressure, non-`PUBLISH` control
/// packets can pass earlier `QoS` 1/ `QoS` 2 publishes that are not currently
/// sendable. Application publishes preserve FIFO with other publishes.
#[derive(Clone, Debug)]
pub struct AsyncClient {
    request_tx: RequestSender,
}

/// Builder for synchronous MQTT clients.
///
/// The request channel is bounded by default using
/// [`MqttOptions::request_channel_capacity`]. Use [`Self::capacity`] to override
/// the bounded capacity, or [`Self::unbounded`] to opt into an unbounded request channel.
#[derive(Debug)]
pub struct ClientBuilder {
    options: MqttOptions,
    capacity: RequestChannelCapacity,
}

/// Builder for asynchronous MQTT clients.
///
/// The request channel is bounded by default using
/// [`MqttOptions::request_channel_capacity`]. Use [`Self::capacity`] to override
/// the bounded capacity, or [`Self::unbounded`] to opt into an unbounded request channel.
#[derive(Debug)]
pub struct AsyncClientBuilder {
    options: MqttOptions,
    capacity: RequestChannelCapacity,
}

#[must_use]
fn build_async_client(
    options: MqttOptions,
    capacity: RequestChannelCapacity,
) -> (AsyncClient, EventLoop) {
    let (eventloop, request_tx, control_request_tx, immediate_disconnect_tx) =
        EventLoop::new_for_async_client_with_capacity(options, capacity);
    let client = AsyncClient {
        request_tx: RequestSender::WithNotice {
            requests: request_tx,
            control_requests: control_request_tx,
            immediate_disconnect: immediate_disconnect_tx,
        },
    };

    (client, eventloop)
}

impl ClientBuilder {
    /// Create a new builder for a synchronous [`Client`].
    #[must_use]
    pub const fn new(options: MqttOptions) -> Self {
        let capacity = RequestChannelCapacity::Bounded(options.request_channel_capacity());
        Self { options, capacity }
    }

    /// Use a bounded request channel with the given capacity.
    ///
    /// `0` creates a bounded zero-capacity rendezvous channel. Use [`Self::unbounded`]
    /// for an unbounded request channel.
    #[must_use]
    pub const fn capacity(mut self, cap: usize) -> Self {
        self.capacity = RequestChannelCapacity::Bounded(cap);
        self
    }

    /// Use an unbounded request channel.
    #[must_use]
    pub const fn unbounded(mut self) -> Self {
        self.capacity = RequestChannelCapacity::Unbounded;
        self
    }

    /// Build a synchronous client and connection.
    ///
    /// This builder always produces the synchronous client pair so the
    /// terminal `build()` method matches the entry point that created it.
    ///
    /// # Panics
    ///
    /// Panics if the current-thread Tokio runtime cannot be created.
    #[must_use]
    pub fn build(self) -> (Client, Connection) {
        self.try_build()
            .expect("could not build synchronous MQTT client")
    }

    /// Try to build a synchronous client and connection.
    ///
    /// # Errors
    ///
    /// Returns [`ClientBuildError`] if options validation fails or the
    /// current-thread Tokio runtime cannot be created.
    pub fn try_build(self) -> Result<(Client, Connection), ClientBuildError> {
        self.options.validate()?;
        let (client, eventloop) = build_async_client(self.options, self.capacity);
        let client = Client { client };
        let runtime = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ClientBuildError::Runtime)?;

        let connection = Connection::new(eventloop, runtime);
        Ok((client, connection))
    }
}

impl AsyncClientBuilder {
    /// Create a new builder for an asynchronous [`AsyncClient`].
    #[must_use]
    pub const fn new(options: MqttOptions) -> Self {
        let capacity = RequestChannelCapacity::Bounded(options.request_channel_capacity());
        Self { options, capacity }
    }

    /// Use a bounded request channel with the given capacity.
    ///
    /// `0` creates a bounded zero-capacity rendezvous channel. Use [`Self::unbounded`]
    /// for an unbounded request channel.
    #[must_use]
    pub const fn capacity(mut self, cap: usize) -> Self {
        self.capacity = RequestChannelCapacity::Bounded(cap);
        self
    }

    /// Use an unbounded request channel.
    #[must_use]
    pub const fn unbounded(mut self) -> Self {
        self.capacity = RequestChannelCapacity::Unbounded;
        self
    }

    /// Build an asynchronous client and event loop.
    ///
    /// This builder always produces the asynchronous client pair so the
    /// terminal `build()` method matches the entry point that created it.
    #[must_use]
    pub fn build(self) -> (AsyncClient, EventLoop) {
        self.try_build()
            .expect("could not build asynchronous MQTT client")
    }

    /// Try to build an asynchronous client and event loop.
    ///
    /// # Errors
    ///
    /// Returns [`ClientBuildError`] if options validation fails.
    pub fn try_build(self) -> Result<(AsyncClient, EventLoop), ClientBuildError> {
        self.options.validate()?;
        Ok(build_async_client(self.options, self.capacity))
    }
}

impl AsyncClient {
    /// Create a builder for an [`AsyncClient`].
    ///
    /// The returned [`AsyncClientBuilder`] only builds the asynchronous
    /// client pair, which keeps the terminal `build()` method aligned with
    /// this entry point.
    #[must_use]
    pub const fn builder(options: MqttOptions) -> AsyncClientBuilder {
        AsyncClientBuilder::new(options)
    }

    /// Create a new `AsyncClient` from a channel `Sender`.
    ///
    /// This is mostly useful for creating a test instance where you can
    /// listen on the corresponding receiver.
    #[must_use]
    pub const fn from_senders(request_tx: Sender<Request>) -> Self {
        Self {
            request_tx: RequestSender::Plain(request_tx),
        }
    }

    async fn send_request_async(&self, request: Request) -> Result<(), ClientError> {
        match &self.request_tx {
            RequestSender::Plain(tx) => tx.send_async(request).await.map_err(map_plain_send_error),
            RequestSender::WithNotice {
                requests,
                control_requests,
                ..
            } => {
                let tx = if is_publish_request(&request) {
                    requests
                } else {
                    control_requests
                };
                tx.send_async(RequestEnvelope::plain(request))
                    .await
                    .map_err(map_send_envelope_error)
            }
        }
    }

    fn try_send_request(&self, request: Request) -> Result<(), ClientError> {
        match &self.request_tx {
            RequestSender::Plain(tx) => tx.try_send(request).map_err(map_plain_try_send_error),
            RequestSender::WithNotice {
                requests,
                control_requests,
                ..
            } => {
                let tx = if is_publish_request(&request) {
                    requests
                } else {
                    control_requests
                };
                tx.try_send(RequestEnvelope::plain(request))
                    .map_err(map_try_send_envelope_error)
            }
        }
    }

    fn send_request(&self, request: Request) -> Result<(), ClientError> {
        match &self.request_tx {
            RequestSender::Plain(tx) => tx.send(request).map_err(map_plain_send_error),
            RequestSender::WithNotice {
                requests,
                control_requests,
                ..
            } => {
                let tx = if is_publish_request(&request) {
                    requests
                } else {
                    control_requests
                };
                tx.send(RequestEnvelope::plain(request))
                    .map_err(map_send_envelope_error)
            }
        }
    }

    async fn send_immediate_disconnect_async(&self, request: Request) -> Result<(), ClientError> {
        match &self.request_tx {
            RequestSender::Plain(tx) => tx.send_async(request).await.map_err(map_plain_send_error),
            RequestSender::WithNotice {
                immediate_disconnect,
                ..
            } => immediate_disconnect
                .send_async(RequestEnvelope::plain(request))
                .await
                .map_err(map_send_envelope_error),
        }
    }

    fn try_send_immediate_disconnect(&self, request: Request) -> Result<(), ClientError> {
        match &self.request_tx {
            RequestSender::Plain(tx) => tx.try_send(request).map_err(map_plain_try_send_error),
            RequestSender::WithNotice {
                immediate_disconnect,
                ..
            } => immediate_disconnect
                .try_send(RequestEnvelope::plain(request))
                .map_err(map_try_send_envelope_error),
        }
    }

    fn send_immediate_disconnect(&self, request: Request) -> Result<(), ClientError> {
        match &self.request_tx {
            RequestSender::Plain(tx) => tx.send(request).map_err(map_plain_send_error),
            RequestSender::WithNotice {
                immediate_disconnect,
                ..
            } => immediate_disconnect
                .send(RequestEnvelope::plain(request))
                .map_err(map_send_envelope_error),
        }
    }

    async fn send_tracked_publish_async(
        &self,
        publish: Publish,
    ) -> Result<PublishNotice, ClientError> {
        let RequestSender::WithNotice {
            requests: request_tx,
            ..
        } = &self.request_tx
        else {
            return Err(ClientError::TrackingUnavailable);
        };

        let (notice_tx, notice) = PublishNoticeTx::new();
        request_tx
            .send_async(RequestEnvelope::tracked_publish(publish, notice_tx))
            .await
            .map_err(map_send_envelope_error)?;
        Ok(notice)
    }

    fn try_send_tracked_publish(&self, publish: Publish) -> Result<PublishNotice, ClientError> {
        let RequestSender::WithNotice {
            requests: request_tx,
            ..
        } = &self.request_tx
        else {
            return Err(ClientError::TrackingUnavailable);
        };

        let (notice_tx, notice) = PublishNoticeTx::new();
        request_tx
            .try_send(RequestEnvelope::tracked_publish(publish, notice_tx))
            .map_err(map_try_send_envelope_error)?;
        Ok(notice)
    }

    async fn send_tracked_subscribe_async(
        &self,
        subscribe: Subscribe,
    ) -> Result<SubscribeNotice, ClientError> {
        let RequestSender::WithNotice {
            control_requests: request_tx,
            ..
        } = &self.request_tx
        else {
            return Err(ClientError::TrackingUnavailable);
        };

        let (notice_tx, notice) = SubscribeNoticeTx::new();
        request_tx
            .send_async(RequestEnvelope::tracked_subscribe(subscribe, notice_tx))
            .await
            .map_err(map_send_envelope_error)?;
        Ok(notice)
    }

    fn try_send_tracked_subscribe(
        &self,
        subscribe: Subscribe,
    ) -> Result<SubscribeNotice, ClientError> {
        let RequestSender::WithNotice {
            control_requests: request_tx,
            ..
        } = &self.request_tx
        else {
            return Err(ClientError::TrackingUnavailable);
        };

        let (notice_tx, notice) = SubscribeNoticeTx::new();
        request_tx
            .try_send(RequestEnvelope::tracked_subscribe(subscribe, notice_tx))
            .map_err(map_try_send_envelope_error)?;
        Ok(notice)
    }

    async fn send_tracked_unsubscribe_async(
        &self,
        unsubscribe: Unsubscribe,
    ) -> Result<UnsubscribeNotice, ClientError> {
        let RequestSender::WithNotice {
            control_requests: request_tx,
            ..
        } = &self.request_tx
        else {
            return Err(ClientError::TrackingUnavailable);
        };

        let (notice_tx, notice) = UnsubscribeNoticeTx::new();
        request_tx
            .send_async(RequestEnvelope::tracked_unsubscribe(unsubscribe, notice_tx))
            .await
            .map_err(map_send_envelope_error)?;
        Ok(notice)
    }

    fn try_send_tracked_unsubscribe(
        &self,
        unsubscribe: Unsubscribe,
    ) -> Result<UnsubscribeNotice, ClientError> {
        let RequestSender::WithNotice {
            control_requests: request_tx,
            ..
        } = &self.request_tx
        else {
            return Err(ClientError::TrackingUnavailable);
        };

        let (notice_tx, notice) = UnsubscribeNoticeTx::new();
        request_tx
            .try_send(RequestEnvelope::tracked_unsubscribe(unsubscribe, notice_tx))
            .map_err(map_try_send_envelope_error)?;
        Ok(notice)
    }

    /// Sends a MQTT Publish to the `EventLoop`.
    async fn handle_publish<T, P>(
        &self,
        topic: T,
        payload: P,
        options: PublishOptions,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        P: IntoPublishPayload,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_publish_topic(&topic);
        let mut publish = Publish::from_bytes(topic, options.qos(), payload.into_publish_payload());
        publish.retain = options.retain();
        let publish = Request::Publish(publish);

        if invalid_topic {
            return Err(ClientError::InvalidRequest(Box::new(publish)));
        }

        self.send_request_async(publish).await?;
        Ok(())
    }

    async fn handle_publish_tracked<T, P>(
        &self,
        topic: T,
        payload: P,
        options: PublishOptions,
    ) -> Result<PublishNotice, ClientError>
    where
        T: Into<PublishTopic>,
        P: IntoPublishPayload,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_publish_topic(&topic);
        let mut publish = Publish::from_bytes(topic, options.qos(), payload.into_publish_payload());
        publish.retain = options.retain();
        let request = Request::Publish(publish.clone());

        if invalid_topic {
            return Err(ClientError::InvalidRequest(Box::new(request)));
        }

        self.send_tracked_publish_async(publish).await
    }

    /// Sends a MQTT Publish to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic is invalid or if the request cannot be
    /// queued on the event loop.
    pub async fn publish<T, P>(
        &self,
        topic: T,
        payload: P,
        options: PublishOptions,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        P: IntoPublishPayload,
    {
        self.handle_publish(topic, payload, options).await
    }

    /// Sends a MQTT Publish to the `EventLoop` and returns a notice which resolves on MQTT ack milestone.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic is invalid or if the request cannot be
    /// queued on the event loop.
    pub async fn publish_tracked<T, P>(
        &self,
        topic: T,
        payload: P,
        options: PublishOptions,
    ) -> Result<PublishNotice, ClientError>
    where
        T: Into<PublishTopic>,
        P: IntoPublishPayload,
    {
        self.handle_publish_tracked(topic, payload, options).await
    }

    /// Attempts to send a MQTT Publish to the `EventLoop`.
    fn handle_try_publish<T, P>(
        &self,
        topic: T,
        payload: P,
        options: PublishOptions,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        P: IntoPublishPayload,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_publish_topic(&topic);
        let mut publish = Publish::from_bytes(topic, options.qos(), payload.into_publish_payload());
        publish.retain = options.retain();
        let publish = Request::Publish(publish);

        if invalid_topic {
            return Err(ClientError::InvalidRequest(Box::new(publish)));
        }

        self.try_send_request(publish)?;
        Ok(())
    }

    fn handle_try_publish_tracked<T, P>(
        &self,
        topic: T,
        payload: P,
        options: PublishOptions,
    ) -> Result<PublishNotice, ClientError>
    where
        T: Into<PublishTopic>,
        P: IntoPublishPayload,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_publish_topic(&topic);
        let mut publish = Publish::from_bytes(topic, options.qos(), payload.into_publish_payload());
        publish.retain = options.retain();
        let request = Request::Publish(publish.clone());

        if invalid_topic {
            return Err(ClientError::InvalidRequest(Box::new(request)));
        }

        self.try_send_tracked_publish(publish)
    }

    /// Attempts to send a MQTT Publish to the `EventLoop`.
    ///
    /// This is the non-blocking publish API for overload policies that may drop
    /// outgoing publishes. If the bounded request channel is full, this returns
    /// an error immediately and the publish has not been queued.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic is invalid or if the request cannot be
    /// queued immediately on the event loop.
    pub fn try_publish<T, P>(
        &self,
        topic: T,
        payload: P,
        options: PublishOptions,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        P: IntoPublishPayload,
    {
        self.handle_try_publish(topic, payload, options)
    }

    /// Attempts to send a MQTT Publish to the `EventLoop` and returns a notice.
    ///
    /// This is the non-blocking tracked publish API for overload policies that
    /// may drop outgoing publishes. If the bounded request channel is full, this
    /// returns an error immediately and the publish has not been queued.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic is invalid or if the request cannot be
    /// queued immediately on the event loop.
    pub fn try_publish_tracked<T, P>(
        &self,
        topic: T,
        payload: P,
        options: PublishOptions,
    ) -> Result<PublishNotice, ClientError>
    where
        T: Into<PublishTopic>,
        P: IntoPublishPayload,
    {
        self.handle_try_publish_tracked(topic, payload, options)
    }

    /// Prepares a MQTT PubAck/PubRec packet for manual acknowledgement.
    ///
    /// Returns `None` for `QoS0` publishes, which do not require acknowledgement.
    pub const fn prepare_ack(&self, publish: &Publish) -> Option<ManualAck> {
        prepare_ack(publish)
    }

    /// Sends a prepared MQTT PubAck/PubRec to the `EventLoop`.
    ///
    /// This is useful when [`AckMode::Manual`](crate::AckMode::Manual) is enabled and
    /// acknowledgement must be deferred.
    ///
    /// Manual ACK mode is advanced and application-managed. Applications must acknowledge
    /// every incoming `QoS` 1/`QoS` 2 publish to remain MQTT-compliant. The library rejects
    /// invalid or duplicate manual ACKs, but it cannot guarantee eventual ACK completion.
    ///
    /// # Errors
    ///
    /// Returns an error if the acknowledgement cannot be queued on the event
    /// loop.
    pub async fn manual_ack(&self, ack: ManualAck) -> Result<(), ClientError> {
        self.send_request_async(ack.into_request()).await?;
        Ok(())
    }

    /// Attempts to send a prepared MQTT PubAck/PubRec to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the acknowledgement cannot be queued immediately on
    /// the event loop.
    pub fn try_manual_ack(&self, ack: ManualAck) -> Result<(), ClientError> {
        self.try_send_request(ack.into_request())?;
        Ok(())
    }

    /// Sends a MQTT PubAck/PubRec to the `EventLoop` based on publish `QoS`.
    /// Only needed if [`AckMode::Manual`](crate::AckMode::Manual) is configured.
    ///
    /// Applications using manual ACK mode must acknowledge every incoming `QoS` 1/`QoS` 2
    /// publish to remain MQTT-compliant.
    ///
    /// # Errors
    ///
    /// Returns an error if the derived acknowledgement cannot be queued on the
    /// event loop.
    pub async fn ack(&self, publish: &Publish) -> Result<(), ClientError> {
        if let Some(ack) = self.prepare_ack(publish) {
            self.manual_ack(ack).await?;
        }
        Ok(())
    }

    /// Attempts to send a MQTT PubAck/PubRec to the `EventLoop` based on publish `QoS`.
    /// Only needed if [`AckMode::Manual`](crate::AckMode::Manual) is configured.
    ///
    /// # Errors
    ///
    /// Returns an error if the derived acknowledgement cannot be queued
    /// immediately on the event loop.
    pub fn try_ack(&self, publish: &Publish) -> Result<(), ClientError> {
        if let Some(ack) = self.prepare_ack(publish) {
            self.try_manual_ack(ack)?;
        }
        Ok(())
    }

    /// Sends a MQTT Subscribe to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued on the event loop.
    pub async fn subscribe<F: Into<TopicFilter>>(
        &self,
        topic: F,
        qos: QoS,
    ) -> Result<(), ClientError> {
        let (subscribe, valid) = subscribe_from_topic_filter(topic, qos);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(subscribe.into())));
        }

        self.send_request_async(subscribe.into()).await?;
        Ok(())
    }

    /// Sends a tracked MQTT Subscribe to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued on the event loop.
    pub async fn subscribe_tracked<F: Into<TopicFilter>>(
        &self,
        topic: F,
        qos: QoS,
    ) -> Result<SubscribeNotice, ClientError> {
        let (subscribe, valid) = subscribe_from_topic_filter(topic, qos);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(subscribe.into())));
        }

        self.send_tracked_subscribe_async(subscribe).await
    }

    /// Attempts to send a MQTT Subscribe to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued immediately on the event loop.
    pub fn try_subscribe<F: Into<TopicFilter>>(
        &self,
        topic: F,
        qos: QoS,
    ) -> Result<(), ClientError> {
        let (subscribe, valid) = subscribe_from_topic_filter(topic, qos);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(subscribe.into())));
        }

        self.try_send_request(subscribe.into())?;
        Ok(())
    }

    /// Attempts to send a tracked MQTT Subscribe to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued immediately on the event loop.
    pub fn try_subscribe_tracked<F: Into<TopicFilter>>(
        &self,
        topic: F,
        qos: QoS,
    ) -> Result<SubscribeNotice, ClientError> {
        let (subscribe, valid) = subscribe_from_topic_filter(topic, qos);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(subscribe.into())));
        }

        self.try_send_tracked_subscribe(subscribe)
    }

    /// Sends a MQTT Subscribe for multiple topics to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued on the event loop.
    pub async fn subscribe_many<T, I>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = I>,
        I: Into<SubscribeFilterInput>,
    {
        let (subscribe, valid) = subscribe_from_filter_inputs(topics);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(subscribe.into())));
        }

        self.send_request_async(subscribe.into()).await?;
        Ok(())
    }

    /// Sends a tracked MQTT Subscribe for multiple topics to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued on the event loop.
    pub async fn subscribe_many_tracked<T, I>(
        &self,
        topics: T,
    ) -> Result<SubscribeNotice, ClientError>
    where
        T: IntoIterator<Item = I>,
        I: Into<SubscribeFilterInput>,
    {
        let (subscribe, valid) = subscribe_from_filter_inputs(topics);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(subscribe.into())));
        }

        self.send_tracked_subscribe_async(subscribe).await
    }

    /// Attempts to send a MQTT Subscribe for multiple topics to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued immediately on the event loop.
    pub fn try_subscribe_many<T, I>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = I>,
        I: Into<SubscribeFilterInput>,
    {
        let (subscribe, valid) = subscribe_from_filter_inputs(topics);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(subscribe.into())));
        }
        self.try_send_request(subscribe.into())?;
        Ok(())
    }

    /// Attempts to send a tracked MQTT Subscribe for multiple topics to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued immediately on the event loop.
    pub fn try_subscribe_many_tracked<T, I>(
        &self,
        topics: T,
    ) -> Result<SubscribeNotice, ClientError>
    where
        T: IntoIterator<Item = I>,
        I: Into<SubscribeFilterInput>,
    {
        let (subscribe, valid) = subscribe_from_filter_inputs(topics);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(subscribe.into())));
        }

        self.try_send_tracked_subscribe(subscribe)
    }

    /// Sends a MQTT Unsubscribe to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued on the event loop.
    pub async fn unsubscribe<F: Into<TopicFilter>>(&self, topic: F) -> Result<(), ClientError> {
        let (unsubscribe, valid) = unsubscribe_from_topic_filter(topic);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(Request::Unsubscribe(
                unsubscribe,
            ))));
        }

        let request = Request::Unsubscribe(unsubscribe);
        self.send_request_async(request).await?;
        Ok(())
    }

    /// Sends a MQTT Unsubscribe for multiple topic filters to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued on the event loop.
    pub async fn unsubscribe_many<T, F>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = F>,
        F: Into<TopicFilter>,
    {
        let (unsubscribe, valid) = unsubscribe_from_topic_filters(topics);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(Request::Unsubscribe(
                unsubscribe,
            ))));
        }

        let request = Request::Unsubscribe(unsubscribe);
        self.send_request_async(request).await?;
        Ok(())
    }

    /// Sends a tracked MQTT Unsubscribe to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued on the event loop.
    pub async fn unsubscribe_tracked<F: Into<TopicFilter>>(
        &self,
        topic: F,
    ) -> Result<UnsubscribeNotice, ClientError> {
        let (unsubscribe, valid) = unsubscribe_from_topic_filter(topic);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(Request::Unsubscribe(
                unsubscribe,
            ))));
        }

        self.send_tracked_unsubscribe_async(unsubscribe).await
    }

    /// Sends a tracked MQTT Unsubscribe for multiple topic filters to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued on the event loop.
    pub async fn unsubscribe_many_tracked<T, F>(
        &self,
        topics: T,
    ) -> Result<UnsubscribeNotice, ClientError>
    where
        T: IntoIterator<Item = F>,
        F: Into<TopicFilter>,
    {
        let (unsubscribe, valid) = unsubscribe_from_topic_filters(topics);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(Request::Unsubscribe(
                unsubscribe,
            ))));
        }

        self.send_tracked_unsubscribe_async(unsubscribe).await
    }

    /// Attempts to send a MQTT Unsubscribe to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued immediately on the event loop.
    pub fn try_unsubscribe<F: Into<TopicFilter>>(&self, topic: F) -> Result<(), ClientError> {
        let (unsubscribe, valid) = unsubscribe_from_topic_filter(topic);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(Request::Unsubscribe(
                unsubscribe,
            ))));
        }

        let request = Request::Unsubscribe(unsubscribe);
        self.try_send_request(request)?;
        Ok(())
    }

    /// Attempts to send a MQTT Unsubscribe for multiple topic filters to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued immediately on the event loop.
    pub fn try_unsubscribe_many<T, F>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = F>,
        F: Into<TopicFilter>,
    {
        let (unsubscribe, valid) = unsubscribe_from_topic_filters(topics);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(Request::Unsubscribe(
                unsubscribe,
            ))));
        }

        let request = Request::Unsubscribe(unsubscribe);
        self.try_send_request(request)?;
        Ok(())
    }

    /// Attempts to send a tracked MQTT Unsubscribe to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued immediately on the event loop.
    pub fn try_unsubscribe_tracked<F: Into<TopicFilter>>(
        &self,
        topic: F,
    ) -> Result<UnsubscribeNotice, ClientError> {
        let (unsubscribe, valid) = unsubscribe_from_topic_filter(topic);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(Request::Unsubscribe(
                unsubscribe,
            ))));
        }

        self.try_send_tracked_unsubscribe(unsubscribe)
    }

    /// Attempts to send a tracked MQTT Unsubscribe for multiple topic filters to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued immediately on the event loop.
    pub fn try_unsubscribe_many_tracked<T, F>(
        &self,
        topics: T,
    ) -> Result<UnsubscribeNotice, ClientError>
    where
        T: IntoIterator<Item = F>,
        F: Into<TopicFilter>,
    {
        let (unsubscribe, valid) = unsubscribe_from_topic_filters(topics);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(Request::Unsubscribe(
                unsubscribe,
            ))));
        }

        self.try_send_tracked_unsubscribe(unsubscribe)
    }

    /// Queues a graceful MQTT disconnect barrier.
    ///
    /// Once the event loop observes this request, it stops processing later
    /// application work, flushes previously accepted `QoS` 0 publishes, and
    /// waits for previously accepted outbound `QoS` 1/ `QoS` 2 publishes and
    /// tracked subscribe/unsubscribe requests to complete. `QoS` 1 publishes
    /// complete on `PUBACK`, `QoS` 2 publishes complete on `PUBCOMP`, tracked
    /// subscribes complete on `SUBACK`, and tracked unsubscribes complete on
    /// `UNSUBACK`. It then sends and flushes MQTT `DISCONNECT`.
    ///
    /// This request uses the normal client request channel. Under publish
    /// flow-control pressure, it may pass earlier `QoS` 1/ `QoS` 2 publishes
    /// that are not currently sendable; once observed, it becomes the graceful
    /// drain barrier.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued on the
    /// event loop.
    pub async fn disconnect(&self) -> Result<(), ClientError> {
        let request = Request::Disconnect(Disconnect);
        self.send_request_async(request).await?;
        Ok(())
    }

    /// Queues a graceful MQTT disconnect barrier with a drain timeout.
    ///
    /// Once the event loop observes this request, it stops processing later
    /// application work, flushes previously accepted `QoS` 0 publishes, and waits
    /// up to `timeout` for previously accepted outbound `QoS` 1/ `QoS` 2 publishes
    /// and tracked subscribe/unsubscribe requests to complete. `QoS` 1 publishes
    /// complete on `PUBACK`, `QoS` 2 publishes complete on `PUBCOMP`, tracked
    /// subscribes complete on `SUBACK`, and tracked unsubscribes complete on
    /// `UNSUBACK`.
    ///
    /// If the drain completes before the deadline, the event loop sends and
    /// flushes MQTT `DISCONNECT`. If the deadline expires first, polling returns
    /// `ConnectionError::DisconnectTimeout` and MQTT `DISCONNECT` is not sent.
    ///
    /// This request uses the normal client request channel. The timeout starts
    /// only after the event loop observes this request, not necessarily when
    /// this method queues it.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued on the
    /// event loop.
    pub async fn disconnect_with_timeout(&self, timeout: Duration) -> Result<(), ClientError> {
        let request = Request::DisconnectWithTimeout(Disconnect, timeout);
        self.send_request_async(request).await?;
        Ok(())
    }

    /// Sends a MQTT disconnect immediately without waiting for in-flight requests.
    ///
    /// This request uses a dedicated immediate shutdown channel, not the normal
    /// application request channel. It may bypass queued application work and
    /// does not wait for unresolved `QoS` 1/ `QoS` 2 publish handshakes.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued on the
    /// event loop.
    pub async fn disconnect_now(&self) -> Result<(), ClientError> {
        let request = Request::DisconnectNow(Disconnect);
        self.send_immediate_disconnect_async(request).await?;
        Ok(())
    }

    /// Attempts to queue a graceful MQTT disconnect barrier.
    ///
    /// Once the event loop observes this request, it stops processing later
    /// application work, flushes previously accepted `QoS` 0 publishes, and waits
    /// for previously accepted outbound `QoS` 1/ `QoS` 2 publishes and tracked
    /// subscribe/unsubscribe requests to complete before sending MQTT
    /// `DISCONNECT`.
    ///
    /// This request uses the normal client request channel. Under publish
    /// flow-control pressure, it may pass earlier `QoS` 1/ `QoS` 2 publishes
    /// that are not currently sendable; once observed, it becomes the graceful
    /// drain barrier.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued
    /// immediately on the event loop.
    pub fn try_disconnect(&self) -> Result<(), ClientError> {
        let request = Request::Disconnect(Disconnect);
        self.try_send_request(request)?;
        Ok(())
    }

    /// Attempts to queue a graceful MQTT disconnect with a drain timeout.
    ///
    /// Once the event loop observes this request, it stops processing later
    /// application work, flushes previously accepted `QoS` 0 publishes, and waits
    /// up to `timeout` for previously accepted outbound `QoS` 1/ `QoS` 2 publishes
    /// and tracked subscribe/unsubscribe requests to complete. `QoS` 1 publishes
    /// complete on `PUBACK`, `QoS` 2 publishes complete on `PUBCOMP`, tracked
    /// subscribes complete on `SUBACK`, and tracked unsubscribes complete on
    /// `UNSUBACK`.
    ///
    /// If the drain completes before the deadline, the event loop sends and
    /// flushes MQTT `DISCONNECT`. If the deadline expires first, polling returns
    /// `ConnectionError::DisconnectTimeout` and MQTT `DISCONNECT` is not sent.
    ///
    /// This request uses the normal client request channel. The timeout starts
    /// only after the event loop observes this request, not necessarily when
    /// this method queues it.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued
    /// immediately on the event loop.
    pub fn try_disconnect_with_timeout(&self, timeout: Duration) -> Result<(), ClientError> {
        let request = Request::DisconnectWithTimeout(Disconnect, timeout);
        self.try_send_request(request)?;
        Ok(())
    }

    /// Attempts to queue an immediate MQTT disconnect.
    ///
    /// This request uses a dedicated immediate shutdown channel, not the normal
    /// application request channel. It may bypass queued application work and
    /// does not wait for unresolved `QoS` 1/ `QoS` 2 publish handshakes.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued
    /// immediately on the event loop.
    pub fn try_disconnect_now(&self) -> Result<(), ClientError> {
        let request = Request::DisconnectNow(Disconnect);
        self.try_send_immediate_disconnect(request)?;
        Ok(())
    }
}

const fn prepare_ack(publish: &Publish) -> Option<ManualAck> {
    let ack = match publish.qos {
        QoS::AtMostOnce => return None,
        QoS::AtLeastOnce => ManualAck::PubAck(PubAck::new(publish.pkid)),
        QoS::ExactlyOnce => ManualAck::PubRec(PubRec::new(publish.pkid)),
    };
    Some(ack)
}

/// A synchronous client, communicates with MQTT `EventLoop`.
///
/// This is cloneable and can be used to synchronously [`publish`](`AsyncClient::publish`),
/// [`subscribe`](`AsyncClient::subscribe`) through the `EventLoop`/`Connection`, which is to be polled in parallel
/// by iterating over the object returned by [`Connection.iter()`](Connection::iter) in a separate thread.
///
/// **NOTE**: The `EventLoop`/`Connection` must be regularly polled(`.next()` in case of `Connection`) in order
/// to send, receive and process packets from the broker, i.e. move ahead.
///
/// An asynchronous channel handle can also be extracted if necessary.
#[derive(Clone)]
pub struct Client {
    client: AsyncClient,
}

impl Client {
    /// Create a builder for a [`Client`].
    ///
    /// The returned [`ClientBuilder`] only builds the synchronous client
    /// pair, which keeps the terminal `build()` method aligned with this
    /// entry point.
    #[must_use]
    pub const fn builder(options: MqttOptions) -> ClientBuilder {
        ClientBuilder::new(options)
    }

    /// Create a new `Client` from a channel `Sender`.
    ///
    /// This is mostly useful for creating a test instance where you can
    /// listen on the corresponding receiver.
    #[must_use]
    pub const fn from_sender(request_tx: Sender<Request>) -> Self {
        Self {
            client: AsyncClient::from_senders(request_tx),
        }
    }

    /// Sends a MQTT Publish to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the topic is invalid or if the request cannot be
    /// queued on the event loop.
    pub fn publish<T, P>(
        &self,
        topic: T,
        payload: P,
        options: PublishOptions,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        P: IntoPublishPayload,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_publish_topic(&topic);
        let mut publish = Publish::from_bytes(topic, options.qos(), payload.into_publish_payload());
        publish.retain = options.retain();
        let publish = Request::Publish(publish);
        if invalid_topic {
            return Err(ClientError::InvalidRequest(Box::new(publish)));
        }
        self.client.send_request(publish)?;
        Ok(())
    }

    /// Attempts to send a MQTT Publish to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic is invalid or if the request cannot be
    /// queued immediately on the event loop.
    pub fn try_publish<T, P>(
        &self,
        topic: T,
        payload: P,
        options: PublishOptions,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        P: IntoPublishPayload,
    {
        self.client.try_publish(topic, payload, options)?;
        Ok(())
    }

    /// Prepares a MQTT PubAck/PubRec packet for manual acknowledgement.
    ///
    /// Returns `None` for `QoS0` publishes, which do not require acknowledgement.
    pub const fn prepare_ack(&self, publish: &Publish) -> Option<ManualAck> {
        self.client.prepare_ack(publish)
    }

    /// Sends a prepared MQTT PubAck/PubRec to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the acknowledgement cannot be queued on the event
    /// loop.
    pub fn manual_ack(&self, ack: ManualAck) -> Result<(), ClientError> {
        self.client.send_request(ack.into_request())?;
        Ok(())
    }

    /// Attempts to send a prepared MQTT PubAck/PubRec to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the acknowledgement cannot be queued immediately on
    /// the event loop.
    pub fn try_manual_ack(&self, ack: ManualAck) -> Result<(), ClientError> {
        self.client.try_manual_ack(ack)?;
        Ok(())
    }

    /// Sends a MQTT PubAck/PubRec to the `EventLoop` based on publish `QoS`.
    /// Only needed if [`AckMode::Manual`](crate::AckMode::Manual) is configured.
    ///
    /// Applications using manual ACK mode must acknowledge every incoming `QoS` 1/`QoS` 2
    /// publish to remain MQTT-compliant.
    ///
    /// # Errors
    ///
    /// Returns an error if the derived acknowledgement cannot be queued on the
    /// event loop.
    pub fn ack(&self, publish: &Publish) -> Result<(), ClientError> {
        if let Some(ack) = self.prepare_ack(publish) {
            self.manual_ack(ack)?;
        }
        Ok(())
    }

    /// Attempts to send a MQTT PubAck/PubRec to the `EventLoop` based on publish `QoS`.
    /// Only needed if [`AckMode::Manual`](crate::AckMode::Manual) is configured.
    ///
    /// # Errors
    ///
    /// Returns an error if the derived acknowledgement cannot be queued
    /// immediately on the event loop.
    pub fn try_ack(&self, publish: &Publish) -> Result<(), ClientError> {
        if let Some(ack) = self.prepare_ack(publish) {
            self.try_manual_ack(ack)?;
        }
        Ok(())
    }

    /// Sends a MQTT Subscribe to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued on the event loop.
    pub fn subscribe<F: Into<TopicFilter>>(&self, topic: F, qos: QoS) -> Result<(), ClientError> {
        let (subscribe, valid) = subscribe_from_topic_filter(topic, qos);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(subscribe.into())));
        }

        self.client.send_request(subscribe.into())?;
        Ok(())
    }

    /// Sends a MQTT Subscribe to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued immediately on the event loop.
    pub fn try_subscribe<F: Into<TopicFilter>>(
        &self,
        topic: F,
        qos: QoS,
    ) -> Result<(), ClientError> {
        self.client.try_subscribe(topic, qos)?;
        Ok(())
    }

    /// Sends a MQTT Subscribe for multiple topics to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued on the event loop.
    pub fn subscribe_many<T, I>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = I>,
        I: Into<SubscribeFilterInput>,
    {
        let (subscribe, valid) = subscribe_from_filter_inputs(topics);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(subscribe.into())));
        }

        self.client.send_request(subscribe.into())?;
        Ok(())
    }

    /// Attempts to send a MQTT Subscribe for multiple topics to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued immediately on the event loop.
    pub fn try_subscribe_many<T, I>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = I>,
        I: Into<SubscribeFilterInput>,
    {
        self.client.try_subscribe_many(topics)
    }

    /// Sends a MQTT Unsubscribe to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued on the event loop.
    pub fn unsubscribe<F: Into<TopicFilter>>(&self, topic: F) -> Result<(), ClientError> {
        let (unsubscribe, valid) = unsubscribe_from_topic_filter(topic);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(Request::Unsubscribe(
                unsubscribe,
            ))));
        }

        let request = Request::Unsubscribe(unsubscribe);
        self.client.send_request(request)?;
        Ok(())
    }

    /// Sends a MQTT Unsubscribe for multiple topic filters to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued on the event loop.
    pub fn unsubscribe_many<T, F>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = F>,
        F: Into<TopicFilter>,
    {
        let (unsubscribe, valid) = unsubscribe_from_topic_filters(topics);
        if !valid {
            return Err(ClientError::InvalidRequest(Box::new(Request::Unsubscribe(
                unsubscribe,
            ))));
        }

        let request = Request::Unsubscribe(unsubscribe);
        self.client.send_request(request)?;
        Ok(())
    }

    /// Sends a MQTT Unsubscribe to the `EventLoop`
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid or if the request
    /// cannot be queued immediately on the event loop.
    pub fn try_unsubscribe<F: Into<TopicFilter>>(&self, topic: F) -> Result<(), ClientError> {
        self.client.try_unsubscribe(topic)?;
        Ok(())
    }

    /// Attempts to send a MQTT Unsubscribe for multiple topic filters to the `EventLoop`.
    ///
    /// # Errors
    ///
    /// Returns an error if the filter list is invalid or if the request cannot
    /// be queued immediately on the event loop.
    pub fn try_unsubscribe_many<T, F>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = F>,
        F: Into<TopicFilter>,
    {
        self.client.try_unsubscribe_many(topics)?;
        Ok(())
    }

    /// Queues a graceful MQTT disconnect barrier.
    ///
    /// Once the event loop observes this request, it stops processing later
    /// application work, flushes previously accepted `QoS` 0 publishes, and waits
    /// for previously accepted outbound `QoS` 1/ `QoS` 2 publishes and tracked
    /// subscribe/unsubscribe requests to complete before sending MQTT
    /// `DISCONNECT`.
    ///
    /// This request uses the normal client request channel. Under publish
    /// flow-control pressure, it may pass earlier `QoS` 1/ `QoS` 2 publishes
    /// that are not currently sendable; once observed, it becomes the graceful
    /// drain barrier.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued on the
    /// event loop.
    pub fn disconnect(&self) -> Result<(), ClientError> {
        let request = Request::Disconnect(Disconnect);
        self.client.send_request(request)?;
        Ok(())
    }

    /// Queues a graceful MQTT disconnect barrier with a drain timeout.
    ///
    /// Once the event loop observes this request, it stops processing later
    /// application work, flushes previously accepted `QoS` 0 publishes, and waits
    /// up to `timeout` for previously accepted outbound `QoS` 1/ `QoS` 2 publishes
    /// and tracked subscribe/unsubscribe requests to complete. `QoS` 1 publishes
    /// complete on `PUBACK`, `QoS` 2 publishes complete on `PUBCOMP`, tracked
    /// subscribes complete on `SUBACK`, and tracked unsubscribes complete on
    /// `UNSUBACK`.
    ///
    /// If the drain completes before the deadline, the event loop sends and
    /// flushes MQTT `DISCONNECT`. If the deadline expires first, polling returns
    /// `ConnectionError::DisconnectTimeout` and MQTT `DISCONNECT` is not sent.
    ///
    /// This request uses the normal client request channel. The timeout starts
    /// only after the event loop observes this request, not necessarily when
    /// this method queues it.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued on the
    /// event loop.
    pub fn disconnect_with_timeout(&self, timeout: Duration) -> Result<(), ClientError> {
        let request = Request::DisconnectWithTimeout(Disconnect, timeout);
        self.client.send_request(request)?;
        Ok(())
    }

    /// Sends a MQTT disconnect immediately without waiting for in-flight requests.
    ///
    /// This request uses a dedicated immediate shutdown channel, not the normal
    /// application request channel. It may bypass queued application work and
    /// does not wait for unresolved `QoS` 1/ `QoS` 2 publish handshakes.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued on the
    /// event loop.
    pub fn disconnect_now(&self) -> Result<(), ClientError> {
        let request = Request::DisconnectNow(Disconnect);
        self.client.send_immediate_disconnect(request)?;
        Ok(())
    }

    /// Attempts to queue a graceful MQTT disconnect barrier.
    ///
    /// Once the event loop observes this request, it stops processing later
    /// application work, flushes previously accepted `QoS` 0 publishes, and waits
    /// for previously accepted outbound `QoS` 1/ `QoS` 2 publishes and tracked
    /// subscribe/unsubscribe requests to complete before sending MQTT
    /// `DISCONNECT`.
    ///
    /// This request uses the normal client request channel. Under publish
    /// flow-control pressure, it may pass earlier `QoS` 1/ `QoS` 2 publishes
    /// that are not currently sendable; once observed, it becomes the graceful
    /// drain barrier.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued
    /// immediately on the event loop.
    pub fn try_disconnect(&self) -> Result<(), ClientError> {
        self.client.try_disconnect()?;
        Ok(())
    }

    /// Attempts to queue a graceful MQTT disconnect barrier with a drain timeout.
    ///
    /// Once the event loop observes this request, it stops processing later
    /// application work, flushes previously accepted `QoS` 0 publishes, and waits
    /// up to `timeout` for previously accepted outbound `QoS` 1/ `QoS` 2 publishes
    /// and tracked subscribe/unsubscribe requests to complete. `QoS` 1 publishes
    /// complete on `PUBACK`, `QoS` 2 publishes complete on `PUBCOMP`, tracked
    /// subscribes complete on `SUBACK`, and tracked unsubscribes complete on
    /// `UNSUBACK`.
    ///
    /// If the drain completes before the deadline, the event loop sends and
    /// flushes MQTT `DISCONNECT`. If the deadline expires first, polling returns
    /// `ConnectionError::DisconnectTimeout` and MQTT `DISCONNECT` is not sent.
    ///
    /// This request uses the normal client request channel. The timeout starts
    /// only after the event loop observes this request, not necessarily when
    /// this method queues it.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued
    /// immediately on the event loop.
    pub fn try_disconnect_with_timeout(&self, timeout: Duration) -> Result<(), ClientError> {
        self.client.try_disconnect_with_timeout(timeout)?;
        Ok(())
    }

    /// Sends a MQTT disconnect immediately without waiting for in-flight requests.
    ///
    /// This request uses a dedicated immediate shutdown channel, not the normal
    /// application request channel. It may bypass queued application work and
    /// does not wait for unresolved `QoS` 1/ `QoS` 2 publish handshakes.
    ///
    /// # Errors
    ///
    /// Returns an error if the disconnect request cannot be queued
    /// immediately on the event loop.
    pub fn try_disconnect_now(&self) -> Result<(), ClientError> {
        self.client.try_disconnect_now()?;
        Ok(())
    }
}

#[must_use]
fn valid_publish_topic(topic: &str) -> bool {
    !topic.is_empty() && valid_topic(topic)
}

#[must_use]
fn valid_mqtt_string_value(value: &str) -> bool {
    !value.as_bytes().contains(&0) && u16::try_from(value.len()).is_ok()
}

#[must_use]
fn valid_topic_filter(filter: &str) -> bool {
    valid_filter(filter) && valid_mqtt_string_value(filter)
}

fn subscribe_from_topic_filter<F: Into<TopicFilter>>(topic: F, qos: QoS) -> (Subscribe, bool) {
    let (topic, validate) = topic.into().into_string_and_validation();
    let filter = SubscribeFilter::new(topic, qos);
    let valid = !validate || valid_topic_filter(&filter.path);

    (Subscribe::new_many([filter]), valid)
}

fn subscribe_from_filter_inputs<T, I>(topics: T) -> (Subscribe, bool)
where
    T: IntoIterator<Item = I>,
    I: Into<SubscribeFilterInput>,
{
    let mut filters = Vec::new();
    let mut valid = true;

    for input in topics {
        let (filter, validate) = input.into().into_filter_and_validation();
        valid &= !validate || valid_topic_filter(&filter.path);
        filters.push(filter);
    }

    valid &= !filters.is_empty();

    (Subscribe::new_many(filters), valid)
}

fn unsubscribe_from_topic_filter<F: Into<TopicFilter>>(topic: F) -> (Unsubscribe, bool) {
    let (topic, validate) = topic.into().into_string_and_validation();
    let valid = !validate || valid_topic_filter(&topic);

    (Unsubscribe::new(topic), valid)
}

fn unsubscribe_from_topic_filters<T, F>(topics: T) -> (Unsubscribe, bool)
where
    T: IntoIterator<Item = F>,
    F: Into<TopicFilter>,
{
    let topic_filters = topics
        .into_iter()
        .map(|topic| topic.into().into_string_and_validation())
        .collect::<Vec<_>>();
    let valid = !topic_filters.is_empty()
        && topic_filters
            .iter()
            .all(|(topic, validate)| !*validate || valid_topic_filter(topic));
    let topics = topic_filters.into_iter().map(|(topic, _)| topic).collect();

    (Unsubscribe { pkid: 0, topics }, valid)
}

/// Error type returned by [`Connection::recv`]
#[derive(Debug, Eq, PartialEq)]
pub struct RecvError;

/// Error type returned by [`Connection::try_recv`]
#[derive(Debug, Eq, PartialEq)]
pub enum TryRecvError {
    /// User has closed requests channel
    Disconnected,
    /// Did not resolve
    Empty,
}

/// Error type returned by [`Connection::recv_timeout`]
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum RecvTimeoutError {
    /// User has closed requests channel
    #[error("all request senders have been dropped")]
    Disconnected,
    /// No event arrived before the supplied duration elapsed.
    #[error("no event arrived within {duration:?}")]
    Timeout { duration: Duration },
}

///  MQTT connection. Maintains all the necessary state
pub struct Connection {
    pub eventloop: EventLoop,
    runtime: Runtime,
}
impl Connection {
    const fn new(eventloop: EventLoop, runtime: Runtime) -> Self {
        Self { eventloop, runtime }
    }

    /// Returns an iterator over this connection. Iterating over this is all that's
    /// necessary to make connection progress and maintain a robust connection.
    /// Just continuing to loop will reconnect
    /// **NOTE** Don't block this while iterating
    // ideally this should be named iter_mut because it requires a mutable reference
    // Also we can implement IntoIter for this to make it easy to iterate over it
    #[must_use = "Connection should be iterated over a loop to make progress"]
    pub const fn iter(&mut self) -> Iter<'_> {
        Iter { connection: self }
    }

    /// Attempt to fetch an incoming [`Event`] on the [`EventLoop`], returning an error
    /// if all clients/users have closed requests channel.
    ///
    /// [`EventLoop`]: super::EventLoop
    ///
    /// # Errors
    ///
    /// Returns [`RecvError`] if all request senders have been dropped.
    pub fn recv(&mut self) -> Result<Result<Event, ConnectionError>, RecvError> {
        let f = self.eventloop.poll();
        let event = self.runtime.block_on(f);

        resolve_event(event).ok_or(RecvError)
    }

    /// Attempt to fetch an incoming [`Event`] on the [`EventLoop`], returning an error
    /// if none immediately present or all clients/users have closed requests channel.
    ///
    /// [`EventLoop`]: super::EventLoop
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError::Empty`] if no event is immediately ready, or
    /// [`TryRecvError::Disconnected`] if all request senders have been dropped.
    pub fn try_recv(&mut self) -> Result<Result<Event, ConnectionError>, TryRecvError> {
        let f = self.eventloop.poll();
        // Enters the runtime context so we can poll the future, as required by `now_or_never()`.
        // ref: https://docs.rs/tokio/latest/tokio/runtime/struct.Runtime.html#method.enter
        let _guard = self.runtime.enter();
        let event = f.now_or_never().ok_or(TryRecvError::Empty)?;

        resolve_event(event).ok_or(TryRecvError::Disconnected)
    }

    /// Attempt to fetch an incoming [`Event`] on the [`EventLoop`], returning an error
    /// if all clients/users have closed requests channel or the timeout has expired.
    ///
    /// [`EventLoop`]: super::EventLoop
    ///
    /// # Errors
    ///
    /// Returns [`RecvTimeoutError::Timeout`] if no event arrives before
    /// `duration`, or [`RecvTimeoutError::Disconnected`] if all request
    /// senders have been dropped.
    pub fn recv_timeout(
        &mut self,
        duration: Duration,
    ) -> Result<Result<Event, ConnectionError>, RecvTimeoutError> {
        let f = self.eventloop.poll();
        let event = self
            .runtime
            .block_on(async { timeout(duration, f).await })
            .map_err(|_| RecvTimeoutError::Timeout { duration })?;

        resolve_event(event).ok_or(RecvTimeoutError::Disconnected)
    }
}

fn resolve_event(event: Result<Event, ConnectionError>) -> Option<Result<Event, ConnectionError>> {
    match event {
        Ok(v) => Some(Ok(v)),
        // closing of request channel should stop the iterator
        Err(ConnectionError::RequestsDone) => {
            trace!("Done with requests");
            None
        }
        Err(e) => Some(Err(e)),
    }
}

/// Iterator which polls the `EventLoop` for connection progress
pub struct Iter<'a> {
    connection: &'a mut Connection,
}

impl Iterator for Iter<'_> {
    type Item = Result<Event, ConnectionError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.connection.recv().ok()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::LastWill;

    #[test]
    fn calling_iter_twice_on_connection_shouldnt_panic() {
        let mut mqttoptions = MqttOptions::new("test-1", "localhost");
        let will = LastWill::new("hello/world", "good bye", QoS::AtMostOnce, false);
        mqttoptions.set_keep_alive(5).set_last_will(will);

        let (_, mut connection) = Client::builder(mqttoptions).capacity(10).build();
        drop(connection.iter());
        drop(connection.iter());
    }

    #[test]
    fn builder_uses_options_request_channel_capacity_by_default() {
        let mut mqttoptions = MqttOptions::new("test-1", "localhost");
        mqttoptions.set_request_channel_capacity(1);
        let builder: AsyncClientBuilder = AsyncClient::builder(mqttoptions);
        let (client, _eventloop) = builder.build();

        client
            .try_publish("hello/world", "one", PublishOptions::new(QoS::AtMostOnce))
            .expect("first request should fit configured capacity");
        assert!(matches!(
            client.try_publish("hello/world", "two", PublishOptions::new(QoS::AtMostOnce)),
            Err(ClientError::RequestChannelFull(request)) if matches!(*request, Request::Publish(_))
        ));
    }

    #[test]
    fn sync_and_async_entry_points_return_distinct_builder_types() {
        let sync_builder = Client::builder(MqttOptions::new("test-sync", "localhost"));
        let async_builder = AsyncClient::builder(MqttOptions::new("test-async", "localhost"));

        let _: ClientBuilder = sync_builder;
        let _: AsyncClientBuilder = async_builder;
    }

    #[test]
    fn builder_capacity_overrides_options_request_channel_capacity() {
        let mut mqttoptions = MqttOptions::new("test-1", "localhost");
        mqttoptions.set_request_channel_capacity(1);
        let (client, _eventloop) = Client::builder(mqttoptions).capacity(2).build();

        client
            .try_publish("hello/world", "one", PublishOptions::new(QoS::AtMostOnce))
            .expect("first request should fit overridden capacity");
        client
            .try_publish("hello/world", "two", PublishOptions::new(QoS::AtMostOnce))
            .expect("second request should fit overridden capacity");
        assert!(matches!(
            client.try_publish("hello/world", "three", PublishOptions::new(QoS::AtMostOnce)),
            Err(ClientError::RequestChannelFull(request)) if matches!(*request, Request::Publish(_))
        ));
    }

    #[test]
    fn builder_capacity_zero_is_bounded_rendezvous() {
        let mqttoptions = MqttOptions::new("test-1", "localhost");
        let (client, _eventloop) = AsyncClient::builder(mqttoptions).capacity(0).build();

        assert!(matches!(
            client.try_publish("hello/world", "one", PublishOptions::new(QoS::AtMostOnce)),
            Err(ClientError::RequestChannelFull(request)) if matches!(*request, Request::Publish(_))
        ));
    }

    #[test]
    fn try_publish_reports_disconnected_request_channel() {
        let (tx, rx) = flume::bounded(1);
        drop(rx);
        let client = AsyncClient::from_senders(tx);

        assert!(matches!(
            client.try_publish("hello/world", "one", PublishOptions::new(QoS::AtMostOnce)),
            Err(ClientError::RequestChannelDisconnected(request))
                if matches!(*request, Request::Publish(_))
        ));
    }

    #[test]
    fn try_publish_reports_full_plain_request_channel() {
        let (tx, _rx) = flume::bounded(1);
        let client = AsyncClient::from_senders(tx);
        client
            .try_publish("hello/world", "one", PublishOptions::new(QoS::AtMostOnce))
            .expect("first request should fit bounded channel");

        assert!(matches!(
            client.try_publish("hello/world", "two", PublishOptions::new(QoS::AtMostOnce)),
            Err(ClientError::RequestChannelFull(request)) if matches!(*request, Request::Publish(_))
        ));
    }

    #[test]
    fn blocking_publish_reports_disconnected_plain_request_channel() {
        let (tx, rx) = flume::bounded(1);
        drop(rx);
        let client = Client::from_sender(tx);

        assert!(matches!(
            client.publish("hello/world", "one", PublishOptions::new(QoS::AtMostOnce)),
            Err(ClientError::RequestChannelDisconnected(request))
                if matches!(*request, Request::Publish(_))
        ));
    }

    #[test]
    fn client_error_display_messages_are_specific() {
        assert_eq!(
            ClientError::RequestChannelFull(Box::new(Request::PingReq)).to_string(),
            "Request channel is full"
        );
        assert_eq!(
            ClientError::RequestChannelDisconnected(Box::new(Request::PingReq)).to_string(),
            "Request channel is disconnected"
        );
        assert_eq!(
            ClientError::InvalidRequest(Box::new(Request::PingReq)).to_string(),
            "Invalid MQTT request"
        );
    }

    #[test]
    fn unbounded_builder_allows_try_publish_without_polling() {
        let mqttoptions = MqttOptions::new("test-1", "localhost");
        let (client, _eventloop) = AsyncClient::builder(mqttoptions).unbounded().build();

        for i in 0..128 {
            client
                .try_publish("hello/world", vec![i], PublishOptions::new(QoS::AtMostOnce))
                .expect("unbounded channel should accept requests without polling");
        }
    }

    #[tokio::test]
    async fn bounded_publish_blocks_when_channel_is_full_without_polling() {
        let mqttoptions = MqttOptions::new("test-1", "localhost");
        let (client, _eventloop) = AsyncClient::builder(mqttoptions).capacity(1).build();

        client
            .publish("hello/world", "one", PublishOptions::new(QoS::AtMostOnce))
            .await
            .expect("first request should fit bounded channel");

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(25),
            client.publish("hello/world", "two", PublishOptions::new(QoS::AtMostOnce)),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn unbounded_publish_completes_without_polling() {
        let mqttoptions = MqttOptions::new("test-1", "localhost");
        let (client, _eventloop) = AsyncClient::builder(mqttoptions).unbounded().build();

        for i in 0..128 {
            client
                .publish("hello/world", vec![i], PublishOptions::new(QoS::AtMostOnce))
                .await
                .expect("unbounded channel should accept requests without polling");
        }
    }

    #[test]
    fn should_be_able_to_build_test_client_from_channel() {
        let (tx, rx) = flume::bounded(1);
        let client = Client::from_sender(tx);
        client
            .publish(
                "hello/world",
                "good bye",
                PublishOptions::new(QoS::ExactlyOnce),
            )
            .expect("Should be able to publish");
        drop(rx.try_recv().expect("Should have message"));
    }

    #[test]
    fn prepare_ack_maps_qos_to_manual_ack_packets_v4() {
        let (tx, _) = flume::bounded(1);
        let client = Client::from_sender(tx);

        let qos0 = Publish::new("hello/world", QoS::AtMostOnce, vec![1]);
        assert!(client.prepare_ack(&qos0).is_none());

        let mut qos1 = Publish::new("hello/world", QoS::AtLeastOnce, vec![1]);
        qos1.pkid = 7;
        match client.prepare_ack(&qos1) {
            Some(ManualAck::PubAck(ack)) => assert_eq!(ack.pkid, 7),
            ack => panic!("expected QoS1 PubAck, got {ack:?}"),
        }

        let mut qos2 = Publish::new("hello/world", QoS::ExactlyOnce, vec![1]);
        qos2.pkid = 9;
        match client.prepare_ack(&qos2) {
            Some(ManualAck::PubRec(ack)) => assert_eq!(ack.pkid, 9),
            ack => panic!("expected QoS2 PubRec, got {ack:?}"),
        }
    }

    #[test]
    fn manual_ack_sends_puback_request_v4() {
        let (tx, rx) = flume::bounded(1);
        let client = Client::from_sender(tx);
        client
            .manual_ack(ManualAck::PubAck(PubAck::new(42)))
            .expect("manual_ack should send request");

        let request = rx.try_recv().expect("Should have ack request");
        match request {
            Request::PubAck(ack) => assert_eq!(ack.pkid, 42),
            request => panic!("Expected PubAck request, got {request:?}"),
        }
    }

    #[test]
    fn try_manual_ack_sends_pubrec_request_v4() {
        let (tx, rx) = flume::bounded(1);
        let client = Client::from_sender(tx);
        client
            .try_manual_ack(ManualAck::PubRec(PubRec::new(51)))
            .expect("try_manual_ack should send request");

        let request = rx.try_recv().expect("Should have ack request");
        match request {
            Request::PubRec(ack) => assert_eq!(ack.pkid, 51),
            request => panic!("Expected PubRec request, got {request:?}"),
        }
    }

    #[test]
    fn ack_and_try_ack_use_manual_ack_flow_v4() {
        let (tx, rx) = flume::bounded(2);
        let client = Client::from_sender(tx);

        let mut qos1 = Publish::new("hello/world", QoS::AtLeastOnce, vec![1]);
        qos1.pkid = 11;
        client.ack(&qos1).expect("ack should send PubAck");

        let mut qos2 = Publish::new("hello/world", QoS::ExactlyOnce, vec![1]);
        qos2.pkid = 13;
        client
            .try_ack(&qos2)
            .expect("try_ack should send PubRec request");

        let first = rx.try_recv().expect("Should receive first ack request");
        match first {
            Request::PubAck(ack) => assert_eq!(ack.pkid, 11),
            request => panic!("Expected PubAck request, got {request:?}"),
        }

        let second = rx.try_recv().expect("Should receive second ack request");
        match second {
            Request::PubRec(ack) => assert_eq!(ack.pkid, 13),
            request => panic!("Expected PubRec request, got {request:?}"),
        }
    }

    #[test]
    fn can_publish_with_validated_topic() {
        let (tx, rx) = flume::bounded(1);
        let client = Client::from_sender(tx);
        let valid_topic = ValidatedTopic::new("hello/world").unwrap();
        client
            .publish(
                valid_topic,
                "good bye",
                PublishOptions::new(QoS::ExactlyOnce),
            )
            .expect("Should be able to publish");
        drop(rx.try_recv().expect("Should have message"));
    }

    #[test]
    fn publish_accepts_non_static_borrowed_payloads() {
        let (tx, rx) = flume::bounded(2);
        let client = Client::from_sender(tx);
        let payload_string = "borrowed string payload".to_owned();
        let payload_bytes = vec![1, 2, 3, 4];

        client
            .publish(
                "hello/string",
                payload_string.as_str(),
                PublishOptions::new(QoS::AtMostOnce),
            )
            .expect("Should publish borrowed string payload");
        client
            .publish(
                "hello/bytes",
                payload_bytes.as_slice(),
                PublishOptions::new(QoS::AtMostOnce),
            )
            .expect("Should publish borrowed byte payload");

        let first = rx.try_recv().expect("Should have string publish");
        match first {
            Request::Publish(publish) => assert_eq!(publish.payload, payload_string),
            request => panic!("Expected Publish request, got {request:?}"),
        }

        let second = rx.try_recv().expect("Should have byte publish");
        match second {
            Request::Publish(publish) => assert_eq!(publish.payload, payload_bytes.as_slice()),
            request => panic!("Expected Publish request, got {request:?}"),
        }
    }

    #[test]
    fn publish_accepts_byte_array_payloads() {
        let (tx, rx) = flume::bounded(2);
        let client = Client::from_sender(tx);
        let owned_payload = [1_u8, 2, 3, 4];

        client
            .publish(
                "hello/byte-string",
                b"hello",
                PublishOptions::new(QoS::AtMostOnce),
            )
            .expect("Should publish byte string payload");
        client
            .publish(
                "hello/byte-array",
                owned_payload,
                PublishOptions::new(QoS::AtMostOnce),
            )
            .expect("Should publish owned byte array payload");

        let first = rx.try_recv().expect("Should have byte string publish");
        match first {
            Request::Publish(publish) => assert_eq!(publish.payload, b"hello".as_slice()),
            request => panic!("Expected Publish request, got {request:?}"),
        }

        let second = rx.try_recv().expect("Should have owned byte array publish");
        match second {
            Request::Publish(publish) => assert_eq!(publish.payload, owned_payload.as_slice()),
            request => panic!("Expected Publish request, got {request:?}"),
        }
    }

    #[test]
    fn publish_accepts_borrowed_string_topic() {
        let (tx, rx) = flume::bounded(2);
        let client = Client::from_sender(tx);
        let topic = "hello/world".to_string();
        client
            .publish(&topic, "good bye", PublishOptions::new(QoS::ExactlyOnce))
            .expect("Should be able to publish");
        client
            .try_publish(&topic, "good bye", PublishOptions::new(QoS::ExactlyOnce))
            .expect("Should be able to publish");
        drop(rx.try_recv().expect("Should have message"));
        drop(rx.try_recv().expect("Should have message"));
    }

    #[test]
    fn publish_accepts_cow_topic_variants() {
        let (tx, rx) = flume::bounded(2);
        let client = Client::from_sender(tx);
        client
            .publish(
                std::borrow::Cow::Borrowed("hello/world"),
                "good bye",
                PublishOptions::new(QoS::ExactlyOnce),
            )
            .expect("Should be able to publish");
        client
            .try_publish(
                std::borrow::Cow::Owned("hello/world".to_owned()),
                "good bye",
                PublishOptions::new(QoS::ExactlyOnce),
            )
            .expect("Should be able to publish");
        drop(rx.try_recv().expect("Should have message"));
        drop(rx.try_recv().expect("Should have message"));
    }

    #[test]
    fn publishing_invalid_cow_topic_fails() {
        let (tx, _) = flume::bounded(1);
        let client = Client::from_sender(tx);
        let err = client
            .publish(
                std::borrow::Cow::Borrowed("a/+/b"),
                "good bye",
                PublishOptions::new(QoS::ExactlyOnce),
            )
            .expect_err("Invalid publish topic should fail");
        assert!(
            matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Publish(_)))
        );
    }

    #[test]
    fn validated_topic_ergonomics() {
        let valid_topic = ValidatedTopic::new("hello/world").unwrap();
        let valid_topic_can_be_cloned = valid_topic.clone();
        assert_eq!(valid_topic, valid_topic_can_be_cloned);
    }

    #[test]
    fn creating_invalid_validated_topic_fails() {
        assert_eq!(
            ValidatedTopic::new("a/+/b"),
            Err(InvalidTopic("a/+/b".to_string()))
        );
        assert_eq!(ValidatedTopic::new(""), Err(InvalidTopic(String::new())));
    }

    #[test]
    fn validated_topic_filter_accepts_mqtt_topic_filters() {
        for filter in ["a/b", "a/+/b", "a/#", "#", "+", "/"] {
            assert_eq!(
                ValidatedTopicFilter::new(filter),
                Ok(ValidatedTopicFilter(filter.to_owned()))
            );
        }
    }

    #[test]
    fn creating_invalid_validated_topic_filter_fails() {
        let overlong = "a".repeat(usize::from(u16::MAX) + 1);

        assert_eq!(
            ValidatedTopicFilter::new(""),
            Err(InvalidTopicFilter(String::new()))
        );
        assert_eq!(
            ValidatedTopicFilter::new("a/#/b"),
            Err(InvalidTopicFilter("a/#/b".to_owned()))
        );
        assert_eq!(
            ValidatedTopicFilter::new("a/b#"),
            Err(InvalidTopicFilter("a/b#".to_owned()))
        );
        assert_eq!(
            ValidatedTopicFilter::new("a/+b"),
            Err(InvalidTopicFilter("a/+b".to_owned()))
        );
        assert_eq!(
            ValidatedTopicFilter::new("a\0b"),
            Err(InvalidTopicFilter("a\0b".to_owned()))
        );
        assert_eq!(
            ValidatedTopicFilter::new(overlong.clone()),
            Err(InvalidTopicFilter(overlong))
        );
    }

    #[test]
    fn subscribing_accepts_raw_and_validated_topic_filters() {
        let (tx, rx) = flume::bounded(2);
        let client = Client::from_sender(tx);

        client
            .subscribe("a/+/b", QoS::AtLeastOnce)
            .expect("raw wildcard filter should subscribe");
        client
            .try_subscribe(ValidatedTopicFilter::new("c/#").unwrap(), QoS::ExactlyOnce)
            .expect("validated wildcard filter should subscribe");

        match rx.try_recv().expect("Should have raw subscribe") {
            Request::Subscribe(subscribe) => {
                assert_eq!(subscribe.filters[0].path, "a/+/b");
                assert_eq!(subscribe.filters[0].qos, QoS::AtLeastOnce);
            }
            request => panic!("Expected Subscribe request, got {request:?}"),
        }

        match rx.try_recv().expect("Should have validated subscribe") {
            Request::Subscribe(subscribe) => {
                assert_eq!(subscribe.filters[0].path, "c/#");
                assert_eq!(subscribe.filters[0].qos, QoS::ExactlyOnce);
            }
            request => panic!("Expected Subscribe request, got {request:?}"),
        }
    }

    #[test]
    fn subscribing_invalid_raw_topic_filter_fails() {
        let (tx, rx) = flume::bounded(3);
        let client = Client::from_sender(tx);
        let overlong = "a".repeat(usize::from(u16::MAX) + 1);

        let err = client
            .subscribe("a/#/b", QoS::AtLeastOnce)
            .expect_err("Invalid wildcard placement should fail");
        assert!(
            matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Subscribe(_)))
        );

        let err = client
            .try_subscribe("a\0b", QoS::AtLeastOnce)
            .expect_err("NUL should fail");
        assert!(
            matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Subscribe(_)))
        );

        let err = client
            .try_subscribe(overlong, QoS::AtLeastOnce)
            .expect_err("Overlong filter should fail");
        assert!(
            matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Subscribe(_)))
        );

        assert!(rx.is_empty());
    }

    #[test]
    fn subscribe_many_accepts_raw_protocol_and_validated_filters() {
        let (tx, rx) = flume::bounded(2);
        let client = Client::from_sender(tx);

        client
            .subscribe_many(vec![SubscribeFilter::new(
                "raw/+/filter".to_owned(),
                QoS::AtLeastOnce,
            )])
            .expect("raw protocol filter should subscribe");
        client
            .try_subscribe_many(vec![
                SubscribeFilterInput::new("raw/#", QoS::AtMostOnce),
                SubscribeFilterInput::validated(
                    ValidatedTopicFilter::new("validated/+").unwrap(),
                    QoS::ExactlyOnce,
                ),
            ])
            .expect("validated filter input should subscribe");

        match rx.try_recv().expect("Should have raw subscribe many") {
            Request::Subscribe(subscribe) => {
                assert_eq!(subscribe.filters.len(), 1);
                assert_eq!(subscribe.filters[0].path, "raw/+/filter");
            }
            request => panic!("Expected Subscribe request, got {request:?}"),
        }

        match rx.try_recv().expect("Should have validated subscribe many") {
            Request::Subscribe(subscribe) => {
                assert_eq!(subscribe.filters.len(), 2);
                assert_eq!(subscribe.filters[0].path, "raw/#");
                assert_eq!(subscribe.filters[1].path, "validated/+");
                assert_eq!(subscribe.filters[1].qos, QoS::ExactlyOnce);
            }
            request => panic!("Expected Subscribe request, got {request:?}"),
        }
    }

    #[test]
    fn unsubscribe_validates_topic_filters_and_supports_many() {
        let (tx, rx) = flume::bounded(2);
        let client = Client::from_sender(tx);

        let err = client
            .unsubscribe("a/#/b")
            .expect_err("Invalid wildcard placement should fail");
        assert!(
            matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Unsubscribe(_)))
        );

        let err = client.try_unsubscribe("a\0b").expect_err("NUL should fail");
        assert!(matches!(
            err,
            ClientError::InvalidRequest(req) if matches!(*req, Request::Unsubscribe(_))
        ));

        let err = client
            .try_unsubscribe_many(Vec::<TopicFilter>::new())
            .expect_err("Empty unsubscribe list should fail");
        assert!(matches!(
            err,
            ClientError::InvalidRequest(req) if matches!(*req, Request::Unsubscribe(_))
        ));

        client
            .unsubscribe(ValidatedTopicFilter::new("a/+/b").unwrap())
            .expect("validated unsubscribe should enqueue");
        client
            .try_unsubscribe_many(vec![
                TopicFilter::from("raw/#"),
                TopicFilter::from(ValidatedTopicFilter::new("validated/+").unwrap()),
            ])
            .expect("multi-unsubscribe should enqueue");

        match rx.try_recv().expect("Should have unsubscribe") {
            Request::Unsubscribe(unsubscribe) => assert_eq!(unsubscribe.topics, vec!["a/+/b"]),
            request => panic!("Expected Unsubscribe request, got {request:?}"),
        }

        match rx.try_recv().expect("Should have unsubscribe many") {
            Request::Unsubscribe(unsubscribe) => {
                assert_eq!(unsubscribe.topics, vec!["raw/#", "validated/+"]);
            }
            request => panic!("Expected Unsubscribe request, got {request:?}"),
        }
    }

    #[test]
    fn publishing_invalid_raw_topic_fails() {
        let (tx, _) = flume::bounded(1);
        let client = Client::from_sender(tx);
        let err = client
            .publish("a/+/b", "good bye", PublishOptions::new(QoS::ExactlyOnce))
            .expect_err("Invalid publish topic should fail");
        assert!(
            matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Publish(_)))
        );

        let err = client
            .publish("", "good bye", PublishOptions::new(QoS::ExactlyOnce))
            .expect_err("Empty publish topic should fail");
        assert!(
            matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Publish(_)))
        );
    }

    #[test]
    fn subscribing_with_empty_filter_list_fails() {
        let (tx, rx) = flume::bounded(2);
        let client = Client::from_sender(tx);

        let err = client
            .subscribe_many(Vec::<SubscribeFilter>::new())
            .expect_err("Empty subscribe filter list should fail");
        assert!(
            matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Subscribe(_)))
        );

        let err = client
            .try_subscribe_many(Vec::<SubscribeFilter>::new())
            .expect_err("Empty subscribe filter list should fail");
        assert!(
            matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Subscribe(_)))
        );

        assert!(rx.is_empty());
    }

    #[test]
    fn async_publish_paths_accept_validated_topic() {
        let (tx, rx) = flume::bounded(2);
        let client = AsyncClient::from_senders(tx);
        let runtime = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            client
                .publish(
                    ValidatedTopic::new("hello/world").unwrap(),
                    "good bye",
                    PublishOptions::new(QoS::ExactlyOnce),
                )
                .await
                .expect("Should be able to publish");

            client
                .publish(
                    ValidatedTopic::new("hello/world").unwrap(),
                    Bytes::from_static(b"good bye"),
                    PublishOptions::new(QoS::ExactlyOnce),
                )
                .await
                .expect("Should be able to publish");
        });

        drop(rx.try_recv().expect("Should have message"));
        drop(rx.try_recv().expect("Should have message"));
    }

    #[test]
    fn async_publishing_invalid_raw_topic_fails() {
        let (tx, _) = flume::bounded(2);
        let client = AsyncClient::from_senders(tx);
        let runtime = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let err = client
                .publish("a/+/b", "good bye", PublishOptions::new(QoS::ExactlyOnce))
                .await
                .expect_err("Invalid publish topic should fail");
            assert!(matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Publish(_))));

            let err = client
                .publish(
                    "a/+/b",
                    Bytes::from_static(b"good bye"),
                    PublishOptions::new(QoS::ExactlyOnce),
                )
                .await
                .expect_err("Invalid publish topic should fail");
            assert!(matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Publish(_))));

            let err = client
                .publish("", "good bye", PublishOptions::new(QoS::ExactlyOnce))
                .await
                .expect_err("Empty publish topic should fail");
            assert!(matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Publish(_))));

            let err = client
                .publish(
                    "",
                    Bytes::from_static(b"good bye"),
                    PublishOptions::new(QoS::ExactlyOnce),
                )
                .await
                .expect_err("Empty publish topic should fail");
            assert!(matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Publish(_))));
        });
    }

    #[test]
    fn async_subscribing_with_empty_filter_list_fails() {
        let (tx, rx) = flume::bounded(2);
        let client = AsyncClient::from_senders(tx);
        let runtime = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let err = client
                .subscribe_many(Vec::<SubscribeFilter>::new())
                .await
                .expect_err("Empty subscribe filter list should fail");
            assert!(
                matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Subscribe(_)))
            );
        });

        let err = client
            .try_subscribe_many(Vec::<SubscribeFilter>::new())
            .expect_err("Empty subscribe filter list should fail");
        assert!(
            matches!(err, ClientError::InvalidRequest(req) if matches!(*req, Request::Subscribe(_)))
        );

        assert!(rx.is_empty());
    }

    #[test]
    fn tracked_publish_requires_tracking_channel() {
        let (tx, _) = flume::bounded(2);
        let client = AsyncClient::from_senders(tx);
        let runtime = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let err = client
                .publish_tracked(
                    "hello/world",
                    "good bye",
                    PublishOptions::new(QoS::AtLeastOnce),
                )
                .await
                .expect_err("tracked publish should fail without tracked channel");
            assert!(matches!(err, ClientError::TrackingUnavailable));

            let err = client
                .publish_tracked(
                    "hello/world",
                    Bytes::from_static(b"good bye"),
                    PublishOptions::new(QoS::AtLeastOnce),
                )
                .await
                .expect_err("tracked publish should fail without tracked channel");
            assert!(matches!(err, ClientError::TrackingUnavailable));

            let err = client
                .subscribe_tracked("hello/world", QoS::AtLeastOnce)
                .await
                .expect_err("tracked subscribe should fail without tracked channel");
            assert!(matches!(err, ClientError::TrackingUnavailable));

            let err = client
                .subscribe_many_tracked(vec![SubscribeFilter::new(
                    "hello/world".to_string(),
                    QoS::AtLeastOnce,
                )])
                .await
                .expect_err("tracked subscribe many should fail without tracked channel");
            assert!(matches!(err, ClientError::TrackingUnavailable));

            let err = client
                .unsubscribe_tracked("hello/world")
                .await
                .expect_err("tracked unsubscribe should fail without tracked channel");
            assert!(matches!(err, ClientError::TrackingUnavailable));
        });

        let err = client
            .try_subscribe_tracked("hello/world", QoS::AtLeastOnce)
            .expect_err("tracked try_subscribe should fail without tracked channel");
        assert!(matches!(err, ClientError::TrackingUnavailable));

        let err = client
            .try_subscribe_many_tracked(vec![SubscribeFilter::new(
                "hello/world".to_string(),
                QoS::AtLeastOnce,
            )])
            .expect_err("tracked try_subscribe_many should fail without tracked channel");
        assert!(matches!(err, ClientError::TrackingUnavailable));

        let err = client
            .try_unsubscribe_tracked("hello/world")
            .expect_err("tracked try_unsubscribe should fail without tracked channel");
        assert!(matches!(err, ClientError::TrackingUnavailable));
    }

    #[test]
    fn tracked_unsubscribe_uses_control_request_channel() {
        let (requests, requests_rx) = flume::bounded(1);
        let (control_requests, control_requests_rx) = flume::bounded(1);
        let (immediate_disconnect, _immediate_disconnect_rx) = flume::unbounded();
        let client = AsyncClient {
            request_tx: RequestSender::WithNotice {
                requests,
                control_requests,
                immediate_disconnect,
            },
        };
        let runtime = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime
            .block_on(client.unsubscribe_tracked("hello/world"))
            .expect("tracked unsubscribe should enqueue");

        assert!(requests_rx.is_empty());
        let envelope = control_requests_rx
            .try_recv()
            .expect("tracked unsubscribe should use control channel");
        assert!(matches!(&envelope.request, Request::Unsubscribe(_)));
    }
}
