//! This module offers a high level synchronous and asynchronous abstraction to
//! async eventloop.
use std::borrow::Cow;
use std::time::Duration;

use crate::eventloop::RequestEnvelope;
use crate::mqttbytes::{QoS, v4::*};
use crate::notice::{PublishNoticeTx, RequestNoticeTx};
use crate::{
    ConnectionError, Event, EventLoop, MqttOptions, PublishNotice, Request, RequestNotice,
    valid_filter, valid_topic,
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
        if valid_topic(&topic_string) {
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

/// Client Error
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClientError {
    #[error("Failed to send mqtt requests to eventloop")]
    Request(Request),
    #[error("Failed to send mqtt requests to eventloop")]
    TryRequest(Request),
    #[error("Tracked request API is unavailable for this client instance")]
    TrackingUnavailable,
}

impl From<SendError<Request>> for ClientError {
    fn from(e: SendError<Request>) -> Self {
        Self::Request(e.into_inner())
    }
}

impl From<TrySendError<Request>> for ClientError {
    fn from(e: TrySendError<Request>) -> Self {
        Self::TryRequest(e.into_inner())
    }
}

#[derive(Clone, Debug)]
enum RequestSender {
    Plain(Sender<Request>),
    WithNotice(Sender<RequestEnvelope>),
}

fn into_request(envelope: RequestEnvelope) -> Request {
    let (request, _notice) = envelope.into_parts();
    request
}

fn map_send_envelope_error(err: SendError<RequestEnvelope>) -> ClientError {
    ClientError::Request(into_request(err.into_inner()))
}

fn map_try_send_envelope_error(err: TrySendError<RequestEnvelope>) -> ClientError {
    match err {
        TrySendError::Full(envelope) | TrySendError::Disconnected(envelope) => {
            ClientError::TryRequest(into_request(envelope))
        }
    }
}

/// Prepared acknowledgement packet for manual acknowledgement mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManualAck {
    PubAck(PubAck),
    PubRec(PubRec),
}

impl ManualAck {
    fn into_request(self) -> Request {
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
#[derive(Clone, Debug)]
pub struct AsyncClient {
    request_tx: RequestSender,
}

impl AsyncClient {
    /// Create a new `AsyncClient`.
    ///
    /// `cap` specifies the capacity of the bounded async channel.
    pub fn new(options: MqttOptions, cap: usize) -> (AsyncClient, EventLoop) {
        let (eventloop, request_tx) = EventLoop::new_for_async_client(options, cap);

        let client = AsyncClient {
            request_tx: RequestSender::WithNotice(request_tx),
        };

        (client, eventloop)
    }

    /// Create a new `AsyncClient` from a channel `Sender`.
    ///
    /// This is mostly useful for creating a test instance where you can
    /// listen on the corresponding receiver.
    #[must_use]
    pub fn from_senders(request_tx: Sender<Request>) -> AsyncClient {
        AsyncClient {
            request_tx: RequestSender::Plain(request_tx),
        }
    }

    async fn send_request_async(&self, request: Request) -> Result<(), ClientError> {
        match &self.request_tx {
            RequestSender::Plain(tx) => tx.send_async(request).await.map_err(ClientError::from),
            RequestSender::WithNotice(tx) => tx
                .send_async(RequestEnvelope::plain(request))
                .await
                .map_err(map_send_envelope_error),
        }
    }

    fn try_send_request(&self, request: Request) -> Result<(), ClientError> {
        match &self.request_tx {
            RequestSender::Plain(tx) => tx.try_send(request).map_err(ClientError::from),
            RequestSender::WithNotice(tx) => tx
                .try_send(RequestEnvelope::plain(request))
                .map_err(map_try_send_envelope_error),
        }
    }

    fn send_request(&self, request: Request) -> Result<(), ClientError> {
        match &self.request_tx {
            RequestSender::Plain(tx) => tx.send(request).map_err(ClientError::from),
            RequestSender::WithNotice(tx) => tx
                .send(RequestEnvelope::plain(request))
                .map_err(map_send_envelope_error),
        }
    }

    async fn send_tracked_publish_async(
        &self,
        publish: Publish,
    ) -> Result<PublishNotice, ClientError> {
        let RequestSender::WithNotice(request_tx) = &self.request_tx else {
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
        let RequestSender::WithNotice(request_tx) = &self.request_tx else {
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
    ) -> Result<RequestNotice, ClientError> {
        let RequestSender::WithNotice(request_tx) = &self.request_tx else {
            return Err(ClientError::TrackingUnavailable);
        };

        let (notice_tx, notice) = RequestNoticeTx::new();
        request_tx
            .send_async(RequestEnvelope::tracked_subscribe(subscribe, notice_tx))
            .await
            .map_err(map_send_envelope_error)?;
        Ok(notice)
    }

    fn try_send_tracked_subscribe(
        &self,
        subscribe: Subscribe,
    ) -> Result<RequestNotice, ClientError> {
        let RequestSender::WithNotice(request_tx) = &self.request_tx else {
            return Err(ClientError::TrackingUnavailable);
        };

        let (notice_tx, notice) = RequestNoticeTx::new();
        request_tx
            .try_send(RequestEnvelope::tracked_subscribe(subscribe, notice_tx))
            .map_err(map_try_send_envelope_error)?;
        Ok(notice)
    }

    async fn send_tracked_unsubscribe_async(
        &self,
        unsubscribe: Unsubscribe,
    ) -> Result<RequestNotice, ClientError> {
        let RequestSender::WithNotice(request_tx) = &self.request_tx else {
            return Err(ClientError::TrackingUnavailable);
        };

        let (notice_tx, notice) = RequestNoticeTx::new();
        request_tx
            .send_async(RequestEnvelope::tracked_unsubscribe(unsubscribe, notice_tx))
            .await
            .map_err(map_send_envelope_error)?;
        Ok(notice)
    }

    fn try_send_tracked_unsubscribe(
        &self,
        unsubscribe: Unsubscribe,
    ) -> Result<RequestNotice, ClientError> {
        let RequestSender::WithNotice(request_tx) = &self.request_tx else {
            return Err(ClientError::TrackingUnavailable);
        };

        let (notice_tx, notice) = RequestNoticeTx::new();
        request_tx
            .try_send(RequestEnvelope::tracked_unsubscribe(unsubscribe, notice_tx))
            .map_err(map_try_send_envelope_error)?;
        Ok(notice)
    }

    /// Sends a MQTT Publish to the `EventLoop`.
    async fn handle_publish<T, V>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: V,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        V: Into<Vec<u8>>,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_topic(&topic);
        let mut publish = Publish::new(topic, qos, payload);
        publish.retain = retain;
        let publish = Request::Publish(publish);

        if invalid_topic {
            return Err(ClientError::Request(publish));
        }

        self.send_request_async(publish).await?;
        Ok(())
    }

    async fn handle_publish_tracked<T, V>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: V,
    ) -> Result<PublishNotice, ClientError>
    where
        T: Into<PublishTopic>,
        V: Into<Vec<u8>>,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_topic(&topic);
        let mut publish = Publish::new(topic, qos, payload);
        publish.retain = retain;
        let request = Request::Publish(publish.clone());

        if invalid_topic {
            return Err(ClientError::Request(request));
        }

        self.send_tracked_publish_async(publish).await
    }

    /// Sends a MQTT Publish to the `EventLoop`.
    pub async fn publish<T, V>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: V,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        V: Into<Vec<u8>>,
    {
        self.handle_publish(topic, qos, retain, payload).await
    }

    /// Sends a MQTT Publish to the `EventLoop` and returns a notice which resolves on MQTT ack milestone.
    pub async fn publish_tracked<T, V>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: V,
    ) -> Result<PublishNotice, ClientError>
    where
        T: Into<PublishTopic>,
        V: Into<Vec<u8>>,
    {
        self.handle_publish_tracked(topic, qos, retain, payload)
            .await
    }

    /// Attempts to send a MQTT Publish to the `EventLoop`.
    fn handle_try_publish<T, V>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: V,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        V: Into<Vec<u8>>,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_topic(&topic);
        let mut publish = Publish::new(topic, qos, payload);
        publish.retain = retain;
        let publish = Request::Publish(publish);

        if invalid_topic {
            return Err(ClientError::TryRequest(publish));
        }

        self.try_send_request(publish)?;
        Ok(())
    }

    fn handle_try_publish_tracked<T, V>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: V,
    ) -> Result<PublishNotice, ClientError>
    where
        T: Into<PublishTopic>,
        V: Into<Vec<u8>>,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_topic(&topic);
        let mut publish = Publish::new(topic, qos, payload);
        publish.retain = retain;
        let request = Request::Publish(publish.clone());

        if invalid_topic {
            return Err(ClientError::TryRequest(request));
        }

        self.try_send_tracked_publish(publish)
    }

    /// Attempts to send a MQTT Publish to the `EventLoop`.
    pub fn try_publish<T, V>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: V,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        V: Into<Vec<u8>>,
    {
        self.handle_try_publish(topic, qos, retain, payload)
    }

    /// Attempts to send a MQTT Publish to the `EventLoop` and returns a notice.
    pub fn try_publish_tracked<T, V>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: V,
    ) -> Result<PublishNotice, ClientError>
    where
        T: Into<PublishTopic>,
        V: Into<Vec<u8>>,
    {
        self.handle_try_publish_tracked(topic, qos, retain, payload)
    }

    /// Prepares a MQTT PubAck/PubRec packet for manual acknowledgement.
    ///
    /// Returns `None` for QoS0 publishes, which do not require acknowledgement.
    pub fn prepare_ack(&self, publish: &Publish) -> Option<ManualAck> {
        prepare_ack(publish)
    }

    /// Sends a prepared MQTT PubAck/PubRec to the `EventLoop`.
    ///
    /// This is useful when `manual_acks` is enabled and acknowledgement must be deferred.
    pub async fn manual_ack(&self, ack: ManualAck) -> Result<(), ClientError> {
        self.send_request_async(ack.into_request()).await?;
        Ok(())
    }

    /// Attempts to send a prepared MQTT PubAck/PubRec to the `EventLoop`.
    pub fn try_manual_ack(&self, ack: ManualAck) -> Result<(), ClientError> {
        self.try_send_request(ack.into_request())?;
        Ok(())
    }

    /// Sends a MQTT PubAck/PubRec to the `EventLoop` based on publish QoS.
    /// Only needed if the `manual_acks` flag is set.
    pub async fn ack(&self, publish: &Publish) -> Result<(), ClientError> {
        if let Some(ack) = self.prepare_ack(publish) {
            self.manual_ack(ack).await?;
        }
        Ok(())
    }

    /// Attempts to send a MQTT PubAck/PubRec to the `EventLoop` based on publish QoS.
    /// Only needed if the `manual_acks` flag is set.
    pub fn try_ack(&self, publish: &Publish) -> Result<(), ClientError> {
        if let Some(ack) = self.prepare_ack(publish) {
            self.try_manual_ack(ack)?;
        }
        Ok(())
    }

    /// Sends a MQTT Publish to the `EventLoop`
    async fn handle_publish_bytes<T>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: Bytes,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_topic(&topic);
        let mut publish = Publish::from_bytes(topic, qos, payload);
        publish.retain = retain;
        let publish = Request::Publish(publish);

        if invalid_topic {
            return Err(ClientError::Request(publish));
        }

        self.send_request_async(publish).await?;
        Ok(())
    }

    async fn handle_publish_bytes_tracked<T>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: Bytes,
    ) -> Result<PublishNotice, ClientError>
    where
        T: Into<PublishTopic>,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_topic(&topic);
        let mut publish = Publish::from_bytes(topic, qos, payload);
        publish.retain = retain;
        let request = Request::Publish(publish.clone());

        if invalid_topic {
            return Err(ClientError::Request(request));
        }

        self.send_tracked_publish_async(publish).await
    }

    /// Sends a MQTT Publish to the `EventLoop`
    pub async fn publish_bytes<T>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: Bytes,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
    {
        self.handle_publish_bytes(topic, qos, retain, payload).await
    }

    /// Sends a MQTT Publish with `Bytes` payload and returns a tracked notice.
    pub async fn publish_bytes_tracked<T>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: Bytes,
    ) -> Result<PublishNotice, ClientError>
    where
        T: Into<PublishTopic>,
    {
        self.handle_publish_bytes_tracked(topic, qos, retain, payload)
            .await
    }

    /// Sends a MQTT Subscribe to the `EventLoop`
    pub async fn subscribe<S: Into<String>>(&self, topic: S, qos: QoS) -> Result<(), ClientError> {
        let subscribe = Subscribe::new(topic, qos);
        if !subscribe_has_valid_filters(&subscribe) {
            return Err(ClientError::Request(subscribe.into()));
        }

        self.send_request_async(subscribe.into()).await?;
        Ok(())
    }

    /// Sends a tracked MQTT Subscribe to the `EventLoop`.
    pub async fn subscribe_tracked<S: Into<String>>(
        &self,
        topic: S,
        qos: QoS,
    ) -> Result<RequestNotice, ClientError> {
        let subscribe = Subscribe::new(topic, qos);
        if !subscribe_has_valid_filters(&subscribe) {
            return Err(ClientError::Request(subscribe.into()));
        }

        self.send_tracked_subscribe_async(subscribe).await
    }

    /// Attempts to send a MQTT Subscribe to the `EventLoop`
    pub fn try_subscribe<S: Into<String>>(&self, topic: S, qos: QoS) -> Result<(), ClientError> {
        let subscribe = Subscribe::new(topic, qos);
        if !subscribe_has_valid_filters(&subscribe) {
            return Err(ClientError::TryRequest(subscribe.into()));
        }

        self.try_send_request(subscribe.into())?;
        Ok(())
    }

    /// Attempts to send a tracked MQTT Subscribe to the `EventLoop`.
    pub fn try_subscribe_tracked<S: Into<String>>(
        &self,
        topic: S,
        qos: QoS,
    ) -> Result<RequestNotice, ClientError> {
        let subscribe = Subscribe::new(topic, qos);
        if !subscribe_has_valid_filters(&subscribe) {
            return Err(ClientError::TryRequest(subscribe.into()));
        }

        self.try_send_tracked_subscribe(subscribe)
    }

    /// Sends a MQTT Subscribe for multiple topics to the `EventLoop`
    pub async fn subscribe_many<T>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = SubscribeFilter>,
    {
        let subscribe = Subscribe::new_many(topics);
        if !subscribe_has_valid_filters(&subscribe) {
            return Err(ClientError::Request(subscribe.into()));
        }

        self.send_request_async(subscribe.into()).await?;
        Ok(())
    }

    /// Sends a tracked MQTT Subscribe for multiple topics to the `EventLoop`.
    pub async fn subscribe_many_tracked<T>(&self, topics: T) -> Result<RequestNotice, ClientError>
    where
        T: IntoIterator<Item = SubscribeFilter>,
    {
        let subscribe = Subscribe::new_many(topics);
        if !subscribe_has_valid_filters(&subscribe) {
            return Err(ClientError::Request(subscribe.into()));
        }

        self.send_tracked_subscribe_async(subscribe).await
    }

    /// Attempts to send a MQTT Subscribe for multiple topics to the `EventLoop`
    pub fn try_subscribe_many<T>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = SubscribeFilter>,
    {
        let subscribe = Subscribe::new_many(topics);
        if !subscribe_has_valid_filters(&subscribe) {
            return Err(ClientError::TryRequest(subscribe.into()));
        }
        self.try_send_request(subscribe.into())?;
        Ok(())
    }

    /// Attempts to send a tracked MQTT Subscribe for multiple topics to the `EventLoop`.
    pub fn try_subscribe_many_tracked<T>(&self, topics: T) -> Result<RequestNotice, ClientError>
    where
        T: IntoIterator<Item = SubscribeFilter>,
    {
        let subscribe = Subscribe::new_many(topics);
        if !subscribe_has_valid_filters(&subscribe) {
            return Err(ClientError::TryRequest(subscribe.into()));
        }

        self.try_send_tracked_subscribe(subscribe)
    }

    /// Sends a MQTT Unsubscribe to the `EventLoop`
    pub async fn unsubscribe<S: Into<String>>(&self, topic: S) -> Result<(), ClientError> {
        let unsubscribe = Unsubscribe::new(topic.into());
        let request = Request::Unsubscribe(unsubscribe);
        self.send_request_async(request).await?;
        Ok(())
    }

    /// Sends a tracked MQTT Unsubscribe to the `EventLoop`.
    pub async fn unsubscribe_tracked<S: Into<String>>(
        &self,
        topic: S,
    ) -> Result<RequestNotice, ClientError> {
        let unsubscribe = Unsubscribe::new(topic.into());
        self.send_tracked_unsubscribe_async(unsubscribe).await
    }

    /// Attempts to send a MQTT Unsubscribe to the `EventLoop`
    pub fn try_unsubscribe<S: Into<String>>(&self, topic: S) -> Result<(), ClientError> {
        let unsubscribe = Unsubscribe::new(topic.into());
        let request = Request::Unsubscribe(unsubscribe);
        self.try_send_request(request)?;
        Ok(())
    }

    /// Attempts to send a tracked MQTT Unsubscribe to the `EventLoop`.
    pub fn try_unsubscribe_tracked<S: Into<String>>(
        &self,
        topic: S,
    ) -> Result<RequestNotice, ClientError> {
        let unsubscribe = Unsubscribe::new(topic.into());
        self.try_send_tracked_unsubscribe(unsubscribe)
    }

    /// Sends a MQTT disconnect to the `EventLoop`
    pub async fn disconnect(&self) -> Result<(), ClientError> {
        let request = Request::Disconnect(Disconnect);
        self.send_request_async(request).await?;
        Ok(())
    }

    /// Attempts to send a MQTT disconnect to the `EventLoop`
    pub fn try_disconnect(&self) -> Result<(), ClientError> {
        let request = Request::Disconnect(Disconnect);
        self.try_send_request(request)?;
        Ok(())
    }
}

fn prepare_ack(publish: &Publish) -> Option<ManualAck> {
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
    /// Create a new `Client`
    ///
    /// `cap` specifies the capacity of the bounded async channel.
    pub fn new(options: MqttOptions, cap: usize) -> (Client, Connection) {
        let (client, eventloop) = AsyncClient::new(options, cap);
        let client = Client { client };
        let runtime = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let connection = Connection::new(eventloop, runtime);
        (client, connection)
    }

    /// Create a new `Client` from a channel `Sender`.
    ///
    /// This is mostly useful for creating a test instance where you can
    /// listen on the corresponding receiver.
    #[must_use]
    pub fn from_sender(request_tx: Sender<Request>) -> Client {
        Client {
            client: AsyncClient::from_senders(request_tx),
        }
    }

    /// Sends a MQTT Publish to the `EventLoop`
    pub fn publish<T, V>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: V,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        V: Into<Vec<u8>>,
    {
        let (topic, needs_validation) = topic.into().into_string_and_validation();
        let invalid_topic = needs_validation && !valid_topic(&topic);
        let mut publish = Publish::new(topic, qos, payload);
        publish.retain = retain;
        let publish = Request::Publish(publish);
        if invalid_topic {
            return Err(ClientError::Request(publish));
        }
        self.client.send_request(publish)?;
        Ok(())
    }

    pub fn try_publish<T, V>(
        &self,
        topic: T,
        qos: QoS,
        retain: bool,
        payload: V,
    ) -> Result<(), ClientError>
    where
        T: Into<PublishTopic>,
        V: Into<Vec<u8>>,
    {
        self.client.try_publish(topic, qos, retain, payload)?;
        Ok(())
    }

    /// Prepares a MQTT PubAck/PubRec packet for manual acknowledgement.
    ///
    /// Returns `None` for QoS0 publishes, which do not require acknowledgement.
    pub fn prepare_ack(&self, publish: &Publish) -> Option<ManualAck> {
        self.client.prepare_ack(publish)
    }

    /// Sends a prepared MQTT PubAck/PubRec to the `EventLoop`.
    pub fn manual_ack(&self, ack: ManualAck) -> Result<(), ClientError> {
        self.client.send_request(ack.into_request())?;
        Ok(())
    }

    /// Attempts to send a prepared MQTT PubAck/PubRec to the `EventLoop`.
    pub fn try_manual_ack(&self, ack: ManualAck) -> Result<(), ClientError> {
        self.client.try_manual_ack(ack)?;
        Ok(())
    }

    /// Sends a MQTT PubAck/PubRec to the `EventLoop` based on publish QoS.
    /// Only needed if the `manual_acks` flag is set.
    pub fn ack(&self, publish: &Publish) -> Result<(), ClientError> {
        if let Some(ack) = self.prepare_ack(publish) {
            self.manual_ack(ack)?;
        }
        Ok(())
    }

    /// Attempts to send a MQTT PubAck/PubRec to the `EventLoop` based on publish QoS.
    /// Only needed if the `manual_acks` flag is set.
    pub fn try_ack(&self, publish: &Publish) -> Result<(), ClientError> {
        if let Some(ack) = self.prepare_ack(publish) {
            self.try_manual_ack(ack)?;
        }
        Ok(())
    }

    /// Sends a MQTT Subscribe to the `EventLoop`
    pub fn subscribe<S: Into<String>>(&self, topic: S, qos: QoS) -> Result<(), ClientError> {
        let subscribe = Subscribe::new(topic, qos);
        if !subscribe_has_valid_filters(&subscribe) {
            return Err(ClientError::Request(subscribe.into()));
        }

        self.client.send_request(subscribe.into())?;
        Ok(())
    }

    /// Sends a MQTT Subscribe to the `EventLoop`
    pub fn try_subscribe<S: Into<String>>(&self, topic: S, qos: QoS) -> Result<(), ClientError> {
        self.client.try_subscribe(topic, qos)?;
        Ok(())
    }

    /// Sends a MQTT Subscribe for multiple topics to the `EventLoop`
    pub fn subscribe_many<T>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = SubscribeFilter>,
    {
        let subscribe = Subscribe::new_many(topics);
        if !subscribe_has_valid_filters(&subscribe) {
            return Err(ClientError::Request(subscribe.into()));
        }

        self.client.send_request(subscribe.into())?;
        Ok(())
    }

    pub fn try_subscribe_many<T>(&self, topics: T) -> Result<(), ClientError>
    where
        T: IntoIterator<Item = SubscribeFilter>,
    {
        self.client.try_subscribe_many(topics)
    }

    /// Sends a MQTT Unsubscribe to the `EventLoop`
    pub fn unsubscribe<S: Into<String>>(&self, topic: S) -> Result<(), ClientError> {
        let unsubscribe = Unsubscribe::new(topic.into());
        let request = Request::Unsubscribe(unsubscribe);
        self.client.send_request(request)?;
        Ok(())
    }

    /// Sends a MQTT Unsubscribe to the `EventLoop`
    pub fn try_unsubscribe<S: Into<String>>(&self, topic: S) -> Result<(), ClientError> {
        self.client.try_unsubscribe(topic)?;
        Ok(())
    }

    /// Sends a MQTT disconnect to the `EventLoop`
    pub fn disconnect(&self) -> Result<(), ClientError> {
        let request = Request::Disconnect(Disconnect);
        self.client.send_request(request)?;
        Ok(())
    }

    /// Sends a MQTT disconnect to the `EventLoop`
    pub fn try_disconnect(&self) -> Result<(), ClientError> {
        self.client.try_disconnect()?;
        Ok(())
    }
}

#[must_use]
fn subscribe_has_valid_filters(subscribe: &Subscribe) -> bool {
    !subscribe.filters.is_empty()
        && subscribe
            .filters
            .iter()
            .all(|filter| valid_filter(&filter.path))
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
#[derive(Debug, Eq, PartialEq)]
pub enum RecvTimeoutError {
    /// User has closed requests channel
    Disconnected,
    /// Recv request timedout
    Timeout,
}

///  MQTT connection. Maintains all the necessary state
pub struct Connection {
    pub eventloop: EventLoop,
    runtime: Runtime,
}
impl Connection {
    fn new(eventloop: EventLoop, runtime: Runtime) -> Connection {
        Connection { eventloop, runtime }
    }

    /// Returns an iterator over this connection. Iterating over this is all that's
    /// necessary to make connection progress and maintain a robust connection.
    /// Just continuing to loop will reconnect
    /// **NOTE** Don't block this while iterating
    // ideally this should be named iter_mut because it requires a mutable reference
    // Also we can implement IntoIter for this to make it easy to iterate over it
    #[must_use = "Connection should be iterated over a loop to make progress"]
    pub fn iter(&mut self) -> Iter<'_> {
        Iter { connection: self }
    }

    /// Attempt to fetch an incoming [`Event`] on the [`EventLoop`], returning an error
    /// if all clients/users have closed requests channel.
    ///
    /// [`EventLoop`]: super::EventLoop
    pub fn recv(&mut self) -> Result<Result<Event, ConnectionError>, RecvError> {
        let f = self.eventloop.poll();
        let event = self.runtime.block_on(f);

        resolve_event(event).ok_or(RecvError)
    }

    /// Attempt to fetch an incoming [`Event`] on the [`EventLoop`], returning an error
    /// if none immediately present or all clients/users have closed requests channel.
    ///
    /// [`EventLoop`]: super::EventLoop
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
    pub fn recv_timeout(
        &mut self,
        duration: Duration,
    ) -> Result<Result<Event, ConnectionError>, RecvTimeoutError> {
        let f = self.eventloop.poll();
        let event = self
            .runtime
            .block_on(async { timeout(duration, f).await })
            .map_err(|_| RecvTimeoutError::Timeout)?;

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

    #[test]
    fn calling_iter_twice_on_connection_shouldnt_panic() {
        let mut mqttoptions = MqttOptions::new("test-1", "localhost", 1883);
        let will = LastWill::new("hello/world", "good bye", QoS::AtMostOnce, false);
        mqttoptions.set_keep_alive(5).set_last_will(will);

        let (_, mut connection) = Client::new(mqttoptions, 10);
        let _ = connection.iter();
        let _ = connection.iter();
    }

    #[test]
    fn should_be_able_to_build_test_client_from_channel() {
        let (tx, rx) = flume::bounded(1);
        let client = Client::from_sender(tx);
        client
            .publish("hello/world", QoS::ExactlyOnce, false, "good bye")
            .expect("Should be able to publish");
        let _ = rx.try_recv().expect("Should have message");
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
            .publish(valid_topic, QoS::ExactlyOnce, false, "good bye")
            .expect("Should be able to publish");
        let _ = rx.try_recv().expect("Should have message");
    }

    #[test]
    fn publish_accepts_borrowed_string_topic() {
        let (tx, rx) = flume::bounded(2);
        let client = Client::from_sender(tx);
        let topic = "hello/world".to_string();
        client
            .publish(&topic, QoS::ExactlyOnce, false, "good bye")
            .expect("Should be able to publish");
        client
            .try_publish(&topic, QoS::ExactlyOnce, false, "good bye")
            .expect("Should be able to publish");
        let _ = rx.try_recv().expect("Should have message");
        let _ = rx.try_recv().expect("Should have message");
    }

    #[test]
    fn publish_accepts_cow_topic_variants() {
        let (tx, rx) = flume::bounded(2);
        let client = Client::from_sender(tx);
        client
            .publish(
                std::borrow::Cow::Borrowed("hello/world"),
                QoS::ExactlyOnce,
                false,
                "good bye",
            )
            .expect("Should be able to publish");
        client
            .try_publish(
                std::borrow::Cow::Owned("hello/world".to_owned()),
                QoS::ExactlyOnce,
                false,
                "good bye",
            )
            .expect("Should be able to publish");
        let _ = rx.try_recv().expect("Should have message");
        let _ = rx.try_recv().expect("Should have message");
    }

    #[test]
    fn publishing_invalid_cow_topic_fails() {
        let (tx, _) = flume::bounded(1);
        let client = Client::from_sender(tx);
        let err = client
            .publish(
                std::borrow::Cow::Borrowed("a/+/b"),
                QoS::ExactlyOnce,
                false,
                "good bye",
            )
            .expect_err("Invalid publish topic should fail");
        assert!(matches!(err, ClientError::Request(req) if matches!(req, Request::Publish(_))));
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
    }

    #[test]
    fn publishing_invalid_raw_topic_fails() {
        let (tx, _) = flume::bounded(1);
        let client = Client::from_sender(tx);
        let err = client
            .publish("a/+/b", QoS::ExactlyOnce, false, "good bye")
            .expect_err("Invalid publish topic should fail");
        assert!(matches!(err, ClientError::Request(req) if matches!(req, Request::Publish(_))));
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
                    QoS::ExactlyOnce,
                    false,
                    "good bye",
                )
                .await
                .expect("Should be able to publish");

            client
                .publish_bytes(
                    ValidatedTopic::new("hello/world").unwrap(),
                    QoS::ExactlyOnce,
                    false,
                    Bytes::from_static(b"good bye"),
                )
                .await
                .expect("Should be able to publish");
        });

        let _ = rx.try_recv().expect("Should have message");
        let _ = rx.try_recv().expect("Should have message");
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
                .publish("a/+/b", QoS::ExactlyOnce, false, "good bye")
                .await
                .expect_err("Invalid publish topic should fail");
            assert!(matches!(err, ClientError::Request(req) if matches!(req, Request::Publish(_))));

            let err = client
                .publish_bytes(
                    "a/+/b",
                    QoS::ExactlyOnce,
                    false,
                    Bytes::from_static(b"good bye"),
                )
                .await
                .expect_err("Invalid publish topic should fail");
            assert!(matches!(err, ClientError::Request(req) if matches!(req, Request::Publish(_))));
        });
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
                .publish_tracked("hello/world", QoS::AtLeastOnce, false, "good bye")
                .await
                .expect_err("tracked publish should fail without tracked channel");
            assert!(matches!(err, ClientError::TrackingUnavailable));

            let err = client
                .publish_bytes_tracked(
                    "hello/world",
                    QoS::AtLeastOnce,
                    false,
                    Bytes::from_static(b"good bye"),
                )
                .await
                .expect_err("tracked publish bytes should fail without tracked channel");
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
}
