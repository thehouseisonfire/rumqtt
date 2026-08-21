use napi::Error as NapiError;
use rumqttc_wrapper_core::{DeliveryStatus, Error, ErrorKind};
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse<'a> {
    ok: bool,
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody<'a> {
    code: &'static str,
    kind: &'static str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    broker_reason: Option<u8>,
    retryable: bool,
    delivery: &'static str,
    ambiguous: bool,
}

pub(crate) fn response_error(error: &Error, operation_id: Option<u64>) -> String {
    serde_json::to_string(&ErrorResponse {
        ok: false,
        error: ErrorBody {
            code: error.code().as_str(),
            kind: kind(error.kind()),
            message: error.message(),
            operation_id: operation_id.map(|id| id.to_string()),
            broker_reason: error.broker_reason(),
            retryable: error.retryable(),
            delivery: delivery(error.delivery_status()),
            ambiguous: error.delivery_status() == DeliveryStatus::Ambiguous,
        },
    })
    .unwrap_or_else(|_| internal_panic("error response serialization failed"))
}

pub(crate) fn internal_panic(message: &str) -> String {
    serde_json::json!({
        "ok": false,
        "error": {
            "code": "INTERNAL_PANIC",
            "kind": "internal",
            "message": message,
            "retryable": false,
            "delivery": "notApplicable",
            "ambiguous": false,
        }
    })
    .to_string()
}

pub(crate) fn napi_error(error: impl ToString) -> NapiError {
    NapiError::from_reason(error.to_string())
}

const fn kind(kind: ErrorKind) -> &'static str {
    match kind {
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

const fn delivery(status: DeliveryStatus) -> &'static str {
    match status {
        DeliveryStatus::NotApplicable => "notApplicable",
        DeliveryStatus::NotAdmitted => "notAdmitted",
        DeliveryStatus::Rejected => "rejected",
        DeliveryStatus::Ambiguous => "ambiguous",
    }
}
