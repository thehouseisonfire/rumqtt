//! Optional, owned observations for managed connection drivers. No callbacks or wire changes.

use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum ConnectionRoute {
    #[default]
    Origin = 0,
    RedirectTransition = 1,
    TemporaryTarget = 2,
    PermanentTarget = 3,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum AttemptOutcome {
    #[default]
    None = 0,
    Pending = 1,
    Succeeded = 2,
    Failed = 3,
    Cancelled = 4,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionObservationSnapshot {
    pub route: ConnectionRoute,
    pub attempt: u64,
    pub attempt_route: ConnectionRoute,
    pub attempt_revision: Option<u64>,
    pub outcome: AttemptOutcome,
    pub successful_revision: Option<u64>,
    pub captured_at: Instant,
}

struct State {
    origin_revision: u64,
    phase_revision: Option<u64>,
    snapshot: ConnectionObservationSnapshot,
}

/// Read-only connection observations with a caller-assigned origin profile revision.
/// Revision assignment changes metadata only; it never changes MQTT options.
#[derive(Clone)]
pub struct ConnectionObservation(Arc<Mutex<State>>);

impl Default for ConnectionObservation {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            origin_revision: 0,
            phase_revision: None,
            snapshot: ConnectionObservationSnapshot {
                route: ConnectionRoute::Origin,
                attempt: 0,
                attempt_route: ConnectionRoute::Origin,
                attempt_revision: None,
                outcome: AttemptOutcome::None,
                successful_revision: None,
                captured_at: Instant::now(),
            },
        })))
    }
}

impl ConnectionObservation {
    #[must_use]
    pub fn snapshot(&self) -> ConnectionObservationSnapshot {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot
            .clone()
    }

    pub fn set_origin_revision(&self, revision: u64) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .origin_revision = revision;
    }

    /// Origin revision associated with the current or most recently completed connection phase.
    /// Redirect phases have no origin revision, even after origin options are restored.
    #[must_use]
    pub fn phase_revision(&self) -> Option<u64> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .phase_revision
    }

    #[doc(hidden)]
    pub fn set_route(&self, route: ConnectionRoute) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.snapshot.route = route;
        if route != ConnectionRoute::Origin {
            // Redirect lookup/decision work can fail before any target attempt begins.
            // Restoration must retain its attribution until a new origin attempt begins.
            state.phase_revision = None;
        }
        state.snapshot.captured_at = Instant::now();
    }

    #[doc(hidden)]
    #[must_use]
    pub fn begin_attempt(&self) -> AttemptObservation {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.snapshot.attempt = state.snapshot.attempt.saturating_add(1);
        state.snapshot.attempt_route = state.snapshot.route;
        state.snapshot.attempt_revision =
            (state.snapshot.route == ConnectionRoute::Origin).then_some(state.origin_revision);
        state.phase_revision = state.snapshot.attempt_revision;
        state.snapshot.outcome = AttemptOutcome::Pending;
        state.snapshot.captured_at = Instant::now();
        AttemptObservation {
            observer: self.clone(),
            attempt: state.snapshot.attempt,
            finished: false,
        }
    }
}

#[doc(hidden)]
pub struct AttemptObservation {
    observer: ConnectionObservation,
    attempt: u64,
    finished: bool,
}

impl AttemptObservation {
    pub fn finish(mut self, succeeded: bool) {
        let mut state = self
            .observer
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.finished = true;
        if state.snapshot.attempt != self.attempt {
            return;
        }
        state.snapshot.outcome = if succeeded {
            AttemptOutcome::Succeeded
        } else {
            AttemptOutcome::Failed
        };
        if succeeded {
            state.snapshot.successful_revision = state.snapshot.attempt_revision;
        }
        state.snapshot.captured_at = Instant::now();
    }
}

impl Drop for AttemptObservation {
    fn drop(&mut self) {
        if !self.finished {
            let mut state = self
                .observer
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.snapshot.attempt != self.attempt {
                return;
            }
            state.snapshot.outcome = AttemptOutcome::Cancelled;
            state.snapshot.captured_at = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_phase_attribution_survives_restoration_without_rewriting_attempt_history() {
        let observation = ConnectionObservation::default();
        observation.begin_attempt().finish(true);
        observation.set_origin_revision(3);
        let origin = observation.begin_attempt();
        observation.set_origin_revision(9);
        origin.finish(false);
        assert_eq!(observation.phase_revision(), Some(3));
        let previous_attempt = observation.snapshot();

        for route in [
            ConnectionRoute::RedirectTransition,
            ConnectionRoute::TemporaryTarget,
            ConnectionRoute::PermanentTarget,
        ] {
            observation.set_route(route);
            assert_eq!(observation.phase_revision(), None);
            observation.set_route(ConnectionRoute::Origin);
            assert_eq!(observation.phase_revision(), None);
            let restored = observation.snapshot();
            assert_eq!(restored.attempt, previous_attempt.attempt);
            assert_eq!(restored.attempt_revision, Some(3));
            assert_eq!(restored.successful_revision, Some(0));
        }

        observation.begin_attempt().finish(false);
        assert_eq!(observation.phase_revision(), Some(9));
        assert_eq!(observation.snapshot().attempt_revision, Some(9));
    }

    #[test]
    fn attempts_capture_revisions_and_old_guards_cannot_finish_new_generations() {
        let observation = ConnectionObservation::default();
        let first = observation.begin_attempt();
        observation.set_origin_revision(3);
        first.finish(true);
        assert_eq!(observation.snapshot().successful_revision, Some(0));
        let cancelled = observation.begin_attempt();
        assert_eq!(observation.snapshot().attempt_revision, Some(3));
        drop(cancelled);
        assert_eq!(observation.snapshot().outcome, AttemptOutcome::Cancelled);
        let obsolete = observation.begin_attempt();
        observation.set_route(ConnectionRoute::TemporaryTarget);
        let current = observation.begin_attempt();
        obsolete.finish(true);
        assert_eq!(observation.snapshot().outcome, AttemptOutcome::Pending);
        current.finish(true);
        assert_eq!(observation.snapshot().successful_revision, None);
        assert_eq!(
            observation.snapshot().attempt_route,
            ConnectionRoute::TemporaryTarget
        );
    }
}
