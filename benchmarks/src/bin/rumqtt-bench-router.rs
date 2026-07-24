use clap::Parser;
use std::net::SocketAddr;

#[derive(Debug, Parser)]
#[command(name = "rumqtt-bench-router")]
#[command(about = "Minimal MQTT 3.1.1/5 TCP router for benchmark isolation")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:1883")]
    bind: SocketAddr,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    let address = listener.local_addr()?;
    println!("{address}");
    benchmarks::synthetic_router::run(listener).await
}
