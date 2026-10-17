use super::acknowledgement_tests::{ack_token, client};
use super::*;
use crate::pending_admission::Registry;
use crate::{PendingAdmissionLimits, PendingAdmissionOutcome};

const DEADLINE: Duration = Duration::from_secs(3);

fn pending(handle: &ClientHandle, token: AckToken) -> crate::PendingAdmissionHandle {
    handle
        .defer_admission(
            handle.reserve_pending_admission(0).unwrap(),
            Command::Acknowledge(token),
        )
        .unwrap()
}

fn owner(handle: &ClientHandle) -> std::thread::JoinHandle<()> {
    let runner = Registry::runner(handle.shared.clone());
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(runner.run());
    })
}

fn stop(handle: &ClientHandle, owner: std::thread::JoinHandle<()>) {
    handle
        .shared
        .pending
        .close(Error::new(ErrorKind::Shutdown, "test stopped"));
    owner.join().unwrap();
    let usage = handle.pending_admission_snapshot();
    assert_eq!((usage.pending, usage.retained_bytes), (0, 0));
}

#[test]
fn cancellation_before_poll_proves_no_admission_and_restores_ack_authority() {
    let (handle, requests) = client();
    let token = ack_token(&handle, 1);
    let pending = pending(&handle, token);
    pending.cancel();
    let worker = owner(&handle);
    assert!(matches!(
        pending.wait_timeout(DEADLINE).unwrap(),
        PendingAdmissionOutcome::Cancelled
    ));
    assert!(requests.try_recv().is_err());
    handle.try_admit(Command::Acknowledge(token)).unwrap();
    assert!(requests.try_recv().is_ok());
    stop(&handle, worker);
}

#[test]
fn admission_wins_cancellation_between_native_send_and_result_publication() {
    let (handle, requests) = client();
    let pending = pending(&handle, ack_token(&handle, 1));
    let (entered, arrived) = flume::bounded(1);
    let (release, proceed) = flume::bounded(1);
    pending.after_attempt(entered, proceed);
    let worker = owner(&handle);
    arrived.recv_timeout(DEADLINE).unwrap();
    assert!(requests.try_recv().is_ok());
    assert!(pending.poll().is_none());
    pending.cancel();
    release.send(()).unwrap();
    let PendingAdmissionOutcome::Admitted(admission) = pending.wait_timeout(DEADLINE).unwrap()
    else {
        panic!("native admission was lost")
    };
    let PendingAdmissionOutcome::Admitted(again) = pending.poll().unwrap() else {
        panic!()
    };
    assert_eq!(admission.operation_id, again.operation_id);
    assert!(requests.try_recv().is_err());
    stop(&handle, worker);
}

#[test]
fn recovery_epoch_failure_wins_cancellation_before_result_publication() {
    let (handle, requests) = client();
    let token = ack_token(&handle, 1);
    let pending = pending(&handle, token);
    handle.shared.admission_epoch.fetch_add(1, Ordering::AcqRel);
    let (entered, arrived) = flume::bounded(1);
    let (release, proceed) = flume::bounded(1);
    pending.after_attempt(entered, proceed);
    let worker = owner(&handle);
    arrived.recv_timeout(DEADLINE).unwrap();
    pending.cancel();
    release.send(()).unwrap();
    let PendingAdmissionOutcome::Failed(error) = pending.wait_timeout(DEADLINE).unwrap() else {
        panic!()
    };
    assert!(error.recovery_failure().is_some());
    assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    assert!(requests.try_recv().is_err());
    handle.try_admit(Command::Acknowledge(token)).unwrap();
    stop(&handle, worker);
}

#[test]
fn dropping_unpolled_admission_owner_settles_results_and_releases_storage() {
    let (handle, requests) = client();
    let token = ack_token(&handle, 1);
    let pending = pending(&handle, token);
    drop(Registry::runner(handle.shared.clone()));
    assert!(matches!(
        pending.wait_timeout(DEADLINE).unwrap(),
        PendingAdmissionOutcome::Failed(_)
    ));
    assert_eq!(handle.pending_admission_snapshot().pending, 0);
    assert!(requests.try_recv().is_err());
    handle.try_admit(Command::Acknowledge(token)).unwrap();
}

#[test]
fn reservation_bounds_are_shared_checked_and_rolled_back_without_registration() {
    let mut registry = Registry::new();
    registry.set_limits(PendingAdmissionLimits {
        max_pending: 2,
        max_bytes: 8,
    });
    let first = registry.reserve(6, 0).unwrap();
    assert_eq!(
        registry.reserve(3, 0).err().unwrap().kind(),
        ErrorKind::Backpressure
    );
    assert_eq!(
        registry.reserve(usize::MAX, 0).err().unwrap().kind(),
        ErrorKind::Admission
    );
    let second = registry.reserve(2, 0).unwrap();
    assert_eq!(
        registry.reserve(0, 0).err().unwrap().kind(),
        ErrorKind::Backpressure
    );
    drop(first);
    drop(second);
    assert_eq!(
        (
            registry.snapshot().pending,
            registry.snapshot().retained_bytes
        ),
        (0, 0)
    );
}

#[test]
fn pending_reservations_enforce_one_budget_across_concurrent_producers() {
    let mut registry = Registry::new();
    registry.set_limits(PendingAdmissionLimits {
        max_pending: 4,
        max_bytes: 16,
    });
    let registry = Arc::new(registry);
    let barrier = Arc::new(std::sync::Barrier::new(17));
    let mut workers = Vec::new();
    for _ in 0..16 {
        let registry = registry.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let reservation = registry.reserve(4, 0);
            barrier.wait();
            barrier.wait();
            reservation.is_ok()
        }));
    }
    barrier.wait();
    assert_eq!(
        (
            registry.snapshot().pending,
            registry.snapshot().retained_bytes
        ),
        (4, 16)
    );
    barrier.wait();
    assert_eq!(
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(|accepted| *accepted)
            .count(),
        4
    );
    assert_eq!(registry.snapshot().pending, 0);
}

#[test]
fn pending_observer_deadlines_make_no_delivery_claim_and_callback_waits_are_rejected() {
    let (handle, _requests) = client();
    let pending = pending(&handle, ack_token(&handle, 1));
    let error = pending.wait_timeout(Duration::ZERO).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert_eq!(error.delivery_status(), DeliveryStatus::NotApplicable);
    crate::runtime::with_host_callback(|| {
        assert_eq!(
            pending.wait_timeout(DEADLINE).unwrap_err().code(),
            crate::ErrorCode::InvalidState
        );
        assert!(pending.poll().is_none());
        assert_eq!(
            pending.wait_timeout(Duration::ZERO).unwrap_err().kind(),
            ErrorKind::Timeout
        );
    });
    drop(Registry::runner(handle.shared.clone()));
}

#[cfg(feature = "readiness")]
#[test]
fn final_readiness_deregistration_racing_native_admission_preserves_single_use_ack_authority() {
    let (handle, requests) = client();
    let token = ack_token(&handle, 1);
    let pending = pending(&handle, token);
    let context = crate::readiness::NotificationContext::new(1).unwrap();
    let key = context.register_pending_admission(&pending, 0).unwrap();
    let (entered, arrived) = flume::bounded(1);
    let (release, proceed) = flume::bounded(1);
    pending.after_attempt(entered, proceed);
    drop(pending);
    let worker = owner(&handle);
    arrived.recv_timeout(DEADLINE).unwrap();
    assert!(requests.try_recv().is_ok());
    context.deregister(key).unwrap();
    release.send(()).unwrap();
    stop(&handle, worker);
    assert!(handle.try_admit(Command::Acknowledge(token)).is_err());
    assert!(requests.try_recv().is_err());
}
