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
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OnceCell, Semaphore};

const RUNNING: u8 = 0;
const GRACEFUL_SHUTDOWN: u8 = 1;
const IMMEDIATE_SHUTDOWN: u8 = 2;

static NATIVE_BLOCKING_SLOTS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(crate::native_blocking_capacity())));

#[derive(Debug)]
enum NativeBlockingError {
    Timeout,
    Runtime(String),
}

struct ImmediateShutdownGuard {
    handle: rumqttc_wrapper_core::ClientHandle,
    armed: bool,
}

impl ImmediateShutdownGuard {
    fn new(handle: rumqttc_wrapper_core::ClientHandle) -> Self {
        Self {
            handle,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn escalate(&mut self) {
        if std::mem::replace(&mut self.armed, false) {
            self.handle.close_now_idempotent();
        }
    }
}

impl Drop for ImmediateShutdownGuard {
    fn drop(&mut self) {
        self.escalate();
    }
}

impl std::fmt::Display for NativeBlockingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => {
                formatter.write_str("native blocking operation exceeded its timeout budget")
            }
            Self::Runtime(error) => formatter.write_str(error),
        }
    }
}

async fn run_native_blocking<T, F>(
    budget: Option<Duration>,
    operation: F,
) -> Result<T, NativeBlockingError>
where
    T: Send + 'static,
    F: FnOnce(Option<Duration>) -> T + Send + 'static,
{
    run_native_blocking_on(Arc::clone(&NATIVE_BLOCKING_SLOTS), budget, operation).await
}

async fn run_native_blocking_on<T, F>(
    semaphore: Arc<Semaphore>,
    budget: Option<Duration>,
    operation: F,
) -> Result<T, NativeBlockingError>
where
    T: Send + 'static,
    F: FnOnce(Option<Duration>) -> T + Send + 'static,
{
    let deadline = budget.map(|timeout| tokio::time::Instant::now() + timeout);
    let acquire = semaphore.acquire_owned();
    let permit = match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, acquire)
            .await
            .map_err(|_| NativeBlockingError::Timeout)?,
        None => acquire.await,
    }
    .map_err(|error| NativeBlockingError::Runtime(error.to_string()))?;

    let remaining =
        deadline.map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now()));
    if matches!(remaining, Some(timeout) if timeout.is_zero()) {
        return Err(NativeBlockingError::Timeout);
    }

    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation(
            deadline
                .map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now())),
        )
    });
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, task)
            .await
            .map_err(|_| NativeBlockingError::Timeout)?,
        None => task.await,
    }
    .map_err(|error| NativeBlockingError::Runtime(error.to_string()))
}

fn native_blocking_timeout(message: &'static str) -> rumqttc_wrapper_core::Error {
    rumqttc_wrapper_core::Error::new(ErrorKind::Timeout, message)
        .with_delivery(DeliveryStatus::Ambiguous)
}

struct Started {
    handle: rumqttc_wrapper_core::ClientHandle,
    connection: rumqttc_wrapper_core::ConnectionHandle,
    closer: rumqttc_wrapper_core::NativeClientCloser,
    events: Mutex<rumqttc_wrapper_core::EventConsumer>,
    acks: AckRegistry,
    _native: NativeClient,
}
pub(crate) struct State {
    config: ClientConfig,
    started: OnceCell<Arc<Started>>,
    start_requested: AtomicBool,
    shutdown: AtomicU8,
    boundary_panicked: AtomicBool,
}
impl State {
    fn internal_panic(&self) {
        self.boundary_panicked.store(true, Ordering::SeqCst);
        if let Some(started) = self.started.get() {
            started.handle.terminate_for_internal_panic();
        }
    }

    async fn initialize(&self) -> Result<Arc<Started>, rumqttc_wrapper_core::Error> {
        if self.boundary_panicked.load(Ordering::SeqCst) {
            return Err(internal_panic_error());
        }
        let started = self
            .started
            .get_or_try_init(|| async {
                let cfg = self.config.clone();
                let mut native = run_native_blocking(None, move |_| NativeClient::start(cfg))
                    .await
                    .map_err(|error| {
                        rumqttc_wrapper_core::Error::new(
                            rumqttc_wrapper_core::ErrorKind::Internal,
                            format!("client start task failed: {error}"),
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
            .cloned()?;
        // A boundary panic can race native startup: the panic path may observe an empty cell
        // while `NativeClient::start` is still running. Once the initialized value is published,
        // either this check observes the panic or the panic path observes the populated cell.
        if self.boundary_panicked.load(Ordering::SeqCst) {
            started.handle.terminate_for_internal_panic();
            return Err(internal_panic_error());
        }
        Ok(started)
    }

    async fn start(&self) -> Result<Arc<Started>, rumqttc_wrapper_core::Error> {
        self.start_requested.store(true, Ordering::SeqCst);
        if self.boundary_panicked.load(Ordering::SeqCst) {
            return Err(internal_panic_error());
        }
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
                |result| match result {
                    Ok(started) => Ok(Some((started, timeout.saturating_sub(began.elapsed())))),
                    // NativeClient::start transfers no ownership when construction fails. Closing
                    // such a client is therefore an idempotent no-op rather than a replay of the
                    // original TLS/configuration error.
                    Err(_) if self.started.get().is_none() => Ok(None),
                    Err(error) => Err(error),
                },
            )
    }
}

fn shutdown_error() -> rumqttc_wrapper_core::Error {
    rumqttc_wrapper_core::Error::new(ErrorKind::Shutdown, "client is closing or closed")
        .with_delivery(DeliveryStatus::NotAdmitted)
}

fn internal_panic_error() -> rumqttc_wrapper_core::Error {
    rumqttc_wrapper_core::Error::new(ErrorKind::Internal, "native asynchronous boundary panicked")
        .with_code(rumqttc_wrapper_core::ErrorCode::InternalPanic)
        .with_retryable(false)
}

#[pyclass(module = "rumqttc._native")]
pub struct NativeMqttClient {
    state: Arc<State>,
}

pub fn future<F>(py: Python<'_>, state: Arc<State>, value: F) -> PyResult<Bound<'_, PyAny>>
where
    F: std::future::Future<Output = String> + Send + 'static,
{
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        Ok(AssertUnwindSafe(value)
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                state.internal_panic();
                internal_panic("native asynchronous boundary panicked")
            }))
    })
}

fn tracked_future<F>(py: Python<'_>, state: Arc<State>, value: F) -> PyResult<Bound<'_, PyAny>>
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
                state.internal_panic();
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
                boundary_panicked: AtomicBool::new(false),
            }),
        })
    }
    fn connect<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        future(py, Arc::clone(&s), async move {
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
        future(py, Arc::clone(&s), async move {
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
        tracked_future(py, Arc::clone(&s), async move {
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
                Ok(a) => completion::tracked(a, Arc::clone(&s)),
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
        tracked_future(py, Arc::clone(&s), async move {
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
                Ok(a) => completion::tracked(a, Arc::clone(&s)),
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
        tracked_future(py, Arc::clone(&s), async move {
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
                Ok(a) => completion::tracked(a, Arc::clone(&s)),
                Err(e) => completion::failed(response_error(&e, None)),
            }
        })
    }
    fn acknowledge<'py>(&self, py: Python<'py>, id: u64) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        tracked_future(py, Arc::clone(&s), async move {
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
                    completion::tracked(a, Arc::clone(&s))
                }
                Err(e) => completion::failed(response_error(&e, None)),
            }
        })
    }
    fn next_event<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        future(py, Arc::clone(&s), async move {
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
        future(py, Arc::clone(&s), async move {
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
        future(py, Arc::clone(&s), async move {
            let timeout = Duration::from_millis(timeout_ms);
            let (v, remaining) = match s.begin_shutdown(GRACEFUL_SHUTDOWN, timeout).await {
                Ok(Some(value)) => value,
                Ok(None) => return json!({"ok":true}).to_string(),
                Err(e) => return response_error(&e, None),
            };
            // Cancellation drops this guard at any suspension point below. Since Python has
            // committed the client to closing, cancellation must terminate rather than strand it.
            let mut shutdown_guard = ImmediateShutdownGuard::new(v.handle.clone());
            let c = v.closer.clone();
            let result = run_native_blocking(Some(remaining), move |remaining| {
                c.close(remaining.unwrap_or_default())
            })
            .await;
            if matches!(&result, Ok(Ok(_))) {
                shutdown_guard.disarm();
            } else {
                // Python has committed this client to closing and cannot safely resume it. Ensure
                // any failure to confirm graceful completion cannot leave the native driver live.
                shutdown_guard.escalate();
            }
            match result {
                Ok(Ok(_)) => json!({"ok":true}).to_string(),
                Ok(Err(e)) => response_error(&e, None),
                Err(NativeBlockingError::Timeout) => response_error(
                    &native_blocking_timeout(
                        "native graceful close exceeded its timeout budget; immediate shutdown was requested",
                    ),
                    None,
                ),
                Err(NativeBlockingError::Runtime(error)) => local_error("INTERNAL", error),
            }
        })
    }
    fn close_now<'py>(&self, py: Python<'py>, timeout_ms: u64) -> PyResult<Bound<'py, PyAny>> {
        let s = self.state.clone();
        future(py, Arc::clone(&s), async move {
            let timeout = Duration::from_millis(timeout_ms);
            let (v, remaining) = match s.begin_shutdown(IMMEDIATE_SHUTDOWN, timeout).await {
                Ok(Some(value)) => value,
                Ok(None) => return json!({"ok":true}).to_string(),
                Err(e) => return response_error(&e, None),
            };
            // Dispatch is nonblocking and must not wait behind an unrelated native join. The
            // blocking operation below only reconciles close state and waits for driver exit.
            v.handle.close_now_idempotent();
            let c = v.closer.clone();
            match run_native_blocking(Some(remaining), move |remaining| {
                c.close_now(remaining.unwrap_or_default())
            })
            .await
            {
                Ok(Ok(())) => json!({"ok":true}).to_string(),
                Ok(Err(e)) => response_error(&e, None),
                Err(NativeBlockingError::Timeout) => response_error(
                    &native_blocking_timeout("native immediate close exceeded its timeout budget"),
                    None,
                ),
                Err(NativeBlockingError::Runtime(error)) => local_error("INTERNAL", error),
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

    #[cfg(feature = "benchmark-testing")]
    fn _completion_probe<'py>(
        &self,
        py: Python<'py>,
        value: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future(py, state, async move { value })
    }

    #[cfg(feature = "benchmark-testing")]
    fn _blocking_probe<'py>(
        &self,
        py: Python<'py>,
        duration_ms: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future(py, state, async move {
            match run_native_blocking(None, move |_| {
                std::thread::sleep(Duration::from_millis(duration_ms));
            })
            .await
            {
                Ok(()) => json!({"ok":true}).to_string(),
                Err(error) => local_error("INTERNAL", error.to_string()),
            }
        })
    }

    #[cfg(feature = "panic-testing")]
    fn _inject_async_panic<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future(py, Arc::clone(&state), async move {
            std::panic::panic_any(crate::InjectedAsyncPanic);
        })
    }

    #[cfg(feature = "panic-testing")]
    fn _inject_driver_panic<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future(py, Arc::clone(&state), async move {
            match state.start().await {
                Ok(started) => {
                    started.handle.terminate_for_internal_panic();
                    internal_panic("injected driver-thread panic")
                }
                Err(error) => response_error(&error, None),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    #[test]
    fn timeout_budget_bounds_waiting_for_a_native_blocking_slot() {
        runtime().block_on(async {
            let semaphore = Arc::new(Semaphore::new(1));
            let occupied = Arc::clone(&semaphore).acquire_owned().await.unwrap();
            let called = Arc::new(AtomicBool::new(false));
            let called_by_operation = Arc::clone(&called);
            let started = Instant::now();

            let result =
                run_native_blocking_on(semaphore, Some(Duration::from_millis(25)), move |_| {
                    called_by_operation.store(true, Ordering::Release);
                })
                .await;

            assert!(matches!(result, Err(NativeBlockingError::Timeout)));
            assert!(started.elapsed() < Duration::from_millis(150));
            assert!(!called.load(Ordering::Acquire));
            drop(occupied);
        });
    }

    #[test]
    fn timeout_budget_bounds_a_running_task_without_releasing_its_slot_early() {
        runtime().block_on(async {
            let semaphore = Arc::new(Semaphore::new(1));
            let barrier = Arc::new(Barrier::new(2));
            let operation_barrier = Arc::clone(&barrier);
            let task_semaphore = Arc::clone(&semaphore);
            let task = tokio::spawn(async move {
                run_native_blocking_on(task_semaphore, Some(Duration::from_millis(25)), move |_| {
                    operation_barrier.wait();
                    std::thread::sleep(Duration::from_millis(100));
                })
                .await
            });

            tokio::task::yield_now().await;
            barrier.wait();
            let started = Instant::now();
            let result = task.await.unwrap();

            assert!(matches!(result, Err(NativeBlockingError::Timeout)));
            assert!(started.elapsed() < Duration::from_millis(150));
            assert_eq!(semaphore.available_permits(), 0);
            tokio::time::sleep(Duration::from_millis(125)).await;
            assert_eq!(semaphore.available_permits(), 1);
        });
    }
}
