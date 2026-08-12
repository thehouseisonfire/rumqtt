use std::collections::HashMap;

use crate::AckToken;

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
pub(crate) struct AcknowledgementRegistry {
    by_token: HashMap<AckToken, PreparedAck>,
    by_key: HashMap<AckKey, AckToken>,
}

impl AcknowledgementRegistry {
    pub(crate) fn clear(&mut self) {
        self.by_token.clear();
        self.by_key.clear();
    }

    pub(crate) fn insert(&mut self, token: AckToken, ack: PreparedAck) {
        let key = ack.key();
        self.by_token.insert(token, ack);
        self.by_key.insert(key, token);
    }

    pub(crate) fn remove(&mut self, token: &AckToken) -> Option<PreparedAck> {
        let ack = self.by_token.remove(token)?;
        self.by_key.remove(&ack.key());
        Some(ack)
    }

    pub(crate) fn token(&self, key: AckKey) -> Option<AckToken> {
        self.by_key.get(&key).copied()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.by_token.len()
    }
}
