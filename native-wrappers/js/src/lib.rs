mod client;
mod command;
mod completion;
mod config;
mod error;
mod event;

pub use client::NativeMqttClient;

use std::sync::Arc;

use client::EnvironmentClients;
use napi::Env;
use napi_derive::napi;

#[napi(module_exports)]
#[allow(dead_code)]
fn initialize_environment(env: Env) -> napi::Result<()> {
    let clients = Arc::new(EnvironmentClients::default());
    env.set_instance_data(Arc::clone(&clients), (), |_| {})?;
    // Deno's Node-API compatibility layer currently corrupts teardown state when an asynchronous
    // cleanup hook removes itself. The stable synchronous hook is supported by all three runtimes;
    // cleanup remains bounded by the environment-wide five-second join budget.
    let _hook = env.add_env_cleanup_hook(clients, |clients| {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| clients.shutdown()));
    })?;
    Ok(())
}

#[cfg(feature = "panic-testing")]
#[napi]
pub fn test_active_native_clients() -> u32 {
    client::active_native_clients()
        .try_into()
        .unwrap_or(u32::MAX)
}
