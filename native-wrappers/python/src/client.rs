use crate::error::{internal_panic, local_error, response_error};
use crate::event::{self, AckRegistry};
use crate::{command, completion, config};
use futures_util::FutureExt as _;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyAny;
use rumqttc_wrapper_core::{ClientConfig, Command, DeliveryStatus, ErrorKind, NativeClient};
use serde_json::json;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OnceCell};

const RUNNING: u8 = 0;
const GRACEFUL_SHUTDOWN: u8 = 1;
const IMMEDIATE_SHUTDOWN: u8 = 2;

struct Started {
    handle: rumqttc_wrapper_core::ClientHandle,
    connection: rumqttc_wrapper_core::ConnectionHandle,
    closer: rumqttc_wrapper_core::NativeClientCloser,
    events: Mutex<rumqttc_wrapper_core::EventConsumer>,
    acks: AckRegistry,
    _native: NativeClient,
}
struct State {
    config: ClientConfig,
    started: OnceCell<Arc<Started>>,
    start_requested: AtomicBool,
    shutdown: AtomicU8,
}
impl State {
    async fn initialize(&self) -> Result<Arc<Started>, rumqttc_wrapper_core::Error> {
        self.started
            .get_or_try_init(|| async {
                let cfg = self.config.clone();
                let mut native = tokio::task::spawn_blocking(move || NativeClient::start(cfg))
                    .await
                    .map_err(|e| {
                        rumqttc_wrapper_core::Error::new(
                            rumqttc_wrapper_core::ErrorKind::Internal,
                            format!("client start task failed: {e}"),
                        )
                    })??;
                let events = native.take_events().ok_or_else(|| {
                    rumqttc_wrapper_core::Error::new(
                        rumqttc_wrapper_core::ErrorKind::Internal,
                        "native event consumer is unavailable",
                    )
                })?;
                Ok(Arc::new(Started {
                    handle: native.handle(),
                    connection: native.connection(),
                    closer: native.closer(),
                    events: Mutex::new(events),
                    acks: AckRegistry::default(),
                    _native: native,
                }))
            })
            .await
            .cloned()
    }

    async fn start(&self) -> Result<Arc<Started>, rumqttc_wrapper_core::Error> {
        self.start_requested.store(true, Ordering::SeqCst);
        if self.shutdown.load(Ordering::SeqCst) != RUNNING {
            return Err(shutdown_error());
        }

        let started = self.initialize().await?;
        let shutdown = self.shutdown.load(Ordering::SeqCst);
        if shutdown != RUNNING {
            if shutdown == IMMEDIATE_SHUTDOWN {
                started.handle.close_now_idempotent();
            }
            return Err(shutdown_error());
        }
        Ok(started)
    }

    async fn begin_shutdown(
        &self,
        mode: u8,
        timeout: Duration,
    ) -> Result<Option<(Arc<Started>, Duration)>, rumqttc_wrapper_core::Error> {
        self.shutdown.fetch_max(mode, Ordering::SeqCst);

        // A sequentially-consistent ordering makes this check race-safe: a starter that becomes
        // visible after this load must observe the shutdown request before initializing.
        if !self.start_requested.load(Ordering::SeqCst) {
            return Ok(None);
        }

        let began = Instant::now();
        tokio::time::timeout(timeout, self.initialize())
            .await
            .map_or_else(
                |_| {
                    // If graceful shutdown cannot coordinate with startup in time, ensure the starter
                    // requests immediate termination if initialization completes later.
                    self.shutdown.store(IMMEDIATE_SHUTDOWN, Ordering::SeqCst);
                    Err(rumqttc_wrapper_core::Error::new(
                        ErrorKind::Timeout,
                        "native client initialization did not complete before the shutdown timeout",
                    )
                    .with_delivery(DeliveryStatus::Ambiguous))
                },
                |result| {
                    result.map(|started| Some((started, timeout.saturating_sub(began.elapsed()))))
                },
            )
    }
}

fn shutdown_error() -> rumqttc_wrapper_core::Error {
    rumqttc_wrapper_core::Error::new(ErrorKind::Shutdown, "client is closing or closed")
        .with_delivery(DeliveryStatus::NotAdmitted)
}

#[pyclass(module = "rumqttc._native")]
pub struct NativeMqttClient {
    state: Arc<State>,
}

pub fn future<F>(py: Python<'_>, value: F) -> PyResult<Bound<'_, PyAny>>
where
    F: std::future::Future<Output = String> + Send + 'static,
{
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        Ok(AssertUnwindSafe(value)
            .catch_unwind()
            .await
            .unwrap_or_else(|_| internal_panic("native asynchronous boundary panicked")))
    })
}

fn tracked_future<F>(py: Python<'_>, value: F) -> PyResult<Bound<'_, PyAny>>
where
    F: std::future::Future<Output = (String, Option<completion::NativeCompletion>)>
        + Send
        + 'static,
{
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        Ok(AssertUnwindSafe(value)
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                completion::failed(internal_panic("native asynchronous boundary panicked"))
            }))
    })
}

#[pymethods]
impl NativeMqttClient {
    #[new]
    fn new(value: &str) -> PyResult<Self> {
        let cfg = catch_unwind(AssertUnwindSafe(|| config::parse(value)))
            .map_err(|_| PyValueError::new_err("native constructor panicked"))?
            .map_err(PyValueError::new_err)?;
        Ok(Self {
            state: Arc::new(State {
                config: cfg,
                started: OnceCell::new(),
                start_requested: AtomicBool::new(false),
                shutdown: AtomicU8::new(RUNNING),
            }),
        })
    }
    fn connect<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        future(py, async move {
            match s.start().await{Ok(v)=>match v.connection.wait_async().await{Ok(r)=>json!({"ok":true,"protocol":match r.protocol{rumqttc_wrapper_core::ProtocolVersion::V4=>"3.1.1",rumqttc_wrapper_core::ProtocolVersion::V5=>"5.0"},"sessionPresent":r.session_present}).to_string(),Err(e)=>response_error(&e,None)},Err(e)=>response_error(&e,None)}
        })
    }
    fn enqueue_publish<'py>(
        &self,
        py: Python<'py>,
        topic: String,
        payload: Vec<u8>,
        options: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        future(py, async move {
            let Some(v) = s.started.get().cloned() else {
                return local_error(
                    "CLIENT_NOT_CONNECTED",
                    "connect() has not started the client",
                );
            };
            let cmd = match command::publish(topic, payload, options.as_deref()) {
                Ok(v) => v,
                Err(e) => return local_error("COMMAND_INVALID", e),
            };
            match v.handle.admit_async(Command::Publish(cmd)).await {
                Ok(a) => completion::admission(&a),
                Err(e) => response_error(&e, None),
            }
        })
    }
    fn publish<'py>(
        &self,
        py: Python<'py>,
        topic: String,
        payload: Vec<u8>,
        options: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        tracked_future(py, async move {
            let Some(v) = s.started.get().cloned() else {
                return completion::failed(local_error(
                    "CLIENT_NOT_CONNECTED",
                    "connect() has not started the client",
                ));
            };
            let cmd = match command::publish(topic, payload, options.as_deref()) {
                Ok(v) => v,
                Err(e) => return completion::failed(local_error("COMMAND_INVALID", e)),
            };
            match v.handle.admit_async(Command::Publish(cmd)).await {
                Ok(a) => completion::tracked(a),
                Err(e) => completion::failed(response_error(&e, None)),
            }
        })
    }
    fn subscribe<'py>(
        &self,
        py: Python<'py>,
        filters: String,
        options: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        tracked_future(py, async move {
            let Some(v) = s.started.get().cloned() else {
                return completion::failed(local_error(
                    "CLIENT_NOT_CONNECTED",
                    "connect() has not started the client",
                ));
            };
            let cmd = match command::subscribe(&filters, options.as_deref()) {
                Ok(v) => v,
                Err(e) => return completion::failed(local_error("COMMAND_INVALID", e)),
            };
            match v.handle.admit_async(Command::Subscribe(cmd)).await {
                Ok(a) => completion::tracked(a),
                Err(e) => completion::failed(response_error(&e, None)),
            }
        })
    }
    fn unsubscribe<'py>(
        &self,
        py: Python<'py>,
        filters: String,
        options: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        tracked_future(py, async move {
            let Some(v) = s.started.get().cloned() else {
                return completion::failed(local_error(
                    "CLIENT_NOT_CONNECTED",
                    "connect() has not started the client",
                ));
            };
            let cmd = match command::unsubscribe(&filters, options.as_deref()) {
                Ok(v) => v,
                Err(e) => return completion::failed(local_error("COMMAND_INVALID", e)),
            };
            match v.handle.admit_async(Command::Unsubscribe(cmd)).await {
                Ok(a) => completion::tracked(a),
                Err(e) => completion::failed(response_error(&e, None)),
            }
        })
    }
    fn acknowledge<'py>(&self, py: Python<'py>, id: u64) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        tracked_future(py, async move {
            let Some(v) = s.started.get().cloned() else {
                return completion::failed(local_error(
                    "CLIENT_NOT_CONNECTED",
                    "connect() has not started the client",
                ));
            };
            let Some(mut claim) = v.acks.claim(id) else {
                return completion::failed(local_error(
                    "ACKNOWLEDGEMENT_CONSUMED",
                    "acknowledgement was already consumed or invalid",
                ));
            };
            match v
                .handle
                .admit_async(Command::Acknowledge(claim.token()))
                .await
            {
                Ok(a) => {
                    claim.commit();
                    completion::tracked(a)
                }
                Err(e) => completion::failed(response_error(&e, None)),
            }
        })
    }
    fn next_event<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        future(py, async move {
            // Once initialized, the event receiver remains readable through shutdown so callers
            // can drain buffered events, including the terminal Closed event. Only synchronize
            // with startup when initialization has not completed yet.
            let v = if let Some(started) = s.started.get().cloned() {
                started
            } else {
                match s.start().await {
                    Ok(value) => value,
                    Err(error) => return response_error(&error, None),
                }
            };
            match v.events.lock().await.recv_async().await{Ok(Some(e))=>json!({"ok":true,"done":false,"event":serde_json::from_str::<serde_json::Value>(&event::encode(e,&v.acks)).unwrap_or_default()}).to_string(),Ok(None)=>json!({"ok":true,"done":true}).to_string(),Err(e)=>response_error(&e,None)}
        })
    }
    fn diagnostics<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        future(py, async move {
            let Some(v) = s.started.get().cloned() else {
                return local_error(
                    "CLIENT_NOT_CONNECTED",
                    "connect() has not started the client",
                );
            };
            match v.handle.admit_async(Command::Diagnostics).await {
                Ok(a) => completion::wait_admission(a).await,
                Err(e) => response_error(&e, None),
            }
        })
    }
    fn close<'py>(&self, py: Python<'py>, timeout_ms: u64) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        future(py, async move {
            let timeout = Duration::from_millis(timeout_ms);
            let (v, remaining) = match s.begin_shutdown(GRACEFUL_SHUTDOWN, timeout).await {
                Ok(Some(value)) => value,
                Ok(None) => return json!({"ok":true}).to_string(),
                Err(e) => return response_error(&e, None),
            };
            let c = v.closer.clone();
            match tokio::task::spawn_blocking(move || c.close(remaining)).await {
                Ok(Ok(_)) => json!({"ok":true}).to_string(),
                Ok(Err(e)) => response_error(&e, None),
                Err(e) => local_error("INTERNAL", e.to_string()),
            }
        })
    }
    fn close_now<'py>(&self, py: Python<'py>, timeout_ms: u64) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        future(py, async move {
            let timeout = Duration::from_millis(timeout_ms);
            let (v, remaining) = match s.begin_shutdown(IMMEDIATE_SHUTDOWN, timeout).await {
                Ok(Some(value)) => value,
                Ok(None) => return json!({"ok":true}).to_string(),
                Err(e) => return response_error(&e, None),
            };
            let c = v.closer.clone();
            match tokio::task::spawn_blocking(move || c.close_now(remaining)).await {
                Ok(Ok(())) => json!({"ok":true}).to_string(),
                Ok(Err(e)) => response_error(&e, None),
                Err(e) => local_error("INTERNAL", e.to_string()),
            }
        })
    }
    fn abandon(&self) {
        self.state
            .shutdown
            .store(IMMEDIATE_SHUTDOWN, Ordering::SeqCst);
        if let Some(v) = self.state.started.get() {
            v.handle.close_now_idempotent();
        }
    }
    fn cleanup(&self, py: Python<'_>, timeout_ms: u64) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            self.state
                .shutdown
                .store(IMMEDIATE_SHUTDOWN, Ordering::SeqCst);
            if let Some(v) = self.state.started.get() {
                v.handle.close_now_idempotent();
                let closer = v.closer.clone();
                let _ = py.detach(|| closer.close_now(Duration::from_millis(timeout_ms)));
            }
        }));
    }
}
