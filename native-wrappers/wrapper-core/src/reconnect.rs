//! Application reconnect policy. MQTT recovery remains owned by the native event loop.
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{Error, ErrorCode, ErrorKind, Result};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ReconnectPolicy {
    /// Preserves the historical wrapper retry decisions and immediate retries.
    #[default]
    Legacy,
    Classified(ReconnectConfig),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectJitter {
    None,
    /// Uniform delay between zero and the capped exponential delay, inclusive.
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryBudget {
    Limited(u64),
    Unlimited,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconnectConfig {
    pub initial_delay: Duration,
    pub maximum_delay: Duration,
    pub multiplier: u32,
    pub jitter: ReconnectJitter,
    pub budget: RetryBudget,
    pub stability_interval: Duration,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            maximum_delay: Duration::from_secs(60),
            multiplier: 2,
            jitter: ReconnectJitter::Full,
            budget: RetryBudget::Unlimited,
            stability_interval: Duration::from_secs(30),
        }
    }
}

impl ReconnectConfig {
    /// Validate before installing the complete policy.
    ///
    /// # Errors
    /// Rejects invalid growth, delay ordering, or platform timer ranges.
    pub fn validate(&self) -> Result<()> {
        if self.multiplier == 0 || self.initial_delay > self.maximum_delay {
            return Err(Error::configuration("invalid reconnect backoff"));
        }
        let now = Instant::now();
        if [self.maximum_delay, self.stability_interval]
            .iter()
            .any(|duration| now.checked_add(*duration).is_none())
        {
            return Err(Error::configuration(
                "reconnect duration exceeds platform timer range",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum ReconnectPhase {
    #[default]
    Initial = 0,
    Attempting = 1,
    Connected = 2,
    Waiting = 3,
    Stopped = 4,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum ReconnectStopReason {
    #[default]
    None = 0,
    Shutdown = 1,
    TerminalFailure = 2,
    Exhausted = 3,
}

#[derive(Clone, Debug)]
pub struct ReconnectDiagnostics {
    pub classified: bool,
    pub phase: ReconnectPhase,
    /// Lifetime establishment cycles, including the initial free cycle.
    pub cycles_started: u64,
    pub retries_since_reset: u64,
    pub budget: RetryBudget,
    pub reset_count: u64,
    pub remaining_delay: Option<Duration>,
    pub remaining_stability: Option<Duration>,
    pub last_failure: Option<Error>,
    pub stop_reason: ReconnectStopReason,
    pub captured_at: Instant,
}

// Compare owned, public failure details, not opaque source-chain identity.
impl PartialEq for ReconnectDiagnostics {
    fn eq(&self, other: &Self) -> bool {
        let failure_equal = match (&self.last_failure, &other.last_failure) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                a.kind() == b.kind()
                    && a.code() == b.code()
                    && a.retryable() == b.retryable()
                    && a.context() == b.context()
                    && a.delivery_status() == b.delivery_status()
                    && a.message() == b.message()
                    && a.broker_reason() == b.broker_reason()
                    && a.store_failure() == b.store_failure()
                    && a.auth_failure() == b.auth_failure()
                    && a.redirect_failure() == b.redirect_failure()
                    && a.transport_failure() == b.transport_failure()
                    && a.websocket_failure() == b.websocket_failure()
                    && a.tls_callback_failure() == b.tls_callback_failure()
                    && a.ordered_disconnect_failure() == b.ordered_disconnect_failure()
            }
            _ => false,
        };
        self.classified == other.classified
            && self.phase == other.phase
            && self.cycles_started == other.cycles_started
            && self.retries_since_reset == other.retries_since_reset
            && self.budget == other.budget
            && self.reset_count == other.reset_count
            && self.remaining_delay == other.remaining_delay
            && self.remaining_stability == other.remaining_stability
            && self.stop_reason == other.stop_reason
            && self.captured_at == other.captured_at
            && failure_equal
    }
}
impl Eq for ReconnectDiagnostics {}

#[derive(Clone, Debug)]
pub struct ReconnectExhaustion {
    pub cycles_started: u64,
    pub retries_since_reset: u64,
    pub last_failure: Option<Error>,
}

struct State {
    policy: ReconnectPolicy,
    phase: ReconnectPhase,
    cycles: u64,
    retries: u64,
    resets: u64,
    next_base: Duration,
    due: Option<Instant>,
    stable_at: Option<Instant>,
    last: Option<Error>,
    stop: ReconnectStopReason,
}

impl State {
    const fn new(policy: ReconnectPolicy) -> Self {
        let next_base = match &policy {
            ReconnectPolicy::Legacy => Duration::ZERO,
            ReconnectPolicy::Classified(config) => config.initial_delay,
        };
        Self {
            policy,
            phase: ReconnectPhase::Initial,
            cycles: 0,
            retries: 0,
            resets: 0,
            next_base,
            due: None,
            stable_at: None,
            last: None,
            stop: ReconnectStopReason::None,
        }
    }

    fn refresh(&mut self, now: Instant) {
        if self.stable_at.is_some_and(|deadline| deadline <= now) {
            self.stable_at = None;
            self.retries = 0;
            self.resets = self.resets.saturating_add(1);
            if let ReconnectPolicy::Classified(config) = &self.policy {
                self.next_base = config.initial_delay;
            }
        }
    }

    fn start(&mut self, now: Instant) -> Result<()> {
        self.refresh(now);
        if self.phase == ReconnectPhase::Attempting || self.phase == ReconnectPhase::Connected {
            return Ok(());
        }
        if self.phase == ReconnectPhase::Stopped {
            return Err(Error::new(ErrorKind::Shutdown, "reconnect policy stopped"));
        }
        self.check_budget()?;
        if self.cycles > 0 {
            self.retries = self.retries.saturating_add(1);
        }
        self.cycles = self.cycles.saturating_add(1);
        self.phase = ReconnectPhase::Attempting;
        self.due = None;
        Ok(())
    }

    fn check_budget(&mut self) -> Result<()> {
        if self.cycles == 0 {
            return Ok(());
        }
        if let ReconnectPolicy::Classified(config) = &self.policy
            && let RetryBudget::Limited(limit) = config.budget
            && self.retries >= limit
        {
            self.stop = ReconnectStopReason::Exhausted;
            self.phase = ReconnectPhase::Stopped;
            self.due = None;
            return Err(Error::reconnect_exhausted(ReconnectExhaustion {
                cycles_started: self.cycles,
                retries_since_reset: self.retries,
                last_failure: self.last.clone(),
            }));
        }
        Ok(())
    }

    fn connected(&mut self, now: Instant) {
        self.phase = ReconnectPhase::Connected;
        self.due = None;
        self.stable_at = match &self.policy {
            ReconnectPolicy::Legacy => None,
            ReconnectPolicy::Classified(config) => now.checked_add(config.stability_interval),
        };
        self.refresh(now);
    }

    fn connection_ended(&mut self, now: Instant) {
        // Credit only stability earned before the connection ended. Deferred
        // native cleanup and event delivery cannot extend this interval.
        self.refresh(now);
        self.stable_at = None;
    }

    fn failed(&mut self, error: Error, now: Instant, sample: impl FnOnce(Duration) -> Duration) {
        self.connection_ended(now);
        self.last = Some(error);
        let delay = match &self.policy {
            ReconnectPolicy::Legacy => Duration::ZERO,
            ReconnectPolicy::Classified(config) => {
                let base = self.next_base.min(config.maximum_delay);
                self.next_base = base
                    .saturating_mul(config.multiplier)
                    .min(config.maximum_delay);
                match config.jitter {
                    ReconnectJitter::None => base,
                    ReconnectJitter::Full => sample(base).min(base),
                }
            }
        };
        self.due = now.checked_add(delay);
        self.phase = ReconnectPhase::Waiting;
    }

    fn snapshot(&mut self, now: Instant) -> ReconnectDiagnostics {
        self.refresh(now);
        ReconnectDiagnostics {
            classified: matches!(self.policy, ReconnectPolicy::Classified(_)),
            phase: self.phase,
            cycles_started: self.cycles,
            retries_since_reset: self.retries,
            budget: match &self.policy {
                ReconnectPolicy::Legacy => RetryBudget::Unlimited,
                ReconnectPolicy::Classified(config) => config.budget,
            },
            reset_count: self.resets,
            remaining_delay: self.due.map(|due| due.saturating_duration_since(now)),
            remaining_stability: self.stable_at.map(|due| due.saturating_duration_since(now)),
            last_failure: self.last.clone(),
            stop_reason: self.stop,
            captured_at: now,
        }
    }
}

/// One per client, retained for synchronous diagnostics after driver termination.
pub struct Controller {
    state: parking_lot::Mutex<State>,
    random: parking_lot::Mutex<fastrand::Rng>,
}

fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

impl Controller {
    pub(crate) fn new(policy: ReconnectPolicy) -> Arc<Self> {
        Arc::new(Self {
            state: parking_lot::Mutex::new(State::new(policy)),
            random: parking_lot::Mutex::new(fastrand::Rng::new()),
        })
    }

    pub(crate) fn snapshot(&self) -> ReconnectDiagnostics {
        self.state.lock().snapshot(now())
    }

    pub(crate) fn classified(&self) -> bool {
        matches!(self.state.lock().policy, ReconnectPolicy::Classified(_))
    }

    pub(crate) fn start(&self) -> Result<()> {
        self.state.lock().start(now())
    }

    pub(crate) fn check_budget(&self) -> Result<()> {
        self.state.lock().check_budget()
    }

    pub(crate) fn abandoned(&self) {
        let mut state = self.state.lock();
        state.phase = ReconnectPhase::Waiting;
        state.stable_at = None;
        // Recovery uses the remaining budget and existing delay; it grants no free cycle.
    }

    pub(crate) fn connected(&self) {
        self.state.lock().connected(now());
    }

    pub(crate) fn connection_ended(&self) {
        self.state.lock().connection_ended(now());
    }

    pub(crate) fn failed(&self, error: Error) {
        self.state.lock().failed(error, now(), |base| {
            let nanos = self.random.lock().u128(0..=base.as_nanos());
            Duration::new(
                u64::try_from(nanos / 1_000_000_000).expect("sample is bounded by Duration"),
                (nanos % 1_000_000_000) as u32,
            )
        });
    }

    pub(crate) fn refresh(&self) {
        self.state.lock().refresh(now());
    }

    pub(crate) fn terminate(&self, failure: Option<&Error>) {
        let mut state = self.state.lock();
        state.refresh(now());
        state.phase = ReconnectPhase::Stopped;
        state.due = None;
        state.stable_at = None;
        state.stop = match failure {
            None => ReconnectStopReason::Shutdown,
            Some(error) if error.code() == ErrorCode::ReconnectExhausted => {
                ReconnectStopReason::Exhausted
            }
            Some(_) => ReconnectStopReason::TerminalFailure,
        };
        if let Some(error) = failure
            && error.code() != ErrorCode::ReconnectExhausted
        {
            state.last = Some(error.clone());
        }
    }

    pub(crate) async fn stability(&self) {
        let deadline = self.state.lock().stable_at;
        if let Some(deadline) = deadline {
            tokio::time::sleep_until(deadline.into()).await;
        } else {
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(budget: u64) -> State {
        State::new(ReconnectPolicy::Classified(ReconnectConfig {
            initial_delay: Duration::from_millis(10),
            maximum_delay: Duration::from_millis(25),
            multiplier: 2,
            jitter: ReconnectJitter::Full,
            budget: RetryBudget::Limited(budget),
            stability_interval: Duration::from_secs(30),
        }))
    }

    fn failure() -> Error {
        Error::new(ErrorKind::Network, "transport failed")
    }

    #[test]
    fn initial_cycle_is_free_and_exhaustion_preserves_last_failure() {
        let now = Instant::now();
        for limit in [0, 1, 3] {
            let mut state = policy(limit);
            state.start(now).unwrap();
            for _ in 0..limit {
                state.failed(failure(), now, |_| Duration::ZERO);
                state.start(now).unwrap();
                // Buffered events/polls cannot debit another retry.
                state.start(now).unwrap();
            }
            state.failed(failure(), now, |_| Duration::ZERO);
            let error = state.check_budget().unwrap_err();
            assert_eq!(error.code(), ErrorCode::ReconnectExhausted);
            assert!(!error.retryable());
            let exhaustion = error.reconnect_exhaustion().unwrap();
            assert_eq!(exhaustion.cycles_started, limit + 1);
            assert_eq!(exhaustion.retries_since_reset, limit);
            assert_eq!(
                exhaustion.last_failure.as_ref().unwrap().code(),
                ErrorCode::Network
            );
        }
    }

    #[test]
    fn backoff_caps_and_injected_jitter_cover_both_bounds() {
        let now = Instant::now();
        let mut state = policy(10);
        state.start(now).unwrap();
        for expected in [10, 20, 25, 25] {
            state.failed(failure(), now, |base| base);
            assert_eq!(
                state.snapshot(now).remaining_delay,
                Some(Duration::from_millis(expected))
            );
            state.start(now + Duration::from_secs(1)).unwrap();
        }
        state.failed(failure(), now, |_| Duration::ZERO);
        assert_eq!(state.snapshot(now).remaining_delay, Some(Duration::ZERO));
    }

    #[test]
    fn idle_stability_resets_once_but_short_connections_do_not() {
        let now = Instant::now();
        let mut state = policy(1);
        state.start(now).unwrap();
        state.failed(failure(), now, |_| Duration::ZERO);
        state.start(now).unwrap();
        state.connected(now);
        assert_eq!(
            state
                .snapshot(now + Duration::from_secs(29))
                .retries_since_reset,
            1
        );
        let stable = state.snapshot(now + Duration::from_secs(30));
        assert_eq!(stable.retries_since_reset, 0);
        assert_eq!(stable.reset_count, 1);
        assert_eq!(state.snapshot(now + Duration::from_secs(60)).reset_count, 1);
        state.failed(failure(), now + Duration::from_secs(60), |base| base);
        assert_eq!(
            state
                .snapshot(now + Duration::from_secs(60))
                .remaining_delay,
            Some(Duration::from_millis(10))
        );
        state.start(now + Duration::from_secs(61)).unwrap();
        state.connected(now + Duration::from_secs(61));
        state.failed(failure(), now + Duration::from_secs(62), |_| Duration::ZERO);
        assert_eq!(
            state.check_budget().unwrap_err().code(),
            ErrorCode::ReconnectExhausted
        );
    }

    #[test]
    fn deferred_failure_cannot_extend_connection_stability() {
        let now = Instant::now();
        for (seconds_connected, earned_reset) in [(29, false), (30, true)] {
            let mut state = policy(1);
            state.start(now).unwrap();
            state.failed(failure(), now, |_| Duration::ZERO);
            state.start(now).unwrap();
            state.connected(now);
            state.connection_ended(now + Duration::from_secs(seconds_connected));
            let later = now + Duration::from_secs(60);
            let snapshot = state.snapshot(later);
            assert_eq!(snapshot.retries_since_reset, u64::from(!earned_reset));
            assert_eq!(snapshot.reset_count, u64::from(earned_reset));
            assert_eq!(snapshot.remaining_stability, None);
            state.connection_ended(later);
            state.failed(failure(), later, |base| base);
            let snapshot = state.snapshot(later);
            assert_eq!(snapshot.reset_count, u64::from(earned_reset));
            assert_eq!(
                snapshot.remaining_delay,
                Some(Duration::from_millis(if earned_reset { 10 } else { 20 }))
            );
            if earned_reset {
                state.start(later).unwrap();
                assert_eq!(state.cycles, 3);
            } else {
                assert_eq!(
                    state.start(later).unwrap_err().code(),
                    ErrorCode::ReconnectExhausted
                );
                assert_eq!(state.cycles, 2);
            }
        }
    }

    #[test]
    fn zero_stability_and_zero_backoff_are_explicit() {
        let now = Instant::now();
        let mut state = State::new(ReconnectPolicy::Classified(ReconnectConfig {
            initial_delay: Duration::ZERO,
            maximum_delay: Duration::ZERO,
            stability_interval: Duration::ZERO,
            budget: RetryBudget::Limited(1),
            ..ReconnectConfig::default()
        }));
        state.start(now).unwrap();
        for _ in 0..10 {
            state.connected(now);
            state.failed(failure(), now, |base| base);
            assert_eq!(state.snapshot(now).remaining_delay, Some(Duration::ZERO));
            state.start(now).unwrap();
        }
        assert_eq!(state.resets, 10);
    }

    #[test]
    fn legacy_and_unlimited_policies_never_exhaust() {
        let now = Instant::now();
        for policy in [
            ReconnectPolicy::Legacy,
            ReconnectPolicy::Classified(ReconnectConfig::default()),
        ] {
            let mut state = State::new(policy);
            state.start(now).unwrap();
            for _ in 0..100 {
                state.failed(failure(), now, |_| Duration::ZERO);
                state.start(now).unwrap();
            }
            assert_eq!(state.cycles, 101);
        }
    }

    #[test]
    fn invalid_ranges_and_saturating_growth_are_checked() {
        let mut config = ReconnectConfig {
            multiplier: 0,
            ..ReconnectConfig::default()
        };
        assert!(config.validate().is_err());
        config.multiplier = u32::MAX;
        config.maximum_delay = Duration::ZERO;
        assert!(config.validate().is_err());
        config.maximum_delay = Duration::MAX;
        assert!(config.validate().is_err());
        config.maximum_delay = Duration::from_secs(60);
        config.validate().unwrap();
        let mut state = State::new(ReconnectPolicy::Classified(config));
        let now = Instant::now();
        state.start(now).unwrap();
        state.failed(failure(), now, |base| base);
        state.start(now + Duration::from_secs(1)).unwrap();
        state.failed(failure(), now, |base| base);
        assert_eq!(
            state.snapshot(now).remaining_delay,
            Some(Duration::from_secs(60))
        );
    }
}
