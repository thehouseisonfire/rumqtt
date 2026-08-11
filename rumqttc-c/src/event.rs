use std::sync::Mutex;

use rumqttc_wrapper_core::{AckToken, WrapperEvent};

pub struct EventObject {
    pub event: WrapperEvent,
    pub ack: Mutex<Option<AckToken>>,
}

impl EventObject {
    pub fn new(event: WrapperEvent) -> Self {
        let ack = match &event {
            WrapperEvent::IncomingPublish(publish) => publish.ack_token,
            _ => None,
        };
        Self {
            event,
            ack: Mutex::new(ack),
        }
    }
}
