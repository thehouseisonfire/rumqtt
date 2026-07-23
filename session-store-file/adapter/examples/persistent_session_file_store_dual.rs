use std::error::Error;

use rumqttc_session_store_file::{v4, v5};
use rumqttc_v4::{MqttOptions as V4Options, SessionStoreKey as V4Key};
use rumqttc_v5::{MqttOptions as V5Options, SessionStoreKey as V5Key};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join("rumqttc-session-store-example");
    std::fs::create_dir_all(&root)?;
    let v4_store = v4::SessionFileStore::open(&root).await?;
    let v5_store = v5::SessionFileStore::open(&root).await?;

    let v4_path = v4_store.checkpoint_path(&V4Key::new("example", "client"))?;
    let v5_path = v5_store.checkpoint_path(&V5Key::new("example", "client"))?;
    let mut v4_options = V4Options::new("client-v4", ("localhost", 1883));
    v4_options
        .set_clean_session(false)
        .set_session_store_scope("example")
        .set_session_store(v4_store);
    let mut v5_options = V5Options::new("client-v5", ("localhost", 1883));
    v5_options
        .set_clean_start(false)
        .set_session_expiry_interval(Some(3_600))
        .set_session_store_scope("example")
        .set_session_store(v5_store);
    println!("MQTT 3.1.1 checkpoint: {}", v4_path.display());
    println!("MQTT 5 checkpoint: {}", v5_path.display());
    Ok(())
}
