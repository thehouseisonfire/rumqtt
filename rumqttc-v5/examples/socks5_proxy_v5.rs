use rumqttc::{AsyncClient, MqttOptions, Proxy};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut options = MqttOptions::new("rumqtt-socks5-v5", "broker.example.com");
    options.set_proxy(
        Proxy::socks5("127.0.0.1", 1080).with_credentials("proxy-user", "proxy-password"),
    );

    let (_client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
    while let Ok(event) = eventloop.poll().await {
        println!("{event:?}");
    }
}
