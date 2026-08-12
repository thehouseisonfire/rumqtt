use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::operations::OperationRegistry;
use crate::{
    AckToken, Admission, Completion, DeliveryStatus, Error, ErrorKind, OperationId, Result,
};

#[derive(Clone)]
pub(crate) enum PreparedAck {
    V311(rumqttc_v4::ManualAck),
    V5(rumqttc_v5::ManualAck),
}

impl PreparedAck {
    pub(crate) const fn key(&self) -> AckKey {
        match self {
            Self::V311(rumqttc_v4::ManualAck::PubAck(ack)) => AckKey::V311PubAck(ack.pkid),
            Self::V311(rumqttc_v4::ManualAck::PubRec(ack)) => AckKey::V311PubRec(ack.pkid),
            Self::V5(rumqttc_v5::ManualAck::PubAck(ack)) => AckKey::V5PubAck(ack.pkid),
            Self::V5(rumqttc_v5::ManualAck::PubRec(ack)) => AckKey::V5PubRec(ack.pkid),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AckKey {
    V311PubAck(u16),
    V311PubRec(u16),
    V5PubAck(u16),
    V5PubRec(u16),
}

#[derive(Default)]
struct AckState {
    by_token: HashMap<AckToken, PreparedAck>,
    by_key: HashMap<AckKey, AckToken>,
    completions: HashMap<AckKey, OperationId>,
}

pub(crate) struct AcknowledgementCoordinator {
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

    pub(crate) fn begin_connection(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    pub(crate) fn reserve(self: &Arc<Self>, token: AckToken) -> Result<AckReservation> {
        if token.client != self.client_identity
            || token.generation != self.generation.load(Ordering::Acquire)
        {
            return Err(option_error(
                "acknowledgement token is stale or belongs to another client",
            ));
        }
        let ack = {
            let mut state = self.state.lock().map_err(|_| {
                Error::new(ErrorKind::Internal, "acknowledgement state mutex poisoned")
            })?;
            let ack = state.by_token.remove(&token).ok_or_else(|| {
                option_error("acknowledgement token is unknown, reserved, or already consumed")
            })?;
            state.by_key.remove(&ack.key());
            ack
        };
        Ok(AckReservation {
            coordinator: Arc::clone(self),
            token,
            ack: Some(ack),
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
        self.generation.fetch_add(1, Ordering::AcqRel);
        let completions = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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

pub(crate) struct AckReservation {
    coordinator: Arc<AcknowledgementCoordinator>,
    token: AckToken,
    ack: Option<PreparedAck>,
}

impl AckReservation {
    pub(crate) fn ack(&self) -> &PreparedAck {
        self.ack.as_ref().expect("active ACK reservation")
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

    fn v4_puback(packet_id: u16) -> PreparedAck {
        PreparedAck::V311(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(
            packet_id,
        )))
    }

    #[test]
    fn dropped_reservation_restores_single_use_token() {
        let (operations, _) = OperationRegistry::new(1);
        let coordinator = AcknowledgementCoordinator::new(7, operations);
        coordinator.begin_connection();
        let token = coordinator.insert(v4_puback(3)).unwrap();
        drop(coordinator.reserve(token).unwrap());
        coordinator.reserve(token).unwrap().commit();
        assert!(coordinator.reserve(token).is_err());
    }

    #[test]
    fn insertion_deduplicates_retransmissions_until_reservation() {
        let (operations, _) = OperationRegistry::new(1);
        let coordinator = AcknowledgementCoordinator::new(7, operations);
        coordinator.begin_connection();

        let first = coordinator.insert(v4_puback(3)).unwrap();
        let retransmission = coordinator.insert(v4_puback(3)).unwrap();

        assert_eq!(first, retransmission);
        coordinator.reserve(first).unwrap().commit();
        assert!(coordinator.reserve(retransmission).is_err());
    }

    #[test]
    fn tracking_completion_and_rollback_preserve_exactly_once_resolution() {
        let (operations, _) = OperationRegistry::new(1);
        let coordinator = AcknowledgementCoordinator::new(7, operations);
        let key = AckKey::V311PubAck(3);

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
        coordinator.begin_connection();
        let token = coordinator.insert(v4_puback(3)).unwrap();
        coordinator.reserve(token).unwrap().commit();
        let tracked = coordinator.track(AckKey::V311PubAck(3)).unwrap();
        let error = Error::new(ErrorKind::Network, "connection lost");

        coordinator.invalidate(&error);

        let failure = tracked.completion.wait().unwrap_err();
        assert_eq!(failure.kind(), ErrorKind::Network);
        assert_eq!(failure.delivery_status(), DeliveryStatus::Ambiguous);
        assert!(coordinator.reserve(token).is_err());
    }
}
