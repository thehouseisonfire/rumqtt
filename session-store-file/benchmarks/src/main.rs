#![expect(clippy::cast_precision_loss)]
#![expect(clippy::too_many_lines)]

use std::collections::BTreeMap;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;

mod persistence;

#[derive(Parser, Debug)]
#[command(name = "rumqtt-session-store-file-bench")]
#[command(about = "Maintained benchmark harness for rumqtt file-backed session stores")]
struct Cli {
    #[command(subcommand)]
    command: CommandGroup,
}

#[derive(Subcommand, Debug)]
enum CommandGroup {
    Persistence {
        #[command(subcommand)]
        command: persistence::PersistenceCommand,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Protocol {
    V4,
    V5,
}

impl Protocol {
    const fn as_str(self) -> &'static str {
        match self {
            Self::V4 => "v4",
            Self::V5 => "v5",
        }
    }
}

#[derive(Args, Debug, Clone)]
struct CommonArgs {
    #[arg(long, value_enum, default_value = "v5")]
    protocol: Protocol,
    #[arg(long)]
    run_id: Option<String>,
}

#[derive(Debug)]
struct BrokerEndpoint {
    host: String,
    port: u16,
    transport: TransportKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportKind {
    Tcp,
}

#[derive(Serialize)]
struct BenchOutput {
    schema_version: u32,
    run_id: String,
    scenario: String,
    started_at_unix: u64,
    finished_at_unix: u64,
    config: Value,
    metrics: BTreeMap<String, f64>,
    samples: BTreeMap<String, Vec<f64>>,
    environment: Environment,
}

#[derive(Serialize)]
struct Environment {
    git_commit: Option<String>,
    rustc: Option<String>,
    target: String,
    os: String,
    arch: String,
    cpu_count: usize,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CommandGroup::Persistence { command } => persistence::run(command).await,
    }
}

fn parse_endpoint(url: &str) -> anyhow::Result<BrokerEndpoint> {
    let rest = url.strip_prefix("mqtt://").ok_or_else(|| {
        anyhow::anyhow!("persistence MQTT benchmarks require an mqtt:// broker URL")
    })?;
    let host_port = rest.split('/').next().unwrap_or(rest);
    let (host, port) = host_port
        .rsplit_once(':')
        .with_context(|| format!("broker URL must include host and port: {url}"))?;
    let port = port
        .parse::<u16>()
        .with_context(|| format!("invalid broker port in URL: {url}"))?;
    if host.is_empty() {
        bail!("broker URL must include a host: {url}");
    }
    Ok(BrokerEndpoint {
        host: host.to_owned(),
        port,
        transport: TransportKind::Tcp,
    })
}

fn parse_qos(value: &str) -> Result<u8, String> {
    match value {
        "0" => Ok(0),
        "1" => Ok(1),
        "2" => Ok(2),
        _ => Err(format!("QoS must be 0, 1, or 2, got {value}")),
    }
}

const fn v4_qos(qos: u8) -> rumqttc_v4::mqttbytes::QoS {
    match qos {
        0 => rumqttc_v4::mqttbytes::QoS::AtMostOnce,
        1 => rumqttc_v4::mqttbytes::QoS::AtLeastOnce,
        _ => rumqttc_v4::mqttbytes::QoS::ExactlyOnce,
    }
}

const fn v5_qos(qos: u8) -> rumqttc_v5::mqttbytes::QoS {
    match qos {
        0 => rumqttc_v5::mqttbytes::QoS::AtMostOnce,
        1 => rumqttc_v5::mqttbytes::QoS::AtLeastOnce,
        _ => rumqttc_v5::mqttbytes::QoS::ExactlyOnce,
    }
}

fn run_id(input: Option<&str>, prefix: &str) -> String {
    input.map_or_else(
        || format!("{prefix}-{}-{}", unix_secs(), rand::random::<u32>()),
        ToOwned::to_owned,
    )
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn environment() -> Environment {
    Environment {
        git_commit: command_stdout("git", &["rev-parse", "HEAD"]),
        rustc: command_stdout("rustc", &["--version"]),
        target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        cpu_count: std::thread::available_parallelism().map_or(1, usize::from),
    }
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn print_output(output: BenchOutput) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
