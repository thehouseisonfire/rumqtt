//! Process-local accounting independent of request-channel and protocol queue placement.
use std::sync::{Arc, Mutex};

use crate::publish_admission::PublishProgress;
use crate::{PersistedRequest, Publish};

/// Finite limits on outstanding publish operations and their retained data.
///
/// Bytes charge payload, recoverable topic, property strings/binaries and variable collection
/// storage. Alias-only publishes reserve the maximum MQTT topic length for replay expansion.
/// Fixed per-operation tracking is bounded by `max_outstanding`; this is not an RSS limit.
/// Buffers used for network encoding, checkpoints and caller-owned data are separate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishBudgetLimits {
    pub max_outstanding: usize,
    pub max_bytes: usize,
}

/// Coherent accounting snapshot; queue transfers never change these counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishBudgetSnapshot {
    pub outstanding: usize,
    pub retained_bytes: usize,
    pub limits: PublishBudgetLimits,
    pub recovery_pending: bool,
}

/// Reason why a publish cannot acquire retained-work capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublishBudgetError {
    #[error("session recovery must finish before publish admission")]
    RecoveryPending,
    #[error("outstanding publish limit reached")]
    CountExhausted,
    #[error("retained publish byte limit reached")]
    BytesExhausted,
    #[error("publish data exceeds the per-client byte limit")]
    TooLarge,
}

impl PublishBudgetError {
    pub(crate) const fn can_wait(self) -> bool {
        !matches!(self, Self::TooLarge)
    }
}

#[derive(Debug)]
pub(crate) struct PublishBudget {
    state: Mutex<PublishBudgetSnapshot>,
    progress: Arc<PublishProgress>,
}

/// Owned only by the native terminal sender, never by a completion observer.
#[derive(Debug)]
pub(crate) struct PublishReservation {
    budget: Arc<PublishBudget>,
    bytes: usize,
    notify_on_drop: bool,
}

impl PublishReservation {
    /// Failed channel sends roll back while holding the admission mutex. No other producer
    /// can observe that temporary reservation, so waking it would only create spurious retries.
    pub(crate) fn rollback(mut self) {
        self.notify_on_drop = false;
    }
}

impl Drop for PublishReservation {
    fn drop(&mut self) {
        let mut state = self
            .budget
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.outstanding -= 1;
        state.retained_bytes -= self.bytes;
        drop(state);
        if self.notify_on_drop {
            self.budget.progress.notify();
        }
    }
}

impl PublishBudget {
    pub(crate) fn new(
        limits: PublishBudgetLimits,
        progress: Arc<PublishProgress>,
        recovery_pending: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PublishBudgetSnapshot {
                outstanding: 0,
                retained_bytes: 0,
                limits,
                recovery_pending,
            }),
            progress,
        })
    }

    pub(crate) fn snapshot(&self) -> PublishBudgetSnapshot {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn recovery_pending(&self, pending: bool) {
        self.set_recovery_pending(pending);
        self.progress.notify();
    }

    /// Update the gate inside an admission transaction; its owner notifies after unlocking.
    pub(crate) fn set_recovery_pending(&self, pending: bool) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recovery_pending = pending;
    }

    pub(crate) fn check_size(&self, bytes: Option<usize>) -> Result<usize, PublishBudgetError> {
        let bytes = bytes.ok_or(PublishBudgetError::TooLarge)?;
        if bytes > self.snapshot().limits.max_bytes {
            return Err(PublishBudgetError::TooLarge);
        }
        Ok(bytes)
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        bytes: usize,
    ) -> Result<PublishReservation, PublishBudgetError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.recovery_pending {
            return Err(PublishBudgetError::RecoveryPending);
        }
        if state.outstanding >= state.limits.max_outstanding {
            return Err(PublishBudgetError::CountExhausted);
        }
        if bytes > state.limits.max_bytes - state.retained_bytes {
            return Err(PublishBudgetError::BytesExhausted);
        }
        state.outstanding += 1;
        state.retained_bytes += bytes;
        Ok(PublishReservation {
            budget: Arc::clone(self),
            bytes,
            notify_on_drop: true,
        })
    }

    pub(crate) fn reserve_replay(
        self: &Arc<Self>,
        costs: &[usize],
    ) -> Result<Vec<PublishReservation>, crate::SessionRestoreError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = costs
            .iter()
            .try_fold(0usize, |sum, bytes| sum.checked_add(*bytes));
        if costs.len() > state.limits.max_outstanding - state.outstanding
            || bytes.is_none_or(|bytes| bytes > state.limits.max_bytes - state.retained_bytes)
        {
            return Err(crate::SessionRestoreError::PublishBudgetExceeded);
        }
        state.outstanding += costs.len();
        state.retained_bytes += bytes.expect("checked replay byte sum");
        Ok(costs
            .iter()
            .map(|bytes| PublishReservation {
                budget: Arc::clone(self),
                bytes: *bytes,
                notify_on_drop: true,
            })
            .collect())
    }
}

fn data_charge(
    topic: usize,
    payload: usize,
    response_topic: Option<&str>,
    correlation_data: Option<usize>,
    content_type: Option<&str>,
    user_properties: &[(String, String)],
    subscription_identifiers: usize,
) -> Option<usize> {
    let mut bytes = topic.checked_add(payload)?;
    for len in [
        response_topic.map_or(0, str::len),
        correlation_data.unwrap_or(0),
        content_type.map_or(0, str::len),
        user_properties
            .len()
            .checked_mul(std::mem::size_of::<(String, String)>())?,
        subscription_identifiers.checked_mul(std::mem::size_of::<usize>())?,
    ] {
        bytes = bytes.checked_add(len)?;
    }
    for (key, value) in user_properties {
        bytes = bytes.checked_add(key.len())?.checked_add(value.len())?;
    }
    Some(bytes)
}

pub(crate) fn publish_charge(publish: &Publish) -> Option<usize> {
    let topic = if publish.topic.is_empty() {
        usize::from(u16::MAX)
    } else {
        publish.topic.len()
    };
    let Some(p) = &publish.properties else {
        return topic.checked_add(publish.payload.len());
    };
    data_charge(
        topic,
        publish.payload.len(),
        p.response_topic.as_deref(),
        p.correlation_data.as_ref().map(bytes::Bytes::len),
        p.content_type.as_deref(),
        &p.user_properties,
        p.subscription_identifiers.len(),
    )
}

pub(crate) fn replay_charge(request: &PersistedRequest) -> Option<Option<usize>> {
    match request {
        PersistedRequest::Publish(publish) => {
            let bytes = if let Some(p) = &publish.properties {
                data_charge(
                    publish.topic.len(),
                    publish.payload.len(),
                    p.response_topic.as_deref(),
                    p.correlation_data.as_ref().map(Vec::len),
                    p.content_type.as_deref(),
                    &p.user_properties,
                    p.subscription_identifiers.len(),
                )?
            } else {
                publish.topic.len().checked_add(publish.payload.len())?
            };
            Some(Some(bytes))
        }
        // PUBREL is one outstanding publish; its payload has already been discarded.
        PersistedRequest::PubRel(_) => Some(Some(0)),
        _ => Some(None),
    }
}
