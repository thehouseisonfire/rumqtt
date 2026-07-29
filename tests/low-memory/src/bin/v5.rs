use low_memory_acceptance::{
    BoxError, CONNECTION_TIMEOUT_SECS, Config, Harness, INFLIGHT, KEEP_ALIVE_SECS, MAX_PACKET_SIZE,
    MAX_REQUEST_BATCH, Observation, READ_BATCH_SIZE, REQUEST_CAPACITY, SOCKET_BUFFER_SIZE,
};
use rumqttc_v5::{
    AsyncClient, Event, EventLoop, MqttOptions, NetworkOptions, Outgoing, Packet, PublishOptions,
    QoS, mqttbytes::v5::SubscribeReasonCode,
};

struct V5Harness {
    client: AsyncClient,
    eventloop: EventLoop,
    topic: String,
}

impl V5Harness {
    fn new(config: Config) -> Self {
        let mut network = NetworkOptions::new();
        network.set_connection_timeout(CONNECTION_TIMEOUT_SECS);
        network.set_tcp_send_buffer_size(SOCKET_BUFFER_SIZE);
        network.set_tcp_recv_buffer_size(SOCKET_BUFFER_SIZE);

        let mut options = MqttOptions::new(config.client_id, (config.host, config.port));
        options
            .set_keep_alive(KEEP_ALIVE_SECS)
            .set_max_packet_size(Some(MAX_PACKET_SIZE as u32))
            .set_request_channel_capacity(REQUEST_CAPACITY)
            .set_max_request_batch(MAX_REQUEST_BATCH)
            .set_read_batch_size(READ_BATCH_SIZE)
            .set_outgoing_inflight_upper_limit(INFLIGHT)
            .set_network_options(network);

        let (client, eventloop) = AsyncClient::builder(options).build();
        Self {
            client,
            eventloop,
            topic: config.topic,
        }
    }
}

impl Harness for V5Harness {
    fn topic(&self) -> &str {
        &self.topic
    }

    fn subscribe(&self) -> Result<(), BoxError> {
        self.client
            .try_subscribe(self.topic(), QoS::AtLeastOnce)
            .map_err(Into::into)
    }

    fn publish(&self, payload: Vec<u8>) -> Result<(), BoxError> {
        self.client
            .try_publish(self.topic(), payload, PublishOptions::new(QoS::AtLeastOnce))
            .map_err(Into::into)
    }

    fn disconnect(&self) -> Result<(), BoxError> {
        self.client.try_disconnect().map_err(Into::into)
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
            Event::Incoming(Packet::PubAck(_)) => Observation::PublishAck,
            Event::Incoming(Packet::Publish(publish)) if publish.topic == self.topic => {
                Observation::IncomingPublish(publish.payload.to_vec())
            }
            Event::Incoming(Packet::Publish(publish)) => {
                return Err(
                    format!("received publish on unexpected topic {:?}", publish.topic).into(),
                );
            }
            Event::Incoming(Packet::PingResp) => Observation::PingResponse,
            Event::Outgoing(Outgoing::Publish(_)) => Observation::OutgoingPublish,
            Event::Outgoing(Outgoing::PingReq) => Observation::PingRequest,
            Event::Outgoing(Outgoing::Disconnect) => Observation::GracefulDisconnect,
            _ => Observation::Other,
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), BoxError> {
    let config = Config::from_env("v5")?;
    let mut harness = V5Harness::new(config);
    low_memory_acceptance::run(&mut harness, "v5").await
}
