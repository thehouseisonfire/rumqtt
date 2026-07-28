use benchmarks::synthetic_router::{FaultAction, FaultConfig, RouterConfig};
use clap::{Parser, ValueEnum};
use std::net::SocketAddr;
use std::time::Duration;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Fault {
    DropDelivery,
    DuplicateDelivery,
    DelayDelivery,
    RejectPublish,
    DisconnectPublisher,
    WithholdPuback,
}

impl From<Fault> for FaultAction {
    fn from(value: Fault) -> Self {
        match value {
            Fault::DropDelivery => Self::DropDelivery,
            Fault::DuplicateDelivery => Self::DuplicateDelivery,
            Fault::DelayDelivery => Self::DelayDelivery,
            Fault::RejectPublish => Self::RejectPublish,
            Fault::DisconnectPublisher => Self::DisconnectPublisher,
            Fault::WithholdPuback => Self::WithholdPuback,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "rumqtt-bench-router")]
#[command(about = "Minimal MQTT 3.1.1/5 TCP router for benchmark isolation")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:1883")]
    bind: SocketAddr,
    #[arg(long, value_enum, requires = "fault_topic")]
    fault: Option<Fault>,
    #[arg(long, requires = "fault")]
    fault_topic: Option<String>,
    #[arg(long, default_value_t = 1)]
    fault_occurrence: u64,
    #[arg(long, default_value_t = 0)]
    fault_delay_ms: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    let address = listener.local_addr()?;
    println!("{address}");
    let fault = args.fault.map(|action| FaultConfig {
        action: action.into(),
        topic: args.fault_topic.expect("clap requires a fault topic"),
        occurrence: args.fault_occurrence,
        delay: Duration::from_millis(args.fault_delay_ms),
    });
    benchmarks::synthetic_router::run_with_config(listener, RouterConfig { fault }).await
}
