use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use futures_util::FutureExt;
use napi::Env;
use napi::bindgen_prelude::{BigInt, Uint8Array};
use napi_derive::napi;
use rumqttc_wrapper_core::{Command, NativeClient};
use serde_json::json;
use tokio::sync::Mutex;

use crate::command;
use crate::completion;
use crate::config;
use crate::error::{internal_panic, napi_error, response_error};
use crate::event::{self, AckRegistry};

#[napi]
pub struct NativeMqttClient {
    handle: rumqttc_wrapper_core::ClientHandle,
    connection: rumqttc_wrapper_core::ConnectionHandle,
    closer: rumqttc_wrapper_core::NativeClientCloser,
    events: Arc<Mutex<rumqttc_wrapper_core::EventConsumer>>,
    acknowledgements: Arc<AckRegistry>,
    cleanup: Arc<ClientCleanup>,
    boundary_panicked: AtomicBool,
    panic_event_reported: AtomicBool,
    _native: NativeClient,
}

static ACTIVE_NATIVE_CLIENTS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "panic-testing")]
pub(crate) fn active_native_clients() -> usize {
    ACTIVE_NATIVE_CLIENTS.load(Ordering::Acquire)
}

#[allow(dead_code)] // Used from the Node-API module environment hook in cdylib builds.
pub struct ClientCleanup {
    handle: rumqttc_wrapper_core::ClientHandle,
    closer: rumqttc_wrapper_core::NativeClientCloser,
}

#[allow(dead_code)]
impl ClientCleanup {
    fn signal(&self) {
        self.handle.close_now_idempotent();
    }

    fn join(&self, timeout: Duration) {
        let _ = self.closer.close_now(timeout);
    }
}

#[derive(Default)]
pub struct EnvironmentClients {
    clients: StdMutex<Vec<Weak<ClientCleanup>>>,
}

#[allow(dead_code)]
impl EnvironmentClients {
    pub(crate) fn register(&self, client: &Arc<ClientCleanup>) {
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clients.retain(|client| client.strong_count() != 0);
        clients.push(Arc::downgrade(client));
    }

    pub(crate) fn shutdown(&self) {
        let clients = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        for client in &clients {
            client.signal();
        }
        let started = std::time::Instant::now();
        let timeout = Duration::from_secs(5);
        for client in clients {
            client.join(timeout.saturating_sub(started.elapsed()));
        }
    }
}

#[napi]
impl NativeMqttClient {
    #[napi(constructor)]
    pub fn new(env: Env, config_json: String) -> napi::Result<Self> {
        catch_unwind(AssertUnwindSafe(|| Self::new_inner(env, config_json)))
            .unwrap_or_else(|_| Err(napi_error(internal_panic("native constructor panicked"))))
    }

    fn new_inner(env: Env, config_json: String) -> napi::Result<Self> {
        let config = config::parse(&config_json).map_err(napi_error)?;
        let mut native = NativeClient::start(config).map_err(napi_error)?;
        let handle = native.handle();
        let connection = native.connection();
        let closer = native.closer();
        let cleanup = Arc::new(ClientCleanup {
            handle: handle.clone(),
            closer: closer.clone(),
        });
        let environment = env
            .get_instance_data::<Arc<EnvironmentClients>>()?
            .ok_or_else(|| napi_error("addon environment cleanup state is unavailable"))?;
        environment.register(&cleanup);
        let events = native
            .take_events()
            .ok_or_else(|| napi_error("native event consumer is unavailable"))?;
        let client = Self {
            handle,
            connection,
            closer,
            events: Arc::new(Mutex::new(events)),
            acknowledgements: Arc::new(AckRegistry::default()),
            cleanup,
            boundary_panicked: AtomicBool::new(false),
            panic_event_reported: AtomicBool::new(false),
            _native: native,
        };
        ACTIVE_NATIVE_CLIENTS.fetch_add(1, Ordering::AcqRel);
        Ok(client)
    }

    async fn guard<F>(&self, future: F) -> String
    where
        F: Future<Output = String>,
    {
        if let Ok(response) = AssertUnwindSafe(future).catch_unwind().await { response } else {
            self.boundary_panicked.store(true, Ordering::Release);
            self.cleanup.signal();
            internal_panic("native asynchronous boundary panicked")
        }
    }

    #[napi]
    pub async fn connect(&self) -> String {
        self.guard(async {
            match self.connection.wait_async().await {
                Ok(result) => json!({
                    "ok": true,
                    "protocol": match result.protocol {
                        rumqttc_wrapper_core::ProtocolVersion::V4 => "3.1.1",
                        rumqttc_wrapper_core::ProtocolVersion::V5 => "5.0",
                    },
                    "sessionPresent": result.session_present,
                })
                .to_string(),
                Err(error) => response_error(&error, None),
            }
        })
        .await
    }

    #[napi]
    pub async fn enqueue_publish(
        &self,
        topic: String,
        payload: Uint8Array,
        options_json: Option<String>,
    ) -> String {
        self.guard(async {
            let command = match command::publish(topic, payload.to_vec(), options_json.as_deref()) {
                Ok(command) => command,
                Err(error) => return local_error(error),
            };
            match self.handle.admit_async(Command::Publish(command)).await {
                Ok(admission) => completion::admission(&admission),
                Err(error) => response_error(&error, None),
            }
        })
        .await
    }

    #[napi]
    pub async fn publish(
        &self,
        topic: String,
        payload: Uint8Array,
        options_json: Option<String>,
    ) -> String {
        self.guard(async {
            let command = match command::publish(topic, payload.to_vec(), options_json.as_deref()) {
                Ok(command) => command,
                Err(error) => return local_error(error),
            };
            match self.handle.admit_async(Command::Publish(command)).await {
                Ok(admission) => completion::wait(admission).await,
                Err(error) => response_error(&error, None),
            }
        })
        .await
    }

    #[napi]
    pub async fn subscribe(&self, filters_json: String, options_json: Option<String>) -> String {
        self.guard(async {
            let command = match command::subscribe(&filters_json, options_json.as_deref()) {
                Ok(command) => command,
                Err(error) => return local_error(error),
            };
            match self.handle.admit_async(Command::Subscribe(command)).await {
                Ok(admission) => completion::wait(admission).await,
                Err(error) => response_error(&error, None),
            }
        })
        .await
    }

    #[napi]
    pub async fn unsubscribe(&self, filters_json: String, options_json: Option<String>) -> String {
        self.guard(async {
            let command = match command::unsubscribe(&filters_json, options_json.as_deref()) {
                Ok(command) => command,
                Err(error) => return local_error(error),
            };
            match self.handle.admit_async(Command::Unsubscribe(command)).await {
                Ok(admission) => completion::wait(admission).await,
                Err(error) => response_error(&error, None),
            }
        })
        .await
    }

    #[napi]
    pub async fn acknowledge(&self, ack_id: BigInt) -> String {
        self.guard(async {
            let (_, ack_id, lossless) = ack_id.get_u64();
            if !lossless {
                return local_error("acknowledgement identifier is out of range".to_owned());
            }
            let Some(token) = self.acknowledgements.take(ack_id) else {
                return local_error(
                    "acknowledgement was already consumed or is invalid".to_owned(),
                );
            };
            match self.handle.admit_async(Command::Acknowledge(token)).await {
                Ok(admission) => completion::wait(admission).await,
                Err(error) => response_error(&error, None),
            }
        })
        .await
    }

    #[napi]
    pub async fn next_event(&self) -> String {
        self.guard(async {
            let mut events = self.events.lock().await;
            let response = match events.recv_async().await {
                Ok(Some(value)) => {
                    let encoded = event::encode(value, &self.acknowledgements);
                    let event = serde_json::from_str::<serde_json::Value>(&encoded)
                        .unwrap_or_else(|_| panic_event("event JSON conversion failed"));
                    json!({ "ok": true, "done": false, "event": event }).to_string()
                }
                Ok(None) => json!({ "ok": true, "done": true }).to_string(),
                Err(error) => response_error(&error, None),
            };
            if self.boundary_panicked.load(Ordering::Acquire)
                && !self.panic_event_reported.swap(true, Ordering::AcqRel)
            {
                return json!({
                    "ok": true,
                    "done": false,
                    "event": panic_event("native boundary panicked"),
                })
                .to_string();
            }
            response
        })
        .await
    }

    #[napi]
    pub async fn diagnostics(&self) -> String {
        self.guard(async {
            match self.handle.admit_async(Command::Diagnostics).await {
                Ok(admission) => completion::wait(admission).await,
                Err(error) => response_error(&error, None),
            }
        })
        .await
    }

    #[napi]
    pub async fn close(&self, timeout_ms: u32) -> String {
        self.guard(async {
            let closer = self.closer.clone();
            match tokio::task::spawn_blocking(move || {
                closer.close(Duration::from_millis(timeout_ms.into()))
            })
            .await
            {
                Ok(Ok(_)) => json!({ "ok": true }).to_string(),
                Ok(Err(error)) => response_error(&error, None),
                Err(error) => local_error(format!("close task failed: {error}")),
            }
        })
        .await
    }

    #[napi]
    pub async fn close_now(&self, timeout_ms: u32) -> String {
        self.guard(async {
            let closer = self.closer.clone();
            match tokio::task::spawn_blocking(move || {
                closer.close_now(Duration::from_millis(timeout_ms.into()))
            })
            .await
            {
                Ok(Ok(())) => json!({ "ok": true }).to_string(),
                Ok(Err(error)) => response_error(&error, None),
                Err(error) => local_error(format!("immediate-close task failed: {error}")),
            }
        })
        .await
    }
}

#[cfg(feature = "panic-testing")]
#[napi]
impl NativeMqttClient {
    #[napi]
    pub fn inject_sync_panic(&self) -> String {
        let result = catch_unwind(AssertUnwindSafe(|| panic!("synchronous test panic")));
        debug_assert!(result.is_err());
        self.boundary_panicked.store(true, Ordering::Release);
        self.cleanup.signal();
        internal_panic("native synchronous boundary panicked")
    }

    #[napi]
    pub async fn inject_async_panic(&self) -> String {
        self.guard(async { panic!("asynchronous test panic") })
            .await
    }
}

impl Drop for NativeMqttClient {
    fn drop(&mut self) {
        self.cleanup.signal();
        ACTIVE_NATIVE_CLIENTS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn panic_event(message: &str) -> serde_json::Value {
    json!({
        "type": "driverError",
        "error": {
            "code": "INTERNAL_PANIC",
            "kind": "internal",
            "message": message,
            "retryable": false,
            "delivery": "notApplicable",
            "ambiguous": false,
        }
    })
}

fn local_error(message: String) -> String {
    json!({
        "ok": false,
        "error": {
            "code": "COMMAND_INVALID",
            "kind": "admission",
            "message": message,
            "retryable": false,
            "delivery": "notAdmitted",
            "ambiguous": false,
        }
    })
    .to_string()
}
