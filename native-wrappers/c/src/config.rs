use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use rumqttc_wrapper_core::{
    AckMode, ClientConfig, ExecutionContext, ProtocolConfig, TlsConfig, TransportConfig,
};

pub struct ConfigHandle {
    inner: Mutex<ConfigState>,
}

#[derive(Clone)]
struct ConfigState {
    config: ClientConfig,
    execution: Option<ExecutionContext>,
    // The legacy C builder keeps its TCP address when selecting a WebSocket URL.
    tcp_broker: rumqttc_wrapper_core::BrokerTarget,
}

impl ConfigHandle {
    pub fn new(protocol: u32) -> Option<Self> {
        let config = match protocol {
            1 => ClientConfig::v4("", "", 1883),
            2 => ClientConfig::v5("", "", 1883),
            _ => return None,
        };
        Some(Self {
            inner: Mutex::new(ConfigState {
                tcp_broker: config.common.broker.clone(),
                config,
                execution: None,
            }),
        })
    }

    #[cfg(test)]
    pub fn clone_config(&self) -> Result<ClientConfig, &'static str> {
        self.inner
            .lock()
            .map(|state| state.config.clone())
            .map_err(|_| "configuration lock is poisoned")
    }

    pub fn snapshot(&self) -> Result<(ClientConfig, Option<ExecutionContext>), &'static str> {
        self.inner
            .lock()
            .map(|state| (state.config.clone(), state.execution.clone()))
            .map_err(|_| "configuration lock is poisoned")
    }

    pub fn set_execution(&self, execution: Option<ExecutionContext>) -> Result<(), &'static str> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "configuration lock is poisoned")?;
        let previous = std::mem::replace(&mut state.execution, execution);
        drop(state);
        drop(previous);
        Ok(())
    }

    pub fn update(
        &self,
        update: impl FnOnce(&mut ClientConfig) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        self.update_with_error(update, || "configuration lock is poisoned")
    }

    pub fn update_with_error<E>(
        &self,
        update: impl FnOnce(&mut ClientConfig) -> Result<(), E>,
        poisoned: impl FnOnce() -> E,
    ) -> Result<(), E> {
        let mut locked = self.inner.lock().map_err(|_| poisoned())?;
        let mut state = locked.clone();
        let previous = state.config.common.broker.clone();
        if let Err(error) = update(&mut state.config) {
            drop(locked);
            drop(state);
            return Err(error);
        }
        if matches!(
            state.config.common.broker,
            rumqttc_wrapper_core::BrokerTarget::Tcp { .. }
        ) {
            state.tcp_broker = state.config.common.broker.clone();
            if matches!(
                state.config.common.transport,
                TransportConfig::WebSocket | TransportConfig::Wss(_)
            ) && matches!(
                previous,
                rumqttc_wrapper_core::BrokerTarget::WebSocket { .. }
            ) {
                state.config.common.broker = previous;
            }
        } else if matches!(
            state.config.common.transport,
            TransportConfig::Tcp | TransportConfig::Tls(_)
        ) {
            state.config.common.broker = state.tcp_broker.clone();
        }
        let previous = std::mem::replace(&mut *locked, state);
        drop(locked);
        drop(previous);
        Ok(())
    }
}

pub fn tls_config(
    ca: Vec<u8>,
    certificate: Vec<u8>,
    key: Vec<u8>,
) -> rumqttc_wrapper_core::Result<TlsConfig> {
    TlsConfig::rustls_pem(
        (!ca.is_empty()).then(|| Bytes::from(ca)),
        (!certificate.is_empty()).then(|| Bytes::from(certificate)),
        (!key.is_empty()).then_some(key),
    )
}

pub fn set_transport_tcp(config: &mut ClientConfig) {
    config.common.transport = TransportConfig::Tcp;
}

pub fn set_transport_tls(config: &mut ClientConfig, tls: TlsConfig) {
    config.common.transport = TransportConfig::Tls(tls);
}

pub fn set_transport_websocket(config: &mut ClientConfig, url: String) {
    config.common.broker = rumqttc_wrapper_core::BrokerTarget::WebSocket { url };
    config.common.transport = TransportConfig::WebSocket;
}

pub fn set_transport_wss(config: &mut ClientConfig, url: String, tls: TlsConfig) {
    config.common.broker = rumqttc_wrapper_core::BrokerTarget::WebSocket { url };
    config.common.transport = TransportConfig::Wss(tls);
}

pub const fn set_keep_alive(config: &mut ClientConfig, seconds: u64) {
    config.common.keep_alive = Duration::from_secs(seconds);
}

pub const fn set_connection_timeout(config: &mut ClientConfig, seconds: u64) {
    config.common.connection_timeout = Duration::from_secs(seconds);
}

pub const fn set_event_delivery_timeout(config: &mut ClientConfig, milliseconds: u64) {
    config.common.event_delivery_timeout = Duration::from_millis(milliseconds);
}

pub const fn set_ack_mode(config: &mut ClientConfig, mode: u32) -> Result<(), &'static str> {
    config.common.ack_mode = match mode {
        0 => AckMode::Automatic,
        1 => AckMode::Manual,
        _ => return Err("unknown acknowledgement mode"),
    };
    Ok(())
}

pub const fn set_v4_clean_session(
    config: &mut ClientConfig,
    clean: bool,
) -> Result<(), &'static str> {
    match &mut config.protocol {
        ProtocolConfig::V4(protocol) => {
            protocol.clean_session = clean;
            Ok(())
        }
        ProtocolConfig::V5(_) => Err("clean session is only valid for MQTT 3.1.1"),
    }
}

pub fn set_v5_session(
    config: &mut ClientConfig,
    clean_start: bool,
    expiry_present: bool,
    expiry: u32,
) -> Result<(), &'static str> {
    match &mut config.protocol {
        ProtocolConfig::V5(protocol) => {
            protocol.clean_start = clean_start;
            protocol.connect_properties.session_expiry_interval = expiry_present.then_some(expiry);
            Ok(())
        }
        ProtocolConfig::V4(_) => Err("clean start and session expiry require MQTT 5"),
    }
}
