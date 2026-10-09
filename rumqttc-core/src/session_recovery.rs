//! Connection-owner recovery coordination. Producers must share the owner's admission barrier.
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum RecoveryPhase {
    Quiescing = 1,
    Abandoning = 2,
    ClearingCheckpoint = 3,
    EstablishingFresh = 4,
    CleanDisconnect = 5,
    EstablishingPersistent = 6,
    Completed = 7,
    Failed = 8,
    Interrupted = 9,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoverySnapshot {
    pub phase: RecoveryPhase,
    pub failure_phase: Option<RecoveryPhase>,
    pub abandonment_committed: bool,
    pub checkpoint_cleared: bool,
    pub fresh_established: bool,
    pub raw_session_present: Option<bool>,
}

#[derive(Debug)]
pub struct RecoveryObservation(Mutex<RecoverySnapshot>);
impl RecoveryObservation {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(RecoverySnapshot {
            phase: RecoveryPhase::Quiescing,
            failure_phase: None,
            abandonment_committed: false,
            checkpoint_cleared: false,
            fresh_established: false,
            raw_session_present: None,
        })))
    }
    pub fn snapshot(&self) -> RecoverySnapshot {
        self.0.lock().unwrap().clone()
    }
    pub fn phase(&self, phase: RecoveryPhase) {
        let mut state = self.0.lock().unwrap();
        if !matches!(
            state.phase,
            RecoveryPhase::Completed | RecoveryPhase::Failed | RecoveryPhase::Interrupted
        ) {
            state.phase = phase;
        }
    }
    pub fn abandoned(&self) {
        self.0.lock().unwrap().abandonment_committed = true;
    }
    pub fn cleared(&self) {
        self.0.lock().unwrap().checkpoint_cleared = true;
    }
    pub fn established(&self, raw_session_present: bool) {
        let mut state = self.0.lock().unwrap();
        state.fresh_established = true;
        state.raw_session_present = Some(raw_session_present);
    }
    pub fn fail(&self, interrupted: bool) {
        let mut state = self.0.lock().unwrap();
        if matches!(
            state.phase,
            RecoveryPhase::Completed | RecoveryPhase::Failed | RecoveryPhase::Interrupted
        ) {
            return;
        }
        state.failure_phase = Some(state.phase);
        state.phase = if interrupted {
            RecoveryPhase::Interrupted
        } else {
            RecoveryPhase::Failed
        };
    }
}

#[derive(Debug)]
struct State {
    connected: bool,
    identity: (String, String),
    recovery: Option<Arc<RecoveryObservation>>,
}
#[derive(Debug)]
pub struct SessionRecoveryGate {
    state: Mutex<State>,
    changed: Notify,
}
impl SessionRecoveryGate {
    pub fn new(client_id: String, scope: String) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                connected: false,
                identity: (client_id, scope),
                recovery: None,
            }),
            changed: Notify::new(),
        })
    }
    pub fn can_request(&self) -> bool {
        let state = self.state.lock().unwrap();
        !state.connected && state.recovery.is_none()
    }
    /// Call under the connection owner's producer/shutdown admission barrier.
    pub fn request(&self) -> Option<Arc<RecoveryObservation>> {
        let mut state = self.state.lock().unwrap();
        if state.connected || state.recovery.is_some() {
            return None;
        }
        let observation = RecoveryObservation::new();
        state.recovery = Some(observation.clone());
        drop(state);
        self.changed.notify_waiters();
        Some(observation)
    }
    pub fn observation(&self) -> Option<Arc<RecoveryObservation>> {
        self.state.lock().unwrap().recovery.clone()
    }
    pub fn requested(&self) -> bool {
        self.state.lock().unwrap().recovery.is_some()
    }
    pub fn identity_matches(&self, client_id: &str, scope: &str) -> bool {
        let state = self.state.lock().unwrap();
        state.identity.0 == client_id && state.identity.1 == scope
    }
    pub fn establish(&self, client_id: String, scope: String) {
        let mut state = self.state.lock().unwrap();
        if state.recovery.is_none() {
            state.identity = (client_id, scope);
            state.connected = true;
        }
    }
    pub fn disconnected(&self, client_id: String, scope: String) {
        let mut state = self.state.lock().unwrap();
        state.connected = false;
        if state.recovery.is_none() {
            state.identity = (client_id, scope);
        }
    }
    pub fn complete(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(observation) = state.recovery.take() {
            observation.phase(RecoveryPhase::Completed);
        }
        state.connected = true;
    }
    pub async fn changed(&self) {
        self.changed.notified().await;
    }
}
