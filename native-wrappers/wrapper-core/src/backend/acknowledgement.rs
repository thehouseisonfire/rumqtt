use super::PreparedAck;
use crate::Result;
use crate::acknowledgement::{
    AcknowledgementProtocolOptions, V5AcknowledgementOptions, option_error,
};

pub fn customize_ack(
    original: &PreparedAck,
    options: &AcknowledgementProtocolOptions,
    maximum_packet_size: Option<u32>,
) -> Result<PreparedAck> {
    use rumqttc_v5::mqttbytes::v5::{
        PubAckProperties, PubAckReason, PubRecProperties, PubRecReason,
    };
    let mut prepared = original.clone();
    match (&mut prepared, options) {
        (PreparedAck::V4(_), AcknowledgementProtocolOptions::V5(_)) => {
            return Err(option_error(
                "MQTT 5 acknowledgement options require MQTT 5",
            ));
        }
        (PreparedAck::V5(ack), _) => {
            let defaults = V5AcknowledgementOptions::default();
            let content = match options {
                AcknowledgementProtocolOptions::VersionNeutral => &defaults,
                AcknowledgementProtocolOptions::V5(content) => content,
            };
            let size = content.encoded_size()?;
            if maximum_packet_size.is_some_and(|limit| size as u64 > u64::from(limit)) {
                return Err(option_error(
                    "acknowledgement exceeds the broker's Maximum Packet Size",
                ));
            }
            let has_properties =
                content.reason_string.is_some() || !content.user_properties.is_empty();
            // Both packet types share the client-originated code set, but retain native typed reasons.
            macro_rules! reason {
                ($ty:ident) => {
                    match content.reason_code {
                        0x00 => $ty::Success,
                        0x80 => $ty::UnspecifiedError,
                        0x83 => $ty::ImplementationSpecificError,
                        0x87 => $ty::NotAuthorized,
                        0x90 => $ty::TopicNameInvalid,
                        0x91 => $ty::PacketIdentifierInUse,
                        0x97 => $ty::QuotaExceeded,
                        0x99 => $ty::PayloadFormatInvalid,
                        _ => unreachable!("validated client acknowledgement reason"),
                    }
                };
            }
            match ack {
                rumqttc_v5::ManualAck::PubAck(ack) => {
                    ack.reason = reason!(PubAckReason);
                    ack.properties = has_properties.then(|| PubAckProperties {
                        reason_string: content.reason_string.clone(),
                        user_properties: content.user_properties.clone(),
                    });
                }
                rumqttc_v5::ManualAck::PubRec(ack) => {
                    ack.reason = reason!(PubRecReason);
                    ack.properties = has_properties.then(|| PubRecProperties {
                        reason_string: content.reason_string.clone(),
                        user_properties: content.user_properties.clone(),
                    });
                }
            }
        }
        (PreparedAck::V4(_), AcknowledgementProtocolOptions::VersionNeutral) => {}
    }
    Ok(prepared)
}
