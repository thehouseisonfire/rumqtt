//! Only audited native setters belong here; never replace a live `MqttOptions` object.

use std::sync::Arc;

use crate::configuration_update::ConfigurationDriver;
use crate::{ClientConfig, ProtocolConfig, Result};

enum PreparedTransport {
    V4(rumqttc_v4::Transport),
    V5(rumqttc_v5::Transport),
}

pub struct PreparedProfile {
    transport: PreparedTransport,
    username: Option<String>,
    password: Option<bytes::Bytes>,
    network: rumqttc_core::NetworkOptions,
    timeout: std::time::Duration,
}

pub fn prepare(
    config: &ClientConfig,
    monitor: &Arc<super::TlsCallbackMonitor>,
) -> Result<PreparedProfile> {
    let common = &config.common;
    let transport = match &config.protocol {
        ProtocolConfig::V4(_) => {
            let mut options = super::v4::build_transport_options(common, monitor)?;
            set_v4_credentials(
                &mut options,
                common.username.as_deref(),
                common.password.as_ref(),
            );
            options.validate().map_err(|_| {
                crate::Error::configuration("invalid next-attempt MQTT 3.1.1 profile")
            })?;
            PreparedTransport::V4(options.transport())
        }
        ProtocolConfig::V5(_) => {
            let mut options = super::v5::build_transport_options(common, monitor)?;
            set_v5_credentials(
                &mut options,
                common.username.as_deref(),
                common.password.as_ref(),
            );
            options
                .validate()
                .map_err(|_| crate::Error::configuration("invalid next-attempt MQTT 5 profile"))?;
            PreparedTransport::V5(options.transport())
        }
    };
    Ok(PreparedProfile {
        transport,
        username: common.username.clone(),
        password: common.password.clone(),
        network: super::build_network(common),
        timeout: common.connection_timeout,
    })
}

fn set_v4_credentials(
    options: &mut rumqttc_v4::MqttOptions,
    username: Option<&str>,
    password: Option<&bytes::Bytes>,
) {
    options.clear_auth();
    match (username, password) {
        (Some(username), Some(password)) => {
            options.set_credentials(username, password.clone());
        }
        (Some(username), None) => {
            options.set_username(username);
        }
        _ => {}
    }
}

fn set_v5_credentials(
    options: &mut rumqttc_v5::MqttOptions,
    username: Option<&str>,
    password: Option<&bytes::Bytes>,
) {
    options.clear_auth();
    match (username, password) {
        (Some(username), Some(password)) => {
            options.set_credentials(username, password.clone());
        }
        (Some(username), None) => {
            options.set_username(username);
        }
        (None, Some(password)) => {
            options.set_password(password.clone());
        }
        (None, None) => {}
    }
}

pub fn apply_v4(driver: &mut ConfigurationDriver, eventloop: &mut rumqttc_v4::EventLoop) {
    let tuning = driver.tuning();
    if driver.tuning_is_pending() {
        eventloop
            .mqtt_options
            .set_max_request_batch(tuning.max_request_batch);
        eventloop
            .mqtt_options
            .set_read_batch_size(tuning.read_batch_size);
        eventloop
            .mqtt_options
            .set_pending_throttle(tuning.pending_throttle);
        driver.tuning_applied(eventloop.diagnostics().config.effective_read_batch_size);
    }
    if driver.profile.is_some()
        && !eventloop.diagnostics().connected
        && let Some(profile) = driver.profile.take()
    {
        set_v4_credentials(
            &mut eventloop.mqtt_options,
            profile.username.as_deref(),
            profile.password.as_ref(),
        );
        if let PreparedTransport::V4(transport) = profile.transport {
            eventloop.mqtt_options.set_transport(transport);
        }
        eventloop.set_network_options(profile.network);
        driver.connection_applied();
    }
}

pub fn apply_v5(driver: &mut ConfigurationDriver, eventloop: &mut rumqttc_v5::EventLoop) {
    let tuning = driver.tuning();
    // Native redirect restoration may restore older tuning. Reassert only these audited fields.
    if driver.tuning_is_pending()
        || eventloop.options.max_request_batch() != tuning.max_request_batch
        || eventloop.options.read_batch_size() != tuning.read_batch_size
        || eventloop.options.pending_throttle() != tuning.pending_throttle
    {
        eventloop
            .options
            .set_max_request_batch(tuning.max_request_batch);
        eventloop
            .options
            .set_read_batch_size(tuning.read_batch_size);
        eventloop
            .options
            .set_pending_throttle(tuning.pending_throttle);
        driver.tuning_applied(eventloop.diagnostics().config.effective_read_batch_size);
    }
    if driver.profile.is_some()
        && !eventloop.diagnostics().connected
        && driver.control.observation.snapshot().route == crate::ConnectionRoute::Origin
        && let Some(profile) = driver.profile.take()
    {
        set_v5_credentials(
            &mut eventloop.options,
            profile.username.as_deref(),
            profile.password.as_ref(),
        );
        if let PreparedTransport::V5(transport) = profile.transport {
            eventloop.options.set_transport(transport);
        }
        eventloop.options.set_network_options(profile.network);
        eventloop.options.set_connect_timeout(profile.timeout);
        driver.connection_applied();
    }
}
