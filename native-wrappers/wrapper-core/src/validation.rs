use crate::{DeliveryStatus, Error, ErrorKind, Result};

pub fn validate_mqtt_utf8_string(value: &str, name: &str) -> Result<()> {
    if value.len() > usize::from(u16::MAX) {
        return Err(protocol_option_error(format!(
            "{name} exceeds the MQTT UTF-8 string limit of {} bytes",
            u16::MAX,
        )));
    }
    if value.contains('\0') {
        return Err(protocol_option_error(format!(
            "{name} cannot contain the null character",
        )));
    }
    Ok(())
}

pub fn protocol_option_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Admission, message).with_delivery(DeliveryStatus::NotAdmitted)
}
