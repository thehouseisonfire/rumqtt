use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::backend::{AckKey, PreparedAck};
use crate::operations::OperationRegistry;
use crate::validation::validate_mqtt_utf8_string;
use crate::{
    AckToken, Admission, Completion, DeliveryStatus, Error, ErrorKind, OperationId, Result,
};

/// Contents of a manual acknowledgement. Explicit MQTT 5 options are rejected on MQTT 3.1.1.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AcknowledgementProtocolOptions {
    #[default]
    VersionNeutral,
    V5(V5AcknowledgementOptions),
}

/// Legal client-originated PUBACK/PUBREC contents.
///
/// A negative reason terminates delivery; it does not request retry or shared-subscription
/// reassignment. Diagnostic properties are sent to the broker, not the original publisher.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct V5AcknowledgementOptions {
    pub reason_code: u8,
    pub reason_string: Option<String>,
    pub user_properties: Vec<(String, String)>,
}

const MAX_REMAINING_LENGTH: usize = 268_435_455;

impl V5AcknowledgementOptions {
    /// Validates sender role, MQTT strings, and encoding limits without admitting an operation.
    pub fn validate(&self) -> Result<()> {
        self.encoded_size().map(|_| ())
    }

    fn encoded_size(&self) -> Result<usize> {
        if !matches!(
            self.reason_code,
            0x00 | 0x80 | 0x83 | 0x87 | 0x90 | 0x91 | 0x97 | 0x99
        ) {
            return Err(option_error(
                "invalid client-originated PUBACK/PUBREC reason code",
            ));
        }
        let mut properties_len = 0usize;
        let mut add = |length: usize| -> Result<()> {
            properties_len = properties_len
                .checked_add(length)
                .filter(|length| *length <= MAX_REMAINING_LENGTH)
                .ok_or_else(|| {
                    option_error("acknowledgement properties exceed MQTT encoding limits")
                })?;
            Ok(())
        };
        if let Some(reason) = &self.reason_string {
            validate_mqtt_utf8_string(reason, "acknowledgement reason string")?;
            add(3 + reason.len())?;
        }
        for (name, value) in &self.user_properties {
            validate_mqtt_utf8_string(name, "acknowledgement user-property name")?;
            validate_mqtt_utf8_string(value, "acknowledgement user-property value")?;
            add(5 + name.len() + value.len())?;
        }
        if self.reason_code == 0 && properties_len == 0 {
            return Ok(4);
        }
        let remaining_len = 3usize
            .checked_add(variable_integer_size(properties_len))
            .and_then(|length| length.checked_add(properties_len))
            .filter(|length| *length <= MAX_REMAINING_LENGTH)
            .ok_or_else(|| option_error("acknowledgement exceeds MQTT encoding limits"))?;
        Ok(1 + variable_integer_size(remaining_len) + remaining_len)
    }
}

const fn variable_integer_size(value: usize) -> usize {
    match value {
        0..=127 => 1,
        128..=16_383 => 2,
        16_384..=2_097_151 => 3,
        _ => 4,
    }
}

fn customize_ack(
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

#[derive(Default)]
struct AckState {
    by_token: HashMap<AckToken, PreparedAck>,
    by_key: HashMap<AckKey, AckToken>,
    completions: HashMap<AckKey, OperationId>,
    maximum_packet_size: Option<u32>,
}

pub struct AcknowledgementCoordinator {
    client_identity: u64,
    generation: AtomicU64,
    next_serial: AtomicU64,
    state: Mutex<AckState>,
    operations: OperationRegistry,
}

impl AcknowledgementCoordinator {
    pub(crate) fn new(client_identity: u64, operations: OperationRegistry) -> Arc<Self> {
        Arc::new(Self {
            client_identity,
            generation: AtomicU64::new(0),
            next_serial: AtomicU64::new(1),
            state: Mutex::new(AckState::default()),
            operations,
        })
    }

    pub(crate) fn begin_connection(&self, maximum_packet_size: Option<u32>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.generation.fetch_add(1, Ordering::AcqRel);
        state.maximum_packet_size = maximum_packet_size;
        state.by_token.clear();
        state.by_key.clear();
    }

    pub(crate) fn insert(&self, ack: PreparedAck) -> Option<AckToken> {
        let key = ack.key();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(token) = state.by_key.get(&key) {
            return Some(*token);
        }
        if state.completions.contains_key(&key) {
            return None;
        }
        let token = AckToken {
            client: self.client_identity,
            generation: self.generation.load(Ordering::Acquire),
            serial: self.next_serial.fetch_add(1, Ordering::Relaxed),
        };
        state.by_token.insert(token, ack);
        state.by_key.insert(key, token);
        Some(token)
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        token: AckToken,
        options: &AcknowledgementProtocolOptions,
    ) -> Result<AckReservation> {
        let (ack, prepared) = {
            let mut state = self.state.lock().map_err(|_| {
                Error::new(ErrorKind::Internal, "acknowledgement state mutex poisoned")
            })?;
            if token.client != self.client_identity
                || token.generation != self.generation.load(Ordering::Acquire)
            {
                return Err(option_error(
                    "acknowledgement token is stale or belongs to another client",
                ));
            }
            let original = state.by_token.get(&token).ok_or_else(|| {
                option_error("acknowledgement token is unknown, reserved, or already consumed")
            })?;
            let prepared = customize_ack(original, options, state.maximum_packet_size)?;
            let ack = state.by_token.remove(&token).ok_or_else(|| {
                option_error("acknowledgement token is unknown, reserved, or already consumed")
            })?;
            state.by_key.remove(&ack.key());
            (ack, prepared)
        };
        Ok(AckReservation {
            coordinator: Arc::clone(self),
            token,
            ack: Some(ack),
            prepared,
        })
    }

    pub(crate) fn track(&self, key: AckKey) -> Result<Admission> {
        let admission = self.operations.allocate()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "acknowledgement state mutex poisoned"))?;
        if state
            .completions
            .insert(key, admission.operation_id)
            .is_some()
        {
            self.operations.cancel(admission.operation_id);
            return Err(Error::new(
                ErrorKind::Internal,
                "an acknowledgement for this MQTT packet is already pending",
            ));
        }
        Ok(admission)
    }

    pub(crate) fn rollback_tracking(&self, key: AckKey, operation_id: OperationId) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .completions
            .remove(&key);
        self.operations.cancel(operation_id);
    }

    pub(crate) fn complete(&self, key: AckKey) {
        let operation_id = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .completions
            .remove(&key);
        if let Some(operation_id) = operation_id {
            self.operations
                .complete(operation_id, Ok(Completion::Acknowledged));
        }
    }

    pub(crate) fn invalidate(&self, error: &Error) {
        let completions = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.generation.fetch_add(1, Ordering::AcqRel);
            state.maximum_packet_size = None;
            state.by_token.clear();
            state.by_key.clear();
            std::mem::take(&mut state.completions)
        };
        for operation_id in completions.into_values() {
            self.operations.complete(
                operation_id,
                Err(error.clone().with_delivery(DeliveryStatus::Ambiguous)),
            );
        }
    }
}

pub struct AckReservation {
    coordinator: Arc<AcknowledgementCoordinator>,
    token: AckToken,
    ack: Option<PreparedAck>,
    prepared: PreparedAck,
}

impl AckReservation {
    pub(crate) const fn ack(&self) -> &PreparedAck {
        &self.prepared
    }

    pub(crate) fn commit(mut self) {
        self.ack = None;
    }
}

impl Drop for AckReservation {
    fn drop(&mut self) {
        if let Some(ack) = self.ack.take() {
            let mut state = self
                .coordinator
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.token.generation != self.coordinator.generation.load(Ordering::Acquire) {
                return;
            }
            let key = ack.key();
            state.by_token.insert(self.token, ack);
            state.by_key.insert(key, self.token);
        }
    }
}

fn option_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Admission, message).with_delivery(DeliveryStatus::NotAdmitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v5_ack(qos2: bool, packet_id: u16) -> PreparedAck {
        PreparedAck::V5(if qos2 {
            rumqttc_v5::ManualAck::PubRec(rumqttc_v5::PubRec::new(packet_id, None))
        } else {
            rumqttc_v5::ManualAck::PubAck(rumqttc_v5::PubAck::new(packet_id, None))
        })
    }

    #[test]
    fn invalid_content_does_not_reserve_token_or_allocate_operation() {
        for qos2 in [false, true] {
            let (operations, _) = OperationRegistry::new(1);
            let coordinator = AcknowledgementCoordinator::new(7, operations.clone());
            coordinator.begin_connection(Some(16));
            let token = coordinator.insert(v5_ack(qos2, 3)).unwrap();
            for content in [
                V5AcknowledgementOptions {
                    reason_code: 0x10,
                    ..Default::default()
                },
                V5AcknowledgementOptions {
                    reason_code: 0xff,
                    ..Default::default()
                },
                V5AcknowledgementOptions {
                    reason_string: Some("\0".into()),
                    ..Default::default()
                },
                V5AcknowledgementOptions {
                    reason_string: Some("x".repeat(65_536)),
                    ..Default::default()
                },
                V5AcknowledgementOptions {
                    user_properties: vec![("k".into(), "\0".into())],
                    ..Default::default()
                },
                V5AcknowledgementOptions {
                    reason_string: Some("x".repeat(16)),
                    ..Default::default()
                },
            ] {
                assert!(
                    coordinator
                        .reserve(token, &AcknowledgementProtocolOptions::V5(content))
                        .is_err()
                );
                assert_eq!(coordinator.state.lock().unwrap().by_token.len(), 1);
            }
            coordinator
                .reserve(token, &Default::default())
                .unwrap()
                .commit();
            // Failed validation did not even allocate/cancel an operation.
            assert_eq!(operations.allocate().unwrap().operation_id.get(), 1);
        }
    }

    #[test]
    fn explicit_v5_defaults_are_rejected_on_v4() {
        let (operations, _) = OperationRegistry::new(1);
        let coordinator = AcknowledgementCoordinator::new(7, operations);
        coordinator.begin_connection(None);
        let token = coordinator.insert(v4_puback(3)).unwrap();
        assert!(
            coordinator
                .reserve(
                    token,
                    &AcknowledgementProtocolOptions::V5(Default::default())
                )
                .is_err()
        );
        coordinator
            .reserve(token, &Default::default())
            .unwrap()
            .commit();
    }

    #[test]
    fn dropping_customized_reservation_restores_default_contents() {
        let (operations, _) = OperationRegistry::new(1);
        let coordinator = AcknowledgementCoordinator::new(7, operations);
        coordinator.begin_connection(None);
        let token = coordinator.insert(v5_ack(false, 3)).unwrap();
        let options = AcknowledgementProtocolOptions::V5(V5AcknowledgementOptions {
            reason_code: 0x99,
            reason_string: Some("bad payload".into()),
            user_properties: vec![("k".into(), "v".into())],
        });
        drop(coordinator.reserve(token, &options).unwrap());
        let reserved = coordinator.reserve(token, &Default::default()).unwrap();
        let PreparedAck::V5(rumqttc_v5::ManualAck::PubAck(ack)) = reserved.ack() else {
            panic!()
        };
        assert_eq!(ack.reason, rumqttc_v5::PubAckReason::Success);
        assert!(ack.properties.is_none());
    }

    #[test]
    fn size_validation_matches_codec_at_variable_integer_boundaries() {
        for qos2 in [false, true] {
            for length in [
                0, 120, 121, 124, 125, 126, 127, 128, 16_375, 16_380, 16_384, 65_535,
            ] {
                let content = V5AcknowledgementOptions {
                    reason_string: Some("x".repeat(length)),
                    ..Default::default()
                };
                let size = content.encoded_size().unwrap();
                let options = AcknowledgementProtocolOptions::V5(content);
                let prepared =
                    customize_ack(&v5_ack(qos2, 3), &options, Some(size as u32)).unwrap();
                let mut bytes = bytes::BytesMut::new();
                match prepared {
                    PreparedAck::V5(rumqttc_v5::ManualAck::PubAck(ack)) => {
                        ack.write(&mut bytes).unwrap();
                    }
                    PreparedAck::V5(rumqttc_v5::ManualAck::PubRec(ack)) => {
                        ack.write(&mut bytes).unwrap();
                    }
                    _ => panic!(),
                }
                assert_eq!(bytes.len(), size);
                assert!(customize_ack(&v5_ack(qos2, 3), &options, Some(size as u32 - 1)).is_err());
            }
        }
    }

    #[test]
    fn reconnect_changes_limits_and_dropped_old_reservations_stay_invalid() {
        let (operations, _) = OperationRegistry::new(1);
        let coordinator = AcknowledgementCoordinator::new(7, operations);
        coordinator.begin_connection(Some(100));
        let old_token = coordinator.insert(v5_ack(false, 3)).unwrap();
        let reservation = coordinator.reserve(old_token, &Default::default()).unwrap();
        coordinator.invalidate(&Error::new(ErrorKind::Network, "lost connection"));
        coordinator.begin_connection(Some(4));
        let new_token = coordinator.insert(v5_ack(false, 3)).unwrap();
        drop(reservation);
        assert!(coordinator.reserve(old_token, &Default::default()).is_err());
        assert_eq!(coordinator.state.lock().unwrap().by_token.len(), 1);
        let options = AcknowledgementProtocolOptions::V5(V5AcknowledgementOptions {
            reason_code: 0x80,
            ..Default::default()
        });
        assert!(coordinator.reserve(new_token, &options).is_err());
        coordinator
            .reserve(new_token, &Default::default())
            .unwrap()
            .commit();
    }

    fn v4_puback(packet_id: u16) -> PreparedAck {
        crate::backend::test_v4_puback(packet_id)
    }

    #[test]
    fn dropped_reservation_restores_single_use_token() {
        let (operations, _) = OperationRegistry::new(1);
        let coordinator = AcknowledgementCoordinator::new(7, operations);
        coordinator.begin_connection(None);
        let token = coordinator.insert(v4_puback(3)).unwrap();
        drop(coordinator.reserve(token, &Default::default()).unwrap());
        coordinator
            .reserve(token, &Default::default())
            .unwrap()
            .commit();
        assert!(coordinator.reserve(token, &Default::default()).is_err());
    }

    #[test]
    fn insertion_deduplicates_retransmissions_until_reservation() {
        let (operations, _) = OperationRegistry::new(1);
        let coordinator = AcknowledgementCoordinator::new(7, operations);
        coordinator.begin_connection(None);

        let first = coordinator.insert(v4_puback(3)).unwrap();
        let retransmission = coordinator.insert(v4_puback(3)).unwrap();

        assert_eq!(first, retransmission);
        coordinator
            .reserve(first, &Default::default())
            .unwrap()
            .commit();
        assert!(
            coordinator
                .reserve(retransmission, &Default::default())
                .is_err()
        );
    }

    #[test]
    fn tracking_completion_and_rollback_preserve_exactly_once_resolution() {
        let (operations, _) = OperationRegistry::new(1);
        let coordinator = AcknowledgementCoordinator::new(7, operations);
        let key = AckKey::V4PubAck(3);

        let rolled_back = coordinator.track(key).unwrap();
        coordinator.rollback_tracking(key, rolled_back.operation_id);
        let completed = coordinator.track(key).unwrap();
        coordinator.complete(key);
        coordinator.complete(key);

        assert_eq!(
            completed.completion.wait().unwrap(),
            Completion::Acknowledged
        );
    }

    #[test]
    fn connection_invalidation_stales_tokens_and_fails_tracked_acks() {
        let (operations, _) = OperationRegistry::new(1);
        let coordinator = AcknowledgementCoordinator::new(7, operations);
        coordinator.begin_connection(None);
        let token = coordinator.insert(v4_puback(3)).unwrap();
        coordinator
            .reserve(token, &Default::default())
            .unwrap()
            .commit();
        let tracked = coordinator.track(AckKey::V4PubAck(3)).unwrap();
        let error = Error::new(ErrorKind::Network, "connection lost");

        coordinator.invalidate(&error);

        let failure = tracked.completion.wait().unwrap_err();
        assert_eq!(failure.kind(), ErrorKind::Network);
        assert_eq!(failure.delivery_status(), DeliveryStatus::Ambiguous);
        assert!(coordinator.reserve(token, &Default::default()).is_err());
    }
}
