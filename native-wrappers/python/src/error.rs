use rumqttc_wrapper_core::{DeliveryStatus, Error, ErrorKind};
use serde_json::json;

pub fn response_error(error: &Error, operation_id: Option<u64>) -> String {
    json!({"ok": false, "error": {
        "code": error.code().as_str(), "kind": kind(error.kind()), "message": error.message(),
        "operationId": operation_id.map(|id| id.to_string()), "brokerReason": error.broker_reason(),
        "retryable": error.retryable(), "delivery": delivery(error.delivery_status()),
        "ambiguous": error.delivery_status() == DeliveryStatus::Ambiguous,
    }})
    .to_string()
}

pub fn local_error(code: &str, message: impl Into<String>) -> String {
    json!({"ok": false, "error": {"code": code, "kind": "admission", "message": message.into(),
        "retryable": false, "delivery": "notAdmitted", "ambiguous": false}})
    .to_string()
}

pub fn internal_panic(message: &str) -> String {
    json!({"ok": false, "error": {"code": "INTERNAL_PANIC", "kind": "internal", "message": message,
        "retryable": false, "delivery": "notApplicable", "ambiguous": false}})
    .to_string()
}

const fn kind(value: ErrorKind) -> &'static str {
    match value {
        ErrorKind::Configuration => "configuration",
        ErrorKind::Admission => "admission",
        ErrorKind::Backpressure => "backpressure",
        ErrorKind::Network => "network",
        ErrorKind::Tls => "tls",
        ErrorKind::Protocol => "protocol",
        ErrorKind::Authentication => "authentication",
        ErrorKind::Persistence => "persistence",
        ErrorKind::Timeout => "timeout",
        ErrorKind::Shutdown => "shutdown",
        ErrorKind::Internal => "internal",
    }
}
const fn delivery(value: DeliveryStatus) -> &'static str {
    match value {
        DeliveryStatus::NotApplicable => "notApplicable",
        DeliveryStatus::NotAdmitted => "notAdmitted",
        DeliveryStatus::Rejected => "rejected",
        DeliveryStatus::Ambiguous => "ambiguous",
    }
}
