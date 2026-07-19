use rumqttc::{AsyncClient, ConnectionError, MqttOptions};
use tracing_subscriber::EnvFilter;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,rumqttc::lifecycle=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let options = MqttOptions::new("tracing-v5", ("localhost", 1883));
    let (_client, mut eventloop) = AsyncClient::builder(options).capacity(100).build();

    loop {
        match eventloop.poll().await {
            Ok(_event) => {}
            Err(ConnectionError::RequestsDone) => break,
            Err(error) => eprintln!("connection error: {error}"),
        }
    }
}
