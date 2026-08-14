use rumqttc_wrapper_core::{
    Admission, Completion, PublishCompletion, SubscribeResult, UnsubscribeResult,
};
use serde_json::{Value, json};

use crate::error::response_error;

pub(crate) fn admission(admission: &Admission) -> String {
    json!({ "ok": true, "operationId": admission.operation_id.get().to_string() }).to_string()
}

pub(crate) async fn wait(admission: Admission) -> String {
    let operation_id = admission.operation_id.get();
    match admission.completion.wait_async().await {
        Ok(completion) => success(operation_id, completion).to_string(),
        Err(error) => response_error(&error, Some(operation_id)),
    }
}

fn success(operation_id: u64, completion: Completion) -> Value {
    let result = match completion {
        Completion::Publish(completion) => json!({
            "type": "publish",
            "milestone": match completion {
                PublishCompletion::Qos0Flushed => "qos0Flushed",
                PublishCompletion::Qos1Acknowledged => "qos1Acknowledged",
                PublishCompletion::Qos2Completed => "qos2Completed",
            }
        }),
        Completion::Subscribe(completion) => json!({
            "type": "subscribe",
            "results": completion.results.into_iter().map(|result| match result {
                SubscribeResult::Granted(qos) => json!({ "granted": true, "qos": qos as u8 }),
                SubscribeResult::Rejected(reason) => json!({ "granted": false, "brokerReason": reason.code }),
            }).collect::<Vec<_>>()
        }),
        Completion::Unsubscribe(completion) => {
            let mut result = json!({ "type": "unsubscribe" });
            if let Some(results) = completion.results {
                result["results"] = json!(
                    results
                        .into_iter()
                        .map(|result| match result {
                            UnsubscribeResult::Success => json!({ "status": "success" }),
                            UnsubscribeResult::NoSubscriptionExisted =>
                                json!({ "status": "noSubscriptionExisted" }),
                            UnsubscribeResult::Rejected(reason) =>
                                json!({ "status": "rejected", "brokerReason": reason.code }),
                        })
                        .collect::<Vec<_>>()
                );
            }
            result
        }
        Completion::Acknowledged => json!({ "type": "acknowledged" }),
        Completion::Diagnostics(diagnostics) => json!({
            "type": "diagnostics",
            "connected": diagnostics.connected,
            "disconnecting": diagnostics.disconnecting,
            "pendingRequests": diagnostics.pending_requests,
            "queuedRequests": diagnostics.queued_requests,
            "inflightPublishes": diagnostics.inflight_publishes,
            "maxInflightPublishes": diagnostics.max_inflight_publishes,
            "pendingSubscribes": diagnostics.pending_subscribes,
            "pendingUnsubscribes": diagnostics.pending_unsubscribes,
            "outboundDrained": diagnostics.outbound_drained,
        }),
        Completion::GracefulShutdown => json!({ "type": "gracefulShutdown" }),
        Completion::ImmediateShutdown => json!({ "type": "immediateShutdown" }),
    };
    json!({ "ok": true, "operationId": operation_id.to_string(), "result": result })
}

#[cfg(test)]
mod tests {
    use rumqttc_wrapper_core::UnsubscribeCompletion;

    use super::*;

    #[test]
    fn v4_unsubscribe_omits_unavailable_per_filter_results() {
        let value = success(
            1,
            Completion::Unsubscribe(UnsubscribeCompletion { results: None }),
        );
        assert!(value["result"].get("results").is_none());
    }

    #[test]
    fn v5_unsubscribe_keeps_an_empty_results_array() {
        let value = success(
            1,
            Completion::Unsubscribe(UnsubscribeCompletion {
                results: Some(Vec::new()),
            }),
        );
        assert_eq!(value["result"]["results"], json!([]));
    }
}
