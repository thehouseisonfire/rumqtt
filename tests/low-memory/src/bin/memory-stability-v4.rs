use low_memory_acceptance::{
    BoxError, CONNECTION_TIMEOUT_SECS, INFLIGHT, KEEP_ALIVE_SECS, MAX_PACKET_SIZE,
    MAX_REQUEST_BATCH, READ_BATCH_SIZE, REQUEST_CAPACITY, SOCKET_BUFFER_SIZE,
    stability::{self, Config, Harness, IdleState, Observation},
};
use rumqttc_v4::{
    AsyncClient, Event, EventLoop, MqttOptions, NetworkOptions, Outgoing, Packet, PublishOptions,
    QoS, mqttbytes::v4::SubscribeReasonCode,
};

struct V4Harness {
    client: AsyncClient,
    eventloop: EventLoop,
    primary_topic: String,
    temporary_topic: String,
}

impl V4Harness {
    fn new(config: &Config) -> Self {
        let mut network = NetworkOptions::new();
        network.set_connection_timeout(CONNECTION_TIMEOUT_SECS);
        network.set_tcp_send_buffer_size(SOCKET_BUFFER_SIZE);
        network.set_tcp_recv_buffer_size(SOCKET_BUFFER_SIZE);
        network.set_tcp_nodelay(true);

        let mut options = MqttOptions::new(&config.client_id, (&config.host, config.port));
        options
            .set_keep_alive(KEEP_ALIVE_SECS)
            .set_max_packet_size(MAX_PACKET_SIZE, MAX_PACKET_SIZE)
            .set_request_channel_capacity(REQUEST_CAPACITY)
            .set_max_request_batch(MAX_REQUEST_BATCH)
            .set_read_batch_size(READ_BATCH_SIZE)
            .set_inflight(INFLIGHT);

        let (client, mut eventloop) = AsyncClient::builder(options).build();
        eventloop.set_network_options(network);
        Self {
            client,
            eventloop,
            primary_topic: config.primary_topic.clone(),
            temporary_topic: config.temporary_topic.clone(),
        }
    }
}

impl Harness for V4Harness {
    fn primary_topic(&self) -> &str {
        &self.primary_topic
    }

    fn temporary_topic(&self) -> &str {
        &self.temporary_topic
    }

    fn subscribe(&self, topic: &str) -> Result<(), BoxError> {
        self.client
            .try_subscribe(topic, QoS::AtLeastOnce)
            .map_err(Into::into)
    }

    fn unsubscribe(&self, topic: &str) -> Result<(), BoxError> {
        self.client.try_unsubscribe(topic).map_err(Into::into)
    }

    fn publish(&self, payload: Vec<u8>) -> Result<(), BoxError> {
        self.client
            .try_publish(
                self.primary_topic(),
                payload,
                PublishOptions::new(QoS::AtLeastOnce),
            )
            .map_err(Into::into)
    }

    fn disconnect(&self) -> Result<(), BoxError> {
        self.client.try_disconnect().map_err(Into::into)
    }

    fn idle_state(&self) -> IdleState {
        let diagnostics = self.eventloop.diagnostics();
        let queues = &diagnostics.queues;
        let outbound = &diagnostics.outbound;
        let idle = diagnostics.connected
            && !diagnostics.disconnecting
            && !diagnostics.disconnect_complete
            && queues.pending_len == 0
            && queues.requests_rx_len == 0
            && queues.control_requests_rx_len == 0
            && queues.immediate_disconnect_rx_len == 0
            && outbound.inflight == 0
            && outbound.packet_identifiers_in_use == 0
            && outbound.pending_subscribe == 0
            && outbound.pending_unsubscribe == 0
            && outbound.incoming_puback == 0
            && outbound.outbound_drained;
        IdleState {
            idle,
            detail: format!(
                "connected={} pending={} request_queue={} control_queue={} inflight={} \
                 packet_ids={} pending_subscribe={} pending_unsubscribe={} incoming_puback={} \
                 outbound_drained={}",
                diagnostics.connected,
                queues.pending_len,
                queues.requests_rx_len,
                queues.control_requests_rx_len,
                outbound.inflight,
                outbound.packet_identifiers_in_use,
                outbound.pending_subscribe,
                outbound.pending_unsubscribe,
                outbound.incoming_puback,
                outbound.outbound_drained
            ),
        }
    }

    async fn poll(&mut self) -> Result<Observation, BoxError> {
        let event = match self.eventloop.poll().await {
            Ok(event) => event,
            Err(error) => return Ok(Observation::Disconnected(error.to_string())),
        };

        Ok(match event {
            Event::Incoming(Packet::ConnAck(_)) => Observation::Connected,
            Event::Incoming(Packet::SubAck(suback))
                if suback.return_codes == [SubscribeReasonCode::Success(QoS::AtLeastOnce)] =>
            {
                Observation::Subscribed
            }
            Event::Incoming(Packet::SubAck(suback)) => {
                return Err(format!("SUBACK rejected QoS 1 subscription: {suback:?}").into());
            }
            Event::Incoming(Packet::UnsubAck(_)) => Observation::Unsubscribed,
            Event::Incoming(Packet::PubAck(_)) => Observation::PublishAck,
            Event::Incoming(Packet::Publish(publish)) => Observation::IncomingPublish {
                topic: publish.topic.to_vec(),
                payload: publish.payload.to_vec(),
            },
            Event::Incoming(Packet::PingResp) => Observation::PingResponse,
            Event::Outgoing(Outgoing::Publish(_)) => Observation::OutgoingPublish,
            Event::Outgoing(Outgoing::PubAck(_)) => Observation::OutgoingPublishAck,
            Event::Outgoing(Outgoing::PingReq) => Observation::PingRequest,
            Event::Outgoing(Outgoing::Disconnect) => Observation::GracefulDisconnect,
            _ => Observation::Other,
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), BoxError> {
    let config = Config::from_env("v4")?;
    println!(
        "profile=memory-stability protocol=v4 warmup_rounds={} measured_rounds={} \
         messages_per_round={} {}",
        config.warmup_rounds,
        config.measured_rounds,
        config.messages_per_round,
        stability::settings_summary()
    );
    let mut harness = V4Harness::new(&config);
    stability::run(&mut harness, "v4", &config).await
}
