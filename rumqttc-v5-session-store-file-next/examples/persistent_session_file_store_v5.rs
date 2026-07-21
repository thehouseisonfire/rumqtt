//! Persistent MQTT 5 session using the supported file-store adapter.

use rumqttc::mqttbytes::QoS;
use rumqttc::{AsyncClient, Event, MqttOptions, PublishOptions, SessionStoreKey};
use rumqttc_v5_session_store_file_next::{
    CheckpointState, SessionFileStore, legacy_example_filename,
};
use std::error::Error;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

const CLIENT_ID: &str = "rumqtt-persistent-session-v5";
const SCOPE: &str = "localhost-1884";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();
    let root = std::env::var_os("RUMQTTC_SESSION_STORE").map_or_else(
        || std::env::temp_dir().join("rumqttc-v5-session-store-example"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&root)?;
    let store = SessionFileStore::open(&root).await?;
    if recover_if_requested(&store, &root).await? == RecoveryOutcome::Stop {
        return Ok(());
    }
    println!("Using persistent session store root {}", root.display());

    let mut options = MqttOptions::new(CLIENT_ID, ("localhost", 1884));
    options
        .set_clean_start(false)
        .set_session_expiry_interval(Some(60 * 60))
        .set_session_store_scope(SCOPE)
        .set_keep_alive(5)
        .set_session_store(store);
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
    tokio::spawn(publish(client));

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(packet)) => println!("Incoming = {packet:?}"),
            Ok(Event::Outgoing(packet)) => println!("Outgoing = {packet:?}"),
            Ok(Event::Auth(event)) => println!("Auth = {event:?}"),
            Err(error) => {
                eprintln!(
                    "session/event-loop failure (absence is normal; corruption and legacy files fail closed): {error}"
                );
                return Err(error.into());
            }
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RecoveryOutcome {
    Continue,
    Stop,
}

async fn recover_if_requested(
    store: &SessionFileStore,
    root: &Path,
) -> Result<RecoveryOutcome, Box<dyn Error>> {
    let Ok(action) = std::env::var("RUMQTTC_SESSION_RECOVERY") else {
        return Ok(RecoveryOutcome::Continue);
    };
    if !matches!(action.as_str(), "quarantine" | "clear") {
        return Err(
            io::Error::other("RUMQTTC_SESSION_RECOVERY must be 'quarantine' or 'clear'").into(),
        );
    }
    let key = SessionStoreKey::new(SCOPE, CLIENT_ID);
    match store.inspect(&key).await?.state {
        CheckpointState::LegacyDetected => {
            let path = root.join(legacy_example_filename(&key));
            return Err(io::Error::other(format!(
                "legacy checkpoint detected at {}; move or remove it explicitly, then realign broker state as documented",
                path.display()
            ))
            .into());
        }
        CheckpointState::Absent => {
            println!("No canonical checkpoint exists; no local recovery action was taken");
            print_realign_instructions();
            return Ok(RecoveryOutcome::Stop);
        }
        CheckpointState::Present => {}
    }
    match action.as_str() {
        "quarantine" => println!(
            "quarantined checkpoint: {:?}",
            store.quarantine(&key).await?
        ),
        "clear" => store.operator_clear(&key).await?,
        _ => unreachable!("recovery action was validated above"),
    }
    print_realign_instructions();
    Ok(RecoveryOutcome::Stop)
}

fn print_realign_instructions() {
    println!(
        "Local recovery is complete. Before rerunning this persistent example, connect once with Clean Start=true and an intentional Session Expiry Interval (zero discards the broker-held session)."
    );
}

async fn publish(client: AsyncClient) {
    client
        .subscribe("hello/persistent-session", QoS::AtLeastOnce)
        .await
        .ok();
    for index in 0_u64.. {
        if client
            .publish(
                "hello/persistent-session",
                format!("persistent message {index}"),
                PublishOptions::new(QoS::AtLeastOnce),
            )
            .await
            .is_err()
        {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
