use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use bytes::Bytes;
use tokio::sync::Notify;

use crate::mqttbytes::QoS;
use crate::mqttbytes::v5::{ConnAck, Publish};
use crate::publish_budget::{
    PublishBudget, PublishBudgetError, PublishBudgetLimits, PublishReservation, publish_charge,
};

/// Selects where MQTT 5 publish requests are validated against negotiated broker capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PublishAdmissionPolicy {
    /// Preserve offline queueing and let the event loop validate capabilities when it processes
    /// each request.
    #[default]
    EventLoopValidated,
    /// Require producer-side validation against one coherent active-connection snapshot.
    ///
    /// Before CONNACK, only alias-free, non-retained `QoS` 0 publishes can be admitted. Other
    /// publishes return [`crate::ClientError::PublishAdmissionPending`].
    RequireNegotiatedCapabilities,
}

/// A negotiated-capability reason for rejecting a publish before request-channel admission.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublishAdmissionError {
    #[error("retained publishes are unavailable on the active connection")]
    RetainUnavailable,
    #[error("publish QoS {requested:?} exceeds the broker maximum {maximum:?}")]
    MaximumQos { requested: QoS, maximum: QoS },
    #[error("topic alias zero is invalid")]
    TopicAliasZero,
    #[error("topic alias {alias} exceeds the broker maximum {maximum}")]
    TopicAliasMaximum { alias: u16, maximum: u16 },
    #[error("topic alias {0} has no mapping on the active connection")]
    TopicAliasUnmapped(u16),
}

/// Change notification for capabilities, recovery, publish budget or request-channel capacity.
#[derive(Clone)]
pub struct PublishAdmissionWaiter {
    progress: Arc<PublishProgress>,
    revision: u64,
}

impl fmt::Debug for PublishAdmissionWaiter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublishAdmissionWaiter")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// Separate lock from capability state: terminal senders can release capacity during cleanup.
#[derive(Debug, Default)]
pub(crate) struct PublishProgress {
    revision: Mutex<u64>,
    changed: Notify,
    changed_blocking: Condvar,
}
impl PublishProgress {
    pub(crate) fn notify(&self) {
        let mut revision = self
            .revision
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *revision = revision.wrapping_add(1);
        drop(revision);
        self.changed.notify_waiters();
        self.changed_blocking.notify_all();
    }
}
impl PublishAdmissionWaiter {
    /// Wait for capability, recovery, retained-work capacity or channel progress.
    /// Cancelling this wait never admits a request.
    pub async fn wait_async(&self) {
        loop {
            let changed = self.progress.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if *self
                .progress
                .revision
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                != self.revision
            {
                return;
            }
            changed.await;
        }
    }
    pub(crate) fn wait_blocking(&self) {
        let revision = self
            .progress
            .revision
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(
            self.progress
                .changed_blocking
                .wait_while(revision, |revision| *revision == self.revision)
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}

#[derive(Clone, Copy, Debug)]
struct Capabilities {
    maximum_qos: QoS,
    retain_available: bool,
    topic_alias_maximum: u16,
}

#[derive(Debug)]
struct AdmissionState {
    closed: bool,
    generation: u64,
    capabilities: Option<Capabilities>,
    aliases: HashMap<u16, Bytes>,
}

#[derive(Debug)]
pub struct ManagedPublishAdmission {
    state: Mutex<AdmissionState>,
    policy: PublishAdmissionPolicy,
    pub(crate) budget: Option<Arc<PublishBudget>>,
    progress: Arc<PublishProgress>,
}

#[derive(Debug)]
pub enum AdmissionFailure {
    CapabilitiesUnavailable(PublishAdmissionWaiter),
    Rejected(PublishAdmissionError),
    Budget(PublishBudgetError, Option<PublishAdmissionWaiter>),
    Closed,
}

pub struct ConnectionCleanupGuard<'a> {
    admission: &'a ManagedPublishAdmission,
    state: Option<MutexGuard<'a, AdmissionState>>,
}

impl Drop for ConnectionCleanupGuard<'_> {
    fn drop(&mut self) {
        let mut state = self
            .state
            .take()
            .expect("cleanup guard owns admission state");
        state.capabilities = None;
        state.aliases.clear();
        drop(state);
        self.admission.notify_waiters();
    }
}

impl ManagedPublishAdmission {
    /// Close recovery admission after any producer transaction already in progress.
    /// The lock is released before invoking or awaiting the session store.
    pub(crate) fn begin_recovery(&self) {
        let Some(budget) = &self.budget else {
            return;
        };
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        budget.set_recovery_pending(true);
        drop(state);
        self.notify_waiters();
    }

    #[cfg(test)]
    pub(crate) fn new() -> Arc<Self> {
        Self::configured(
            PublishAdmissionPolicy::RequireNegotiatedCapabilities,
            None,
            false,
        )
    }

    pub(crate) fn configured(
        policy: PublishAdmissionPolicy,
        limits: Option<PublishBudgetLimits>,
        recovery: bool,
    ) -> Arc<Self> {
        let progress = Arc::new(PublishProgress::default());
        Arc::new(Self {
            state: Mutex::new(AdmissionState {
                closed: false,
                generation: 0,
                capabilities: None,
                aliases: HashMap::new(),
            }),
            policy,
            budget: limits
                .map(|limits| PublishBudget::new(limits, Arc::clone(&progress), recovery)),
            progress,
        })
    }

    #[cfg(test)]
    pub(crate) fn try_admit<T, E>(
        self: &Arc<Self>,
        publish: &Publish,
        send: impl FnOnce() -> Result<T, E>,
    ) -> Result<Result<T, E>, AdmissionFailure> {
        self.try_admit_reserved(publish, |_| send())
    }

    pub(crate) fn try_admit_reserved<T, E>(
        self: &Arc<Self>,
        publish: &Publish,
        send: impl FnOnce(Option<PublishReservation>) -> Result<T, E>,
    ) -> Result<Result<T, E>, AdmissionFailure> {
        let progress = self.waiter();
        let bytes = self
            .budget
            .as_ref()
            .map(|budget| budget.check_size(publish_charge(publish)))
            .transpose()
            .map_err(|reason| AdmissionFailure::Budget(reason, None))?;
        let reserve = || {
            self.budget
                .as_ref()
                .map(|budget| budget.reserve(bytes.expect("budget charge exists")))
                .transpose()
                .map_err(|reason| {
                    AdmissionFailure::Budget(reason, reason.can_wait().then(|| progress.clone()))
                })
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(AdmissionFailure::Closed);
        }
        if self
            .budget
            .as_ref()
            .is_some_and(|budget| budget.snapshot().recovery_pending)
        {
            return Err(AdmissionFailure::Budget(
                PublishBudgetError::RecoveryPending,
                Some(progress),
            ));
        }
        let alias = publish
            .properties
            .as_ref()
            .and_then(|properties| properties.topic_alias);
        let depends_on_capabilities =
            publish.qos != QoS::AtMostOnce || publish.retain || alias.is_some();
        if self.policy == PublishAdmissionPolicy::EventLoopValidated {
            return Ok(send(reserve()?));
        }
        let Some(capabilities) = state.capabilities else {
            if depends_on_capabilities {
                return Err(AdmissionFailure::CapabilitiesUnavailable(self.waiter()));
            }
            return Ok(send(reserve()?));
        };

        if publish.retain && !capabilities.retain_available {
            return Err(AdmissionFailure::Rejected(
                PublishAdmissionError::RetainUnavailable,
            ));
        }
        if publish.qos > capabilities.maximum_qos {
            return Err(AdmissionFailure::Rejected(
                PublishAdmissionError::MaximumQos {
                    requested: publish.qos,
                    maximum: capabilities.maximum_qos,
                },
            ));
        }
        if let Some(alias) = alias {
            if alias == 0 {
                return Err(AdmissionFailure::Rejected(
                    PublishAdmissionError::TopicAliasZero,
                ));
            }
            if alias > capabilities.topic_alias_maximum {
                return Err(AdmissionFailure::Rejected(
                    PublishAdmissionError::TopicAliasMaximum {
                        alias,
                        maximum: capabilities.topic_alias_maximum,
                    },
                ));
            }
            if publish.topic.is_empty() && !state.aliases.contains_key(&alias) {
                return Err(AdmissionFailure::Rejected(
                    PublishAdmissionError::TopicAliasUnmapped(alias),
                ));
            }
        }

        let result = send(reserve()?);
        if result.is_ok()
            && let Some(alias) = alias
            && !publish.topic.is_empty()
        {
            state.aliases.insert(alias, publish.topic.clone());
        }
        drop(state);
        Ok(result)
    }

    pub(crate) fn install_connack(&self, connack: &ConnAck) {
        let properties = connack.properties.as_ref();
        let maximum_qos = match properties.and_then(|properties| properties.max_qos) {
            Some(0) => QoS::AtMostOnce,
            Some(1) => QoS::AtLeastOnce,
            _ => QoS::ExactlyOnce,
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.generation = state.generation.wrapping_add(1);
        state.capabilities = Some(Capabilities {
            maximum_qos,
            retain_available: properties
                .and_then(|properties| properties.retain_available)
                .is_none_or(|available| available == 1),
            topic_alias_maximum: properties
                .and_then(|properties| properties.topic_alias_max)
                .unwrap_or(0),
        });
        state.aliases.clear();
        drop(state);
        self.notify_waiters();
    }

    /// Mirrors a Topic Alias mapping committed by the event-loop state machine.
    ///
    /// Automatic alias policies assign aliases after a request leaves the producer channel, so
    /// producer-side strict admission cannot learn those mappings from `try_admit`. Keeping the
    /// negotiated mapping here lets a later alias-only publish receive the same validation as it
    /// would in the event loop.
    pub(crate) fn record_outgoing_topic_alias(&self, publish: &Publish) {
        let Some(alias) = publish
            .properties
            .as_ref()
            .and_then(|properties| properties.topic_alias)
        else {
            return;
        };
        if publish.topic.is_empty() {
            return;
        }

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(capabilities) = state.capabilities else {
            return;
        };
        if alias == 0 || alias > capabilities.topic_alias_maximum {
            return;
        }
        state.aliases.insert(alias, publish.topic.clone());
    }

    pub(crate) fn begin_connection_cleanup(&self) -> ConnectionCleanupGuard<'_> {
        ConnectionCleanupGuard {
            admission: self,
            state: Some(
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            ),
        }
    }

    pub(crate) fn notify_progress(&self) {
        self.progress.notify();
    }

    pub(crate) fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return;
        }
        state.closed = true;
        drop(state);
        self.notify_waiters();
    }

    fn notify_waiters(&self) {
        self.progress.notify();
    }

    pub(crate) fn waiter(self: &Arc<Self>) -> PublishAdmissionWaiter {
        let revision = *self
            .progress
            .revision
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        PublishAdmissionWaiter {
            progress: Arc::clone(&self.progress),
            revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mqttbytes::v5::{ConnAckProperties, ConnectReturnCode, PublishProperties};

    fn connack(maximum_qos: u8, retain_available: u8, topic_alias_maximum: u16) -> ConnAck {
        ConnAck {
            session_present: false,
            code: ConnectReturnCode::Success,
            properties: Some(ConnAckProperties {
                session_expiry_interval: None,
                receive_max: None,
                max_qos: Some(maximum_qos),
                retain_available: Some(retain_available),
                max_packet_size: None,
                assigned_client_identifier: None,
                topic_alias_max: Some(topic_alias_maximum),
                reason_string: None,
                user_properties: Vec::new(),
                wildcard_subscription_available: None,
                subscription_identifiers_available: None,
                shared_subscription_available: None,
                server_keep_alive: None,
                response_information: None,
                server_reference: None,
                authentication_method: None,
                authentication_data: None,
            }),
        }
    }

    fn publish(topic: &str, qos: QoS, retain: bool, alias: Option<u16>) -> Publish {
        let properties = alias.map(|topic_alias| PublishProperties {
            topic_alias: Some(topic_alias),
            ..PublishProperties::default()
        });
        let mut publish = Publish::new(topic, qos, Bytes::new(), properties);
        publish.retain = retain;
        publish
    }

    #[test]
    fn unknown_capabilities_only_admit_universal_publish_form() {
        let admission = ManagedPublishAdmission::new();
        assert!(
            admission
                .try_admit(&publish("topic", QoS::AtMostOnce, false, None), || {
                    Ok::<_, ()>(())
                })
                .unwrap()
                .is_ok()
        );
        assert!(matches!(
            admission.try_admit(&publish("topic", QoS::AtLeastOnce, false, None), || {
                Ok::<_, ()>(())
            }),
            Err(AdmissionFailure::CapabilitiesUnavailable(_))
        ));
    }

    #[test]
    fn negotiated_capabilities_and_alias_mappings_are_transactional() {
        let admission = ManagedPublishAdmission::new();
        admission.install_connack(&connack(0, 0, 2));
        assert!(matches!(
            admission.try_admit(&publish("topic", QoS::AtLeastOnce, false, None), || {
                Ok::<_, ()>(())
            }),
            Err(AdmissionFailure::Rejected(
                PublishAdmissionError::MaximumQos { .. }
            ))
        ));
        assert!(matches!(
            admission.try_admit(&publish("topic", QoS::AtMostOnce, true, None), || {
                Ok::<_, ()>(())
            }),
            Err(AdmissionFailure::Rejected(
                PublishAdmissionError::RetainUnavailable
            ))
        ));

        let binding = publish("mapped/topic", QoS::AtMostOnce, false, Some(1));
        assert!(
            admission
                .try_admit(&binding, || Err::<(), _>(()))
                .unwrap()
                .is_err()
        );
        assert!(matches!(
            admission.try_admit(&publish("", QoS::AtMostOnce, false, Some(1)), || {
                Ok::<_, ()>(())
            }),
            Err(AdmissionFailure::Rejected(
                PublishAdmissionError::TopicAliasUnmapped(1)
            ))
        ));
        assert!(
            admission
                .try_admit(&binding, || Ok::<_, ()>(()))
                .unwrap()
                .is_ok()
        );
        assert!(
            admission
                .try_admit(&publish("", QoS::AtMostOnce, false, Some(1)), || {
                    Ok::<_, ()>(())
                })
                .unwrap()
                .is_ok()
        );
    }

    #[test]
    fn exhausted_budget_does_not_commit_alias_bindings() {
        let admission = ManagedPublishAdmission::configured(
            PublishAdmissionPolicy::RequireNegotiatedCapabilities,
            Some(PublishBudgetLimits {
                max_outstanding: 1,
                max_bytes: 65536,
            }),
            false,
        );
        admission.install_connack(&connack(2, 1, 1));
        let held = admission
            .try_admit_reserved(&publish("a", QoS::AtLeastOnce, false, None), Ok::<_, ()>)
            .unwrap()
            .unwrap();
        assert!(matches!(
            admission.try_admit_reserved(
                &publish("mapped", QoS::AtMostOnce, false, Some(1)),
                Ok::<_, ()>
            ),
            Err(AdmissionFailure::Budget(
                PublishBudgetError::CountExhausted,
                _
            ))
        ));
        drop(held);
        assert!(matches!(
            admission
                .try_admit_reserved(&publish("", QoS::AtMostOnce, false, Some(1)), Ok::<_, ()>),
            Err(AdmissionFailure::Rejected(
                PublishAdmissionError::TopicAliasUnmapped(1)
            ))
        ));
    }

    #[test]
    fn alias_bounds_rebinding_and_generation_reset_are_connection_scoped() {
        let admission = ManagedPublishAdmission::new();
        admission.install_connack(&connack(2, 1, 2));
        assert!(matches!(
            admission.try_admit(&publish("topic", QoS::AtMostOnce, false, Some(0)), || {
                Ok::<_, ()>(())
            }),
            Err(AdmissionFailure::Rejected(
                PublishAdmissionError::TopicAliasZero
            ))
        ));
        assert!(matches!(
            admission.try_admit(&publish("topic", QoS::AtMostOnce, false, Some(3)), || {
                Ok::<_, ()>(())
            }),
            Err(AdmissionFailure::Rejected(
                PublishAdmissionError::TopicAliasMaximum {
                    alias: 3,
                    maximum: 2
                }
            ))
        ));

        for topic in ["first/topic", "rebound/topic"] {
            assert!(
                admission
                    .try_admit(&publish(topic, QoS::AtMostOnce, false, Some(2)), || {
                        Ok::<_, ()>(())
                    })
                    .unwrap()
                    .is_ok()
            );
            assert_eq!(
                admission
                    .state
                    .lock()
                    .unwrap()
                    .aliases
                    .get(&2)
                    .map(Bytes::as_ref),
                Some(topic.as_bytes())
            );
        }

        admission.install_connack(&connack(2, 1, 2));
        assert!(matches!(
            admission.try_admit(&publish("", QoS::AtMostOnce, false, Some(2)), || {
                Ok::<_, ()>(())
            }),
            Err(AdmissionFailure::Rejected(
                PublishAdmissionError::TopicAliasUnmapped(2)
            ))
        ));
        assert_eq!(admission.state.lock().unwrap().generation, 2);
    }

    #[tokio::test]
    async fn waiter_wakes_on_connack_and_cleanup_invalidates_aliases() {
        let admission = ManagedPublishAdmission::new();
        let Err(AdmissionFailure::CapabilitiesUnavailable(waiter)) = admission
            .try_admit(&publish("topic", QoS::AtLeastOnce, false, None), || {
                Ok::<_, ()>(())
            })
        else {
            panic!("strict admission should wait before CONNACK");
        };
        admission.install_connack(&connack(1, 1, 1));
        waiter.wait_async().await;

        let binding = publish("mapped/topic", QoS::AtMostOnce, false, Some(1));
        assert!(
            admission
                .try_admit(&binding, || Ok::<_, ()>(()))
                .unwrap()
                .is_ok()
        );
        drop(admission.begin_connection_cleanup());
        assert!(matches!(
            admission.try_admit(&publish("", QoS::AtMostOnce, false, Some(1)), || {
                Ok::<_, ()>(())
            }),
            Err(AdmissionFailure::CapabilitiesUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn closing_admission_wakes_waiters_and_rejects_further_admission() {
        let admission = ManagedPublishAdmission::new();
        let Err(AdmissionFailure::CapabilitiesUnavailable(waiter)) = admission
            .try_admit(&publish("topic", QoS::AtLeastOnce, false, None), || {
                Ok::<_, ()>(())
            })
        else {
            panic!("strict admission should wait before CONNACK");
        };

        admission.close();
        tokio::time::timeout(std::time::Duration::from_millis(100), waiter.wait_async())
            .await
            .expect("closing admission should wake capability waiters");

        assert!(matches!(
            admission.try_admit(
                &publish("topic", QoS::AtMostOnce, false, None),
                || -> Result<(), ()> { Ok(()) }
            ),
            Err(AdmissionFailure::Closed)
        ));
    }

    #[tokio::test]
    async fn prearmed_waiter_observes_progress_that_precedes_await() {
        let admission = ManagedPublishAdmission::new();
        let waiter = admission.waiter();

        admission.notify_progress();

        tokio::time::timeout(std::time::Duration::from_millis(100), waiter.wait_async())
            .await
            .expect("a prearmed waiter must not miss intervening progress");
    }

    #[tokio::test]
    async fn cancelling_a_waiter_does_not_admit_or_mutate_a_publish() {
        let admission = ManagedPublishAdmission::new();
        let Err(AdmissionFailure::CapabilitiesUnavailable(waiter)) = admission.try_admit(
            &publish("mapped/topic", QoS::AtMostOnce, false, Some(1)),
            || Ok::<_, ()>(()),
        ) else {
            panic!("alias admission should wait before CONNACK");
        };
        let task = tokio::spawn(async move { waiter.wait_async().await });
        tokio::task::yield_now().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        let state = admission.state.lock().unwrap();
        assert!(state.capabilities.is_none());
        assert!(state.aliases.is_empty());
    }
}
