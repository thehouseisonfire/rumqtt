//! Bounded desired configuration, independent of the borrowed native poll.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flume::{Receiver, Sender};
use rumqttc_core::{ConnectionObservation, ConnectionRoute};

use crate::backend::configuration::PreparedProfile;
use crate::{
    Admission, ClientConfig, Completion, Error, ErrorCode, ErrorKind, NetworkConfig, Result,
    SecretBytes, TlsBackend, TlsConfig, TransportConfig,
};

pub const MAX_PENDING_CONFIGURATION_UPDATES: usize = 16;
pub const MAX_CONFIGURATION_UPDATE_BYTES: usize = 1024 * 1024;
pub const MAX_PENDING_CONFIGURATION_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FieldUpdate<T> {
    #[default]
    Unchanged,
    Replace(T),
    Clear,
}

impl<T> FieldUpdate<T> {
    const fn changed(&self) -> bool {
        !matches!(self, Self::Unchanged)
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct BrokerCredentials {
    pub username: Option<String>,
    pub password: Option<SecretBytes>,
}

impl std::fmt::Debug for BrokerCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerCredentials")
            .field("username_present", &self.username.is_some())
            .field("password_present", &self.password.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeConfigUpdate {
    pub max_request_batch: FieldUpdate<usize>,
    pub read_batch_size: FieldUpdate<usize>,
    pub pending_throttle: FieldUpdate<Duration>,
    pub credentials: FieldUpdate<BrokerCredentials>,
    pub broker_tls: FieldUpdate<TlsConfig>,
    pub network: FieldUpdate<NetworkConfig>,
    pub connection_timeout: FieldUpdate<Duration>,
}

impl RuntimeConfigUpdate {
    pub(crate) const fn groups(&self) -> (bool, bool) {
        (
            self.max_request_batch.changed()
                || self.read_batch_size.changed()
                || self.pending_throttle.changed(),
            self.credentials.changed()
                || self.broker_tls.changed()
                || self.network.changed()
                || self.connection_timeout.changed(),
        )
    }

    /// Validate the bounded declarative size without preparing native TLS state.
    /// # Errors
    /// Returns a typed configuration error when retained inputs exceed one update's limit.
    pub fn validate_retained_size(&self) -> Result<()> {
        self.retained_bytes().map(|_| ())
    }

    /// Retained declarative inputs, including TLS identity catalogs; native parsed allocations are separate.
    pub(crate) fn retained_bytes(&self) -> Result<usize> {
        let mut total = std::mem::size_of::<Self>();
        let mut add = |bytes: usize| -> Result<()> {
            total = total.checked_add(bytes).ok_or_else(too_large)?;
            if total > MAX_CONFIGURATION_UPDATE_BYTES {
                return Err(too_large());
            }
            Ok(())
        };
        if let FieldUpdate::Replace(credentials) = &self.credentials {
            add(credentials.username.as_ref().map_or(0, String::len))?;
            add(credentials
                .password
                .as_ref()
                .map_or(0, |p| p.expose().len()))?;
        }
        if let FieldUpdate::Replace(network) = &self.network {
            add(network.bind_device.as_ref().map_or(0, String::len))?;
        }
        if let FieldUpdate::Replace(tls) = &self.broker_tls {
            match &tls.roots {
                crate::TlsRootPolicy::Platform => {}
                crate::TlsRootPolicy::Pem(pem) | crate::TlsRootPolicy::PlatformAndPem(pem) => {
                    add(pem.len())?;
                }
            }
            for value in &tls.alpn_protocols {
                add(std::mem::size_of::<Vec<u8>>())?;
                add(value.len())?;
            }
            add(tls
                .pins
                .len()
                .checked_mul(std::mem::size_of::<crate::TlsPin>())
                .ok_or_else(too_large)?)?;
            add(tls
                .cipher_suites
                .len()
                .checked_mul(2)
                .ok_or_else(too_large)?)?;
            if let Some(identity) = &tls.identity {
                match identity {
                    crate::TlsClientIdentity::RustlsPem {
                        certificate,
                        private_key,
                    } => {
                        add(certificate.len())?;
                        add(private_key.expose().len())?;
                    }
                    crate::TlsClientIdentity::NativePkcs12 { identity, password } => {
                        add(identity.expose().len())?;
                        add(password.expose().len())?;
                    }
                    crate::TlsClientIdentity::External(config) => {
                        for identity in config.identities() {
                            add_identity(identity, &mut add)?;
                        }
                    }
                    crate::TlsClientIdentity::ExternalAsync(config) => {
                        for identity in config.identities() {
                            add_identity(identity, &mut add)?;
                        }
                    }
                }
            }
        }
        Ok(total)
    }

    fn apply(&self, candidate: &mut ClientConfig) -> Result<()> {
        let common = &mut candidate.common;
        let tuning = RuntimeTuning::from(&*common).updated(self)?;
        common.max_request_batch = tuning.max_request_batch;
        common.read_batch_size = tuning.read_batch_size;
        common.pending_throttle = tuning.pending_throttle;
        replace_or_default(&self.network, &mut common.network);
        match &self.connection_timeout {
            FieldUpdate::Unchanged => {}
            FieldUpdate::Replace(timeout) => common.connection_timeout = *timeout,
            FieldUpdate::Clear => common.connection_timeout = crate::config::DEFAULT_TIMEOUT,
        }
        match &self.credentials {
            FieldUpdate::Unchanged => {}
            FieldUpdate::Replace(credentials) => {
                common.username.clone_from(&credentials.username);
                common.password = credentials.password.clone().map(SecretBytes::into_bytes);
            }
            FieldUpdate::Clear => {
                common.username = None;
                common.password = None;
            }
        }
        match &self.broker_tls {
            FieldUpdate::Unchanged => {}
            FieldUpdate::Replace(tls) => match &mut common.transport {
                TransportConfig::Tls(current) | TransportConfig::Wss(current) => {
                    current.clone_from(tls);
                }
                _ => {
                    return Err(unsupported(
                        "TLS updates require an existing TLS or WSS transport",
                    ));
                }
            },
            FieldUpdate::Clear => return Err(unsupported("clearing broker TLS is unsupported")),
        }
        if candidate.protocol_version() == crate::ProtocolVersion::V4
            && candidate.common.username.is_none()
            && candidate.common.password.is_some()
        {
            return Err(unsupported("MQTT 3.1.1 passwords require a username"));
        }
        candidate.validate()
    }
}

fn add_identity(
    identity: &crate::TlsExternalIdentity,
    add: &mut impl FnMut(usize) -> Result<()>,
) -> Result<()> {
    add(std::mem::size_of::<crate::TlsExternalIdentity>())?;
    add(identity.key_id.len())?;
    add(identity.certificate_pem.len())?;
    add(identity
        .signature_schemes
        .len()
        .checked_mul(2)
        .ok_or_else(too_large)?)?;
    Ok(())
}

fn replace_or_default<T: Clone + Default>(update: &FieldUpdate<T>, value: &mut T) {
    match update {
        FieldUpdate::Unchanged => {}
        FieldUpdate::Replace(replacement) => value.clone_from(replacement),
        FieldUpdate::Clear => *value = T::default(),
    }
}

fn too_large() -> Error {
    Error::configuration("configuration update exceeds the retained input limit")
        .with_code(ErrorCode::ConfigurationUpdateTooLarge)
}

fn unsupported(message: &str) -> Error {
    Error::configuration(message).with_code(ErrorCode::ConfigurationUpdateUnsupported)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeTuning {
    pub max_request_batch: usize,
    pub read_batch_size: usize,
    pub pending_throttle: Duration,
}

impl RuntimeTuning {
    fn updated(mut self, update: &RuntimeConfigUpdate) -> Result<Self> {
        replace_or_default(&update.max_request_batch, &mut self.max_request_batch);
        replace_or_default(&update.read_batch_size, &mut self.read_batch_size);
        replace_or_default(&update.pending_throttle, &mut self.pending_throttle);
        if Instant::now().checked_add(self.pending_throttle).is_none() {
            return Err(Error::configuration(
                "duration exceeds the platform timer range",
            ));
        }
        Ok(self)
    }
}

impl From<&crate::CommonConfig> for RuntimeTuning {
    fn from(config: &crate::CommonConfig) -> Self {
        Self {
            max_request_batch: config.max_request_batch,
            read_batch_size: config.read_batch_size,
            pending_throttle: config.pending_throttle,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionProfileSummary {
    pub username_present: bool,
    pub password_present: bool,
    pub tls_backend: Option<TlsBackend>,
    pub tls_identity_present: bool,
    pub tls_pin_count: usize,
    pub network: NetworkConfig,
    pub connection_timeout: Duration,
}

impl From<&crate::CommonConfig> for ConnectionProfileSummary {
    fn from(config: &crate::CommonConfig) -> Self {
        let tls = match &config.transport {
            TransportConfig::Tls(tls) | TransportConfig::Wss(tls) => Some(tls),
            _ => None,
        };
        Self {
            username_present: config.username.is_some(),
            password_present: config.password.is_some(),
            tls_backend: tls.map(|tls| tls.backend),
            tls_identity_present: tls.is_some_and(|tls| tls.identity.is_some()),
            tls_pin_count: tls.map_or(0, |tls| tls.pins.len()),
            network: config.network.clone(),
            connection_timeout: config.connection_timeout,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ActivationState {
    Unchanged = 0,
    Staged = 1,
    Activated = 2,
    Superseded = 3,
    ClosedBeforeActivation = 4,
    UnavailableAfterRedirect = 5,
}

/// Retains only status, never configuration owners or a client reference.
#[derive(Clone, Debug)]
pub struct ConfigurationUpdateReceipt {
    pub revision: u64,
    states: Arc<Mutex<(ActivationState, ActivationState)>>,
}

impl PartialEq for ConfigurationUpdateReceipt {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision && Arc::ptr_eq(&self.states, &other.states)
    }
}
impl Eq for ConfigurationUpdateReceipt {}

impl ConfigurationUpdateReceipt {
    #[must_use]
    pub fn activation(&self) -> (ActivationState, ActivationState) {
        *self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn finish(&self, tuning: bool, state: ActivationState) {
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let value = if tuning { &mut states.0 } else { &mut states.1 };
        if *value == ActivationState::Staged {
            *value = state;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationSnapshot {
    pub revision: u64,
    pub desired_tuning_revision: u64,
    pub effective_tuning_revision: u64,
    pub desired_connection_revision: u64,
    pub effective_connection_revision: u64,
    pub desired_tuning: RuntimeTuning,
    pub effective_tuning: RuntimeTuning,
    pub effective_read_batch_size: usize,
    pub desired_connection: ConnectionProfileSummary,
    pub effective_connection: ConnectionProfileSummary,
    pub connection: crate::ConnectionObservationSnapshot,
    /// The time effective tuning/read-batch values were last sampled at a native poll boundary.
    pub effective_captured_at: Instant,
    pub captured_at: Instant,
    pub closed: bool,
}

#[derive(Default)]
struct Budget {
    count: usize,
    bytes: usize,
    closed: bool,
}
struct Reservation {
    budget: Arc<Mutex<Budget>>,
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut budget = self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        budget.count -= 1;
        budget.bytes -= self.bytes;
    }
}

pub struct UpdateRequest {
    update: RuntimeConfigUpdate,
    result: tokio::sync::oneshot::Sender<Result<Completion>>,
    _reservation: Reservation,
}

pub struct ConfigurationControl {
    sender: Sender<UpdateRequest>,
    budget: Arc<Mutex<Budget>>,
    snapshot: Mutex<ConfigurationSnapshot>,
    pub(crate) observation: ConnectionObservation,
}

impl ConfigurationControl {
    pub(crate) fn admit(
        &self,
        update: &RuntimeConfigUpdate,
        operations: &crate::operations::OperationRegistry,
    ) -> Result<Admission> {
        if update.groups() == (false, false) {
            return Err(Error::configuration("configuration update is empty"));
        }
        let bytes = update.retained_bytes()?;
        let reservation = {
            let mut budget = self
                .budget
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if budget.closed {
                return Err(Error::new(
                    ErrorKind::Shutdown,
                    "configuration admission is closed",
                ));
            }
            if budget.count >= MAX_PENDING_CONFIGURATION_UPDATES
                || bytes > MAX_PENDING_CONFIGURATION_BYTES - budget.bytes
            {
                return Err(Error::new(
                    ErrorKind::Backpressure,
                    "configuration update capacity is exhausted",
                ));
            }
            budget.count += 1;
            budget.bytes += bytes;
            drop(budget);
            Reservation {
                budget: self.budget.clone(),
                bytes,
            }
        };
        let (result, receive) = tokio::sync::oneshot::channel();
        let admission = operations.register(Box::pin(async move {
            receive
                .await
                .unwrap_or_else(|_| {
                    Err(Error::new(
                        ErrorKind::Shutdown,
                        "configuration update did not stage before driver termination",
                    ))
                })
                .into()
        }))?;
        if let Err(error) = self.sender.try_send(UpdateRequest {
            update: update.clone(),
            result,
            _reservation: reservation,
        }) {
            operations.cancel(admission.operation_id);
            return Err(Error::new(
                if matches!(error, flume::TrySendError::Full(_)) {
                    ErrorKind::Backpressure
                } else {
                    ErrorKind::Shutdown
                },
                "configuration update was not admitted",
            ));
        }
        Ok(admission)
    }

    pub(crate) fn snapshot(&self) -> ConfigurationSnapshot {
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        snapshot.connection = self.observation.snapshot();
        snapshot.captured_at = Instant::now();
        snapshot
    }
}

// Field order keeps captured and returned owners covered by the reservation during destruction.
struct ProfilePreparation {
    result: Option<Result<PreparedProfile>>,
    config: ClientConfig,
    tls_callbacks: Arc<crate::backend::TlsCallbackMonitor>,
    _blocking: Option<crate::execution::BlockingWork>,
}

impl ProfilePreparation {
    fn run(mut self) -> Self {
        self.result = Some(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::backend::configuration::prepare(&self.config, &self.tls_callbacks)
            }))
            .unwrap_or_else(|_| {
                Err(Error::configuration(
                    "configuration profile preparation panicked",
                ))
            }),
        );
        self
    }
}

struct Preparing {
    request: UpdateRequest,
    candidate: Option<ClientConfig>,
    tuning: RuntimeTuning,
    work: Option<tokio::task::JoinHandle<ProfilePreparation>>,
}

pub struct ConfigurationDriver {
    pub(crate) receiver: Receiver<UpdateRequest>,
    pub(crate) control: Arc<ConfigurationControl>,
    // Origin declarations become unavailable after a successful permanent move.
    desired: Option<ClientConfig>,
    desired_tuning: RuntimeTuning,
    preparing: Option<Preparing>,
    pub(crate) profile: Option<PreparedProfile>,
    tuning_receipt: Option<ConfigurationUpdateReceipt>,
    tuning_observed: bool,
    connection_receipt: Option<ConfigurationUpdateReceipt>,
    tls_callbacks: Arc<crate::backend::TlsCallbackMonitor>,
}

impl ConfigurationDriver {
    pub(crate) fn new(
        config: ClientConfig,
        observation: &ConnectionObservation,
        tls_callbacks: Arc<crate::backend::TlsCallbackMonitor>,
    ) -> Self {
        let (sender, receiver) = flume::bounded(MAX_PENDING_CONFIGURATION_UPDATES);
        let now = Instant::now();
        let control = Arc::new(ConfigurationControl {
            sender,
            budget: Arc::default(),
            observation: observation.clone(),
            snapshot: Mutex::new(ConfigurationSnapshot {
                revision: 0,
                desired_tuning_revision: 0,
                effective_tuning_revision: 0,
                desired_connection_revision: 0,
                effective_connection_revision: 0,
                desired_tuning: (&config.common).into(),
                effective_tuning: (&config.common).into(),
                effective_read_batch_size: 0,
                desired_connection: (&config.common).into(),
                effective_connection: (&config.common).into(),
                connection: observation.snapshot(),
                effective_captured_at: now,
                captured_at: now,
                closed: false,
            }),
        });
        Self {
            receiver,
            control,
            desired_tuning: (&config.common).into(),
            desired: Some(config),
            preparing: None,
            profile: None,
            tuning_receipt: None,
            tuning_observed: false,
            connection_receipt: None,
            tls_callbacks,
        }
    }

    pub(crate) const fn is_preparing(&self) -> bool {
        self.preparing.is_some()
    }

    pub(crate) fn connection_error(&self, error: Error) -> Error {
        error.with_configuration_revision(self.control.observation.phase_revision())
    }

    pub(crate) fn start(&mut self, request: UpdateRequest) {
        let mut candidate = self.desired.clone();
        let valid = self.validate_route(&request.update).and_then(|()| {
            if let Some(candidate) = &mut candidate {
                request.update.apply(candidate)?;
            }
            self.desired_tuning.updated(&request.update)
        });
        let tuning = match valid {
            Ok(tuning) => tuning,
            Err(error) => {
                let _ = request.result.send(Err(error));
                return;
            }
        };
        let work = request.update.groups().1.then(|| {
            let preparation = ProfilePreparation {
                result: None,
                config: candidate
                    .as_ref()
                    .expect("origin connection declarations remain available")
                    .clone(),
                tls_callbacks: self.tls_callbacks.clone(),
                _blocking: crate::execution::register_blocking_work(),
            };
            tokio::task::spawn_blocking(move || preparation.run())
        });
        self.preparing = Some(Preparing {
            request,
            candidate,
            tuning,
            work,
        });
    }

    fn validate_route(&self, update: &RuntimeConfigUpdate) -> Result<()> {
        if update.groups().1 && self.control.observation.snapshot().route != ConnectionRoute::Origin
        {
            return Err(unsupported(
                "connection-profile updates are unavailable during redirects or on redirected targets",
            ));
        }
        Ok(())
    }

    pub(crate) async fn complete_preparation(&mut self, shared: &crate::handle::Shared) {
        let work = self
            .preparing
            .as_mut()
            .expect("preparation branch is gated")
            .work
            .as_mut();
        match work {
            Some(work) => match work.await {
                Ok(mut preparation) => {
                    let result = preparation.result.take().expect("preparation completed");
                    self.finish_preparation(Ok(result.map(Some)), shared);
                    // The driver owns accepted profiles; cleanup of discarded results remains tracked.
                    drop(preparation);
                }
                Err(error) => self.finish_preparation(Err(error), shared),
            },
            None => self.finish_preparation(Ok(Ok(None)), shared),
        }
    }

    fn finish_preparation(
        &mut self,
        result: std::result::Result<Result<Option<PreparedProfile>>, tokio::task::JoinError>,
        shared: &crate::handle::Shared,
    ) {
        let Preparing {
            request,
            candidate,
            tuning: desired_tuning,
            ..
        } = self.preparing.take().expect("preparation remains owned");
        let result = result
            .unwrap_or_else(|_| {
                Err(Error::configuration(
                    "configuration profile preparation failed",
                ))
            })
            .and_then(|profile| self.validate_route(&request.update).map(|()| profile));
        match result {
            Err(error) => {
                let _ = request.result.send(Err(error));
            }
            Ok(profile) => {
                let guard = match shared.configuration_commit_guard() {
                    Ok(guard) => guard,
                    Err(error) => {
                        let _ = request.result.send(Err(error));
                        return;
                    }
                };
                let (tuning, connection) = request.update.groups();
                let revision = self.control.snapshot().revision.checked_add(1);
                let Some(revision) = revision else {
                    let _ = request.result.send(Err(Error::configuration(
                        "configuration revision space exhausted",
                    )));
                    return;
                };
                let receipt = ConfigurationUpdateReceipt {
                    revision,
                    states: Arc::new(Mutex::new((
                        if tuning {
                            ActivationState::Staged
                        } else {
                            ActivationState::Unchanged
                        },
                        if connection {
                            ActivationState::Staged
                        } else {
                            ActivationState::Unchanged
                        },
                    ))),
                };
                // A tuning preparation may have captured origin declarations before retirement.
                let old_config = if self.desired.is_some() {
                    std::mem::replace(&mut self.desired, candidate)
                } else {
                    candidate
                };
                self.desired_tuning = desired_tuning;
                let old_profile = if connection {
                    std::mem::replace(&mut self.profile, profile)
                } else {
                    None
                };
                if tuning && let Some(old) = self.tuning_receipt.replace(receipt.clone()) {
                    old.finish(true, ActivationState::Superseded);
                }
                if connection && let Some(old) = self.connection_receipt.replace(receipt.clone()) {
                    old.finish(false, ActivationState::Superseded);
                }
                {
                    let mut snapshot = self
                        .control
                        .snapshot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    snapshot.revision = revision;
                    if tuning {
                        snapshot.desired_tuning_revision = revision;
                        snapshot.desired_tuning = desired_tuning;
                    }
                    if connection {
                        snapshot.desired_connection_revision = revision;
                        snapshot.desired_connection = (&self
                            .desired
                            .as_ref()
                            .expect("connection declarations were committed")
                            .common)
                            .into();
                    }
                }
                drop(guard);
                drop(old_profile);
                drop(old_config);
                let _ = request
                    .result
                    .send(Ok(Completion::ConfigurationStaged(receipt)));
            }
        }
    }

    pub(crate) const fn tuning_is_pending(&self) -> bool {
        !self.tuning_observed || self.tuning_receipt.is_some()
    }

    pub(crate) const fn tuning(&self) -> RuntimeTuning {
        self.desired_tuning
    }

    pub(crate) fn tuning_applied(&mut self, effective_read_batch_size: usize) {
        self.tuning_observed = true;
        let mut snapshot = self
            .control
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.effective_tuning_revision = snapshot.desired_tuning_revision;
        snapshot.effective_tuning = snapshot.desired_tuning;
        snapshot.effective_read_batch_size = effective_read_batch_size;
        snapshot.effective_captured_at = Instant::now();
        drop(snapshot);
        if let Some(receipt) = self.tuning_receipt.take() {
            receipt.finish(true, ActivationState::Activated);
        }
    }

    pub(crate) fn connection_applied(&mut self) {
        let revision = {
            let mut snapshot = self
                .control
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            snapshot.effective_connection_revision = snapshot.desired_connection_revision;
            snapshot.effective_connection = snapshot.desired_connection.clone();
            snapshot.effective_connection_revision
        };
        self.control.observation.set_origin_revision(revision);
        if let Some(receipt) = self.connection_receipt.take() {
            receipt.finish(false, ActivationState::Activated);
        }
    }

    pub(crate) fn permanent_redirect(&mut self) {
        if self.desired.is_some() {
            let observation = self.control.observation.snapshot();
            // Routing changes before a target attempt begins; success may still belong to the origin.
            if observation.route != ConnectionRoute::PermanentTarget
                || observation.attempt_route != ConnectionRoute::PermanentTarget
                || observation.outcome != crate::AttemptOutcome::Succeeded
            {
                return;
            }
            self.profile = None;
            self.desired = None;
            if let Some(receipt) = self.connection_receipt.take() {
                receipt.finish(false, ActivationState::UnavailableAfterRedirect);
            }
        }
    }

    /// Resolve configuration work before shutdown drains its registered completions.
    pub(crate) fn close(&mut self) {
        self.control
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed = true;
        self.control
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed = true;
        for (tuning, receipt) in [
            (true, self.tuning_receipt.take()),
            (false, self.connection_receipt.take()),
        ] {
            if let Some(receipt) = receipt {
                receipt.finish(tuning, ActivationState::ClosedBeforeActivation);
            }
        }
        // Dropping the join handle detaches preparation; its owned work reservation still covers cleanup.
        self.preparing = None;
        while self.receiver.try_recv().is_ok() {}
        self.profile = None;
        self.desired = None;
    }
}

impl Drop for ConfigurationDriver {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked_password() -> (bytes::Bytes, std::sync::Weak<()>) {
        struct Owner {
            _lifetime: Arc<()>,
        }
        impl AsRef<[u8]> for Owner {
            fn as_ref(&self) -> &[u8] {
                b"origin-password"
            }
        }
        let lifetime = Arc::new(());
        let retired = Arc::downgrade(&lifetime);
        (
            bytes::Bytes::from_owner(Owner {
                _lifetime: lifetime,
            }),
            retired,
        )
    }

    fn make_driver() -> ConfigurationDriver {
        ConfigurationDriver::new(
            ClientConfig::v4("test", "localhost", 1883),
            &ConnectionObservation::default(),
            Arc::default(),
        )
    }

    #[cfg(feature = "use-rustls-no-provider")]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "Keep preparation and destructor barriers together for both protocols and result paths"
    )]
    fn abandoned_preparation_keeps_work_reserved_through_owner_destruction() {
        struct Owner {
            entered: Sender<()>,
            release: Receiver<()>,
        }
        impl crate::TlsVerifier for Owner {
            fn verify(
                &self,
                _: &crate::TlsVerificationRequest<'_>,
            ) -> std::result::Result<(), crate::TlsCallbackReason> {
                unreachable!("preparation must not invoke verification")
            }
        }
        impl Drop for Owner {
            fn drop(&mut self) {
                self.entered.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        }

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()])
            .unwrap()
            .cert
            .pem();
        for mqtt5 in [false, true] {
            for valid in [false, true] {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .max_blocking_threads(1)
                    .build()
                    .unwrap();
                let (busy, busy_rx) = flume::bounded(1);
                let (resume, resume_rx) = flume::bounded(1);
                let blocker = runtime.spawn_blocking(move || {
                    busy.send(()).unwrap();
                    resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                });
                busy_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                let (entered, entered_rx) = flume::bounded(1);
                let (release, release_rx) = flume::bounded(1);
                let mut config = if mqtt5 {
                    ClientConfig::v5("preparation", "localhost", 1883)
                } else {
                    ClientConfig::v4("preparation", "localhost", 1883)
                };
                config.common.transport = TransportConfig::Tls(TlsConfig::default());
                let mut driver = ConfigurationDriver::new(
                    config,
                    &ConnectionObservation::default(),
                    Arc::default(),
                );
                let (operations, _receivers) = crate::operations::OperationRegistry::new(8);
                let _admission = driver
                    .control
                    .admit(
                        &RuntimeConfigUpdate {
                            broker_tls: FieldUpdate::Replace(TlsConfig {
                                roots: crate::TlsRootPolicy::Pem(
                                    if valid {
                                        certificate.as_bytes()
                                    } else {
                                        b"invalid PEM"
                                    }
                                    .to_vec()
                                    .into(),
                                ),
                                verifier: Some(crate::TlsVerifierConfig(Arc::new(Owner {
                                    entered,
                                    release: release_rx,
                                }))),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        &operations,
                    )
                    .unwrap();
                let work = Arc::new(crate::execution::DriverWork::default());
                runtime.block_on(crate::execution::DRIVER_WORK.scope(work.clone(), async {
                    driver.start(driver.receiver.try_recv().unwrap());
                }));
                assert!(driver.preparing.as_ref().unwrap().work.is_some());
                drop(driver);
                resume.send(()).unwrap();
                entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                let waiting = runtime.block_on(async {
                    tokio::time::timeout(Duration::from_millis(25), work.wait())
                        .await
                        .is_err()
                });
                // Release the destructor before asserting so a regression cannot strand the runtime.
                release.send(()).unwrap();
                runtime.block_on(async {
                    blocker.await.unwrap();
                    tokio::time::timeout(Duration::from_secs(5), work.wait())
                        .await
                        .unwrap();
                });
                assert!(
                    waiting,
                    "teardown passed an active owner destructor (v5={mqtt5}, valid={valid})"
                );
            }
        }
    }

    #[test]
    fn permanent_redirect_retains_the_prepared_profile_until_a_target_attempt_succeeds() {
        for outcome in [
            crate::AttemptOutcome::Succeeded,
            crate::AttemptOutcome::Failed,
            crate::AttemptOutcome::Cancelled,
        ] {
            let observation = ConnectionObservation::default();
            observation.begin_attempt().finish(true);
            let mut config = ClientConfig::v5("origin", "localhost", 1883);
            let (password, retired) = tracked_password();
            config.common.password = Some(password);
            let mut driver = ConfigurationDriver::new(config, &observation, Arc::default());
            driver.profile = Some(
                crate::backend::configuration::prepare(
                    driver.desired.as_ref().unwrap(),
                    &driver.tls_callbacks,
                )
                .unwrap(),
            );
            let receipt = ConfigurationUpdateReceipt {
                revision: 1,
                states: Arc::new(Mutex::new((
                    ActivationState::Unchanged,
                    ActivationState::Staged,
                ))),
            };
            driver.connection_receipt = Some(receipt.clone());

            observation.set_route(ConnectionRoute::PermanentTarget);
            driver.permanent_redirect();
            assert!(driver.profile.is_some());
            assert!(retired.upgrade().is_some());
            assert_eq!(receipt.activation().1, ActivationState::Staged);

            let target = observation.begin_attempt();
            driver.permanent_redirect();
            assert!(driver.profile.is_some());
            assert_eq!(receipt.activation().1, ActivationState::Staged);
            match outcome {
                crate::AttemptOutcome::Succeeded => target.finish(true),
                crate::AttemptOutcome::Failed => target.finish(false),
                crate::AttemptOutcome::Cancelled => drop(target),
                _ => unreachable!(),
            }
            driver.permanent_redirect();
            let succeeded = outcome == crate::AttemptOutcome::Succeeded;
            assert_eq!(driver.profile.is_none(), succeeded);
            assert_eq!(driver.desired.is_none(), succeeded);
            assert_eq!(retired.upgrade().is_none(), succeeded);
            assert_eq!(
                receipt.activation().1,
                if succeeded {
                    ActivationState::UnavailableAfterRedirect
                } else {
                    ActivationState::Staged
                }
            );
            drop(driver);
            if !succeeded {
                assert_eq!(
                    receipt.activation().1,
                    ActivationState::ClosedBeforeActivation
                );
            }
        }
    }

    #[tokio::test]
    async fn tuning_prepared_before_a_permanent_move_cannot_restore_origin_owners() {
        let observation = ConnectionObservation::default();
        let mut config = ClientConfig::v5("origin", "localhost", 1883);
        let (password, retired) = tracked_password();
        config.common.password = Some(password);
        let mut driver = ConfigurationDriver::new(config, &observation, Arc::default());
        let original = driver.control.snapshot().desired_connection;
        let (operations, _receivers) = crate::operations::OperationRegistry::new(8);
        let (backend_tx, _backend_rx) = flume::unbounded();
        let (shutdown_tx, _shutdown_rx) = flume::unbounded();
        let (panic_tx, _panic_rx) = flume::unbounded();
        let shared = crate::handle::Shared::new(
            (
                crate::backend::BackendClient::V5(rumqttc_v5::AsyncClient::from_senders(
                    backend_tx,
                )),
                rumqttc_core::session_recovery::SessionRecoveryGate::new(
                    "origin".into(),
                    "origin".into(),
                ),
            ),
            crate::acknowledgement::AcknowledgementCoordinator::new(1, operations.clone()),
            crate::connection::ConnectionHandle::new(),
            operations.clone(),
            crate::shutdown::ShutdownCoordinator::new(operations.clone(), shutdown_tx),
            panic_tx,
            crate::reconnect::Controller::new(crate::ReconnectPolicy::Legacy),
        );
        let _admission = driver
            .control
            .admit(
                &RuntimeConfigUpdate {
                    read_batch_size: FieldUpdate::Replace(17),
                    ..Default::default()
                },
                &operations,
            )
            .unwrap();
        driver.start(driver.receiver.try_recv().unwrap());
        observation.set_route(ConnectionRoute::PermanentTarget);
        observation.begin_attempt().finish(true);
        driver.permanent_redirect();
        assert!(driver.desired.is_none());
        // The unfinished tuning candidate is now the last owner of the origin password.
        assert!(retired.upgrade().is_some());
        driver.complete_preparation(&shared).await;
        assert!(driver.desired.is_none());
        assert!(retired.upgrade().is_none());
        let receipt = driver.tuning_receipt.as_ref().unwrap().clone();
        assert_eq!(driver.control.snapshot().revision, 1);
        assert_eq!(driver.tuning().read_batch_size, 17);
        assert_eq!(driver.control.snapshot().desired_connection, original);
        driver.tuning_applied(17);
        assert_eq!(receipt.activation().0, ActivationState::Activated);
    }

    #[test]
    fn retained_size_includes_tls_catalog_and_alpn_entry_storage() {
        let mut total = 0;
        add_identity(
            &crate::TlsExternalIdentity {
                key_id: bytes::Bytes::new(),
                certificate_pem: bytes::Bytes::new(),
                signature_schemes: Vec::new(),
            },
            &mut |bytes| {
                total += bytes;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(total, std::mem::size_of::<crate::TlsExternalIdentity>());

        let driver = make_driver();
        let (operations, _receivers) = crate::operations::OperationRegistry::new(1);
        let update = RuntimeConfigUpdate {
            broker_tls: FieldUpdate::Replace(TlsConfig {
                alpn_protocols: vec![
                    Vec::new();
                    MAX_CONFIGURATION_UPDATE_BYTES / std::mem::size_of::<Vec<u8>>()
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            driver
                .control
                .admit(&update, &operations)
                .unwrap_err()
                .code(),
            ErrorCode::ConfigurationUpdateTooLarge
        );
        assert_eq!(driver.control.budget.lock().unwrap().count, 0);
    }

    #[test]
    fn admission_reservations_bound_count_bytes_and_release_without_a_driver() {
        let driver = make_driver();
        let control = driver.control.clone();
        let (operations, _receivers) = crate::operations::OperationRegistry::new(1);
        for _ in 0..MAX_PENDING_CONFIGURATION_UPDATES {
            let admission = control
                .admit(
                    &RuntimeConfigUpdate {
                        read_batch_size: FieldUpdate::Replace(1),
                        ..Default::default()
                    },
                    &operations,
                )
                .unwrap();
            assert_eq!(
                admission
                    .completion
                    .wait_timeout(Duration::ZERO)
                    .unwrap_err()
                    .kind(),
                ErrorKind::Timeout
            );
            drop(admission.completion);
        }
        assert_eq!(
            control
                .admit(
                    &RuntimeConfigUpdate {
                        read_batch_size: FieldUpdate::Clear,
                        ..Default::default()
                    },
                    &operations
                )
                .unwrap_err()
                .kind(),
            ErrorKind::Backpressure
        );
        assert_eq!(
            control.budget.lock().unwrap().count,
            MAX_PENDING_CONFIGURATION_UPDATES
        );
        drop(driver);
        assert_eq!(control.budget.lock().unwrap().count, 0);
        assert!(control.snapshot().closed);
        assert_eq!(
            control
                .admit(
                    &RuntimeConfigUpdate {
                        read_batch_size: FieldUpdate::Clear,
                        ..Default::default()
                    },
                    &operations
                )
                .unwrap_err()
                .kind(),
            ErrorKind::Shutdown
        );
        let driver = make_driver();
        let control = driver.control.clone();
        // Inputs need not be semantically valid to reserve bounded admission storage.
        let update = RuntimeConfigUpdate {
            credentials: FieldUpdate::Replace(BrokerCredentials {
                username: Some("x".repeat(900_000)),
                password: None,
            }),
            ..Default::default()
        };
        for _ in 0..4 {
            control.admit(&update, &operations).unwrap();
        }
        assert_eq!(
            control.admit(&update, &operations).unwrap_err().kind(),
            ErrorKind::Backpressure
        );
        let oversized = RuntimeConfigUpdate {
            credentials: FieldUpdate::Replace(BrokerCredentials {
                username: Some("x".repeat(MAX_CONFIGURATION_UPDATE_BYTES)),
                password: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            control.admit(&oversized, &operations).unwrap_err().code(),
            ErrorCode::ConfigurationUpdateTooLarge
        );
        drop(driver);
        assert_eq!(control.budget.lock().unwrap().bytes, 0);
    }

    #[test]
    fn partial_updates_clear_defaults_and_keep_password_only_protocol_rules() {
        let mut config = ClientConfig::v5("test", "localhost", 1883);
        config.common.network.tcp_nodelay = true;
        RuntimeConfigUpdate {
            credentials: FieldUpdate::Replace(BrokerCredentials {
                username: None,
                password: Some(SecretBytes::new(vec![0, 255])),
            }),
            network: FieldUpdate::Clear,
            pending_throttle: FieldUpdate::Clear,
            connection_timeout: FieldUpdate::Clear,
            ..Default::default()
        }
        .apply(&mut config)
        .unwrap();
        assert_eq!(config.common.password.as_deref(), Some([0, 255].as_slice()));
        assert!(!config.common.network.tcp_nodelay);
        RuntimeConfigUpdate {
            credentials: FieldUpdate::Clear,
            ..Default::default()
        }
        .apply(&mut config)
        .unwrap();
        assert!(config.common.password.is_none());
        assert!(
            RuntimeConfigUpdate {
                credentials: FieldUpdate::Replace(BrokerCredentials {
                    username: None,
                    password: Some(SecretBytes::new(vec![0]))
                }),
                ..Default::default()
            }
            .apply(&mut ClientConfig::v4("test", "localhost", 1883))
            .is_err()
        );
    }

    #[test]
    fn receipts_are_independent_and_terminal_activation_does_not_regress() {
        let receipt = ConfigurationUpdateReceipt {
            revision: 9,
            states: Arc::new(Mutex::new((
                ActivationState::Staged,
                ActivationState::Staged,
            ))),
        };
        let retained = receipt.clone();
        receipt.finish(true, ActivationState::Activated);
        receipt.finish(true, ActivationState::Superseded);
        receipt.finish(false, ActivationState::ClosedBeforeActivation);
        drop(receipt);
        assert_eq!(
            retained.activation(),
            (
                ActivationState::Activated,
                ActivationState::ClosedBeforeActivation
            )
        );
    }
}
