use crate::client::State;
use crate::error::response_error;
use pyo3::prelude::*;
use rumqttc_wrapper_core::{
    Admission, Completion, CompletionHandle, PublishCompletion, SubscribeResult, UnsubscribeResult,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn admission(value: &Admission) -> String {
    json!({"ok":true,"operationId":value.operation_id.get().to_string()}).to_string()
}

pub fn tracked(value: Admission, state: Arc<State>) -> (String, Option<NativeCompletion>) {
    let response = admission(&value);
    (
        response,
        Some(NativeCompletion {
            handle: value.completion,
            state,
        }),
    )
}

pub const fn failed(response: String) -> (String, Option<NativeCompletion>) {
    (response, None)
}

async fn wait_handle(handle: CompletionHandle) -> String {
    let id = handle.operation_id().get();
    match handle.wait_async().await {
        Ok(v) => json!({"ok":true,"operationId":id.to_string(),"result":success(v)}).to_string(),
        Err(e) => response_error(&e, Some(id)),
    }
}

pub async fn wait_admission(value: Admission) -> String {
    wait_handle(value.completion).await
}

#[pyclass(module = "rumqttc._native")]
pub struct NativeCompletion {
    handle: CompletionHandle,
    state: Arc<State>,
}

#[pymethods]
impl NativeCompletion {
    #[getter]
    fn operation_id(&self) -> u64 {
        self.handle.operation_id().get()
    }

    fn wait<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = self.handle.clone();
        let state = Arc::clone(&self.state);
        crate::client::future(py, state, async move { wait_handle(handle).await })
    }
}
fn success(value: Completion) -> Value {
    match value {
        Completion::Publish(v) => {
            json!({"type":"publish","milestone":match v{PublishCompletion::Qos0Flushed=>"qos0Flushed",PublishCompletion::Qos1Acknowledged=>"qos1Acknowledged",PublishCompletion::Qos2Completed=>"qos2Completed"}})
        }
        Completion::Subscribe(v) => {
            json!({"type":"subscribe","results":v.results.into_iter().map(|r|match r{SubscribeResult::Granted(q)=>json!({"granted":true,"qos":q as u8}),SubscribeResult::Rejected(reason)=>json!({"granted":false,"brokerReason":reason.code})}).collect::<Vec<_>>() })
        }
        Completion::Unsubscribe(v) => {
            let mut out = json!({"type":"unsubscribe"});
            if let Some(values) = v.results {
                out["results"] = json!(
                    values
                        .into_iter()
                        .map(|r| match r {
                            UnsubscribeResult::Success => json!({"status":"success"}),
                            UnsubscribeResult::NoSubscriptionExisted =>
                                json!({"status":"noSubscriptionExisted"}),
                            UnsubscribeResult::Rejected(reason) =>
                                json!({"status":"rejected","brokerReason":reason.code}),
                        })
                        .collect::<Vec<_>>()
                );
            }
            out
        }
        Completion::Acknowledged => json!({"type":"acknowledged"}),
        Completion::Diagnostics(v) => {
            json!({"type":"diagnostics","connected":v.connected,"disconnecting":v.disconnecting,"pendingRequests":v.pending_requests,"queuedRequests":v.queued_requests,"inflightPublishes":v.inflight_publishes,"maxInflightPublishes":v.max_inflight_publishes,"pendingSubscribes":v.pending_subscribes,"pendingUnsubscribes":v.pending_unsubscribes,"outboundDrained":v.outbound_drained})
        }
        Completion::GracefulShutdown => json!({"type":"gracefulShutdown"}),
        Completion::ImmediateShutdown => json!({"type":"immediateShutdown"}),
    }
}
