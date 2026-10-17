use super::*;
use crate::completion::CompletionCell;
use crate::{Completion, OperationId};
use std::num::NonZeroU64;
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(super) struct Gate {
    entered: flume::Sender<()>,
    proceed: flume::Receiver<()>,
}
pub(super) fn pause(hook: &Mutex<Option<Gate>>) {
    let gate = lock(hook).take();
    if let Some(gate) = gate {
        gate.entered.send(()).unwrap();
        gate.proceed.recv_timeout(DEADLINE).unwrap();
    }
}
fn gate(hook: &Mutex<Option<Gate>>) -> (flume::Receiver<()>, flume::Sender<()>) {
    let (entered, arrived) = flume::bounded(1);
    let (release, proceed) = flume::bounded(1);
    *lock(hook) = Some(Gate { entered, proceed });
    (arrived, release)
}
fn completion() -> (Arc<CompletionCell>, CompletionHandle) {
    let cell = CompletionCell::new(OperationId(NonZeroU64::new(1).unwrap()));
    (cell.clone(), CompletionHandle::new(cell))
}
fn entries(context: &NotificationContext) -> Vec<ReadyEntry> {
    let mut entries = Vec::new();
    context.collect(128, |entry| entries.push(entry)).unwrap();
    entries
}
fn event_source() -> (
    EventReadiness,
    Arc<EventView>,
    flume::Receiver<WrapperEvent>,
) {
    let source = Source::new(SourceKind::Events);
    let (sender, receiver) = flume::bounded(8);
    let view = source.event_view(sender);
    (EventReadiness(source), view, receiver)
}

#[test]
fn immutable_results_notify_once_per_registration_and_remain_readable() {
    let context = NotificationContext::new(2).unwrap();
    let (cell, handle) = completion();
    let key = context.register_completion(&handle, 7).unwrap();
    assert!(entries(&context).is_empty());
    assert!(context.acknowledge(key, 1).is_err());
    assert!(!context.prepare_wait().unwrap());
    cell.complete(Ok(Completion::Acknowledged));
    let entry = entries(&context)[0];
    assert_eq!(
        (entry.key, entry.tag, entry.kind),
        (key, 7, SourceKind::Completion)
    );
    context.acknowledge(key, entry.revision).unwrap();
    assert!(!context.prepare_wait().unwrap());
    assert_eq!(handle.try_wait().unwrap(), Some(Completion::Acknowledged));
    assert!(entries(&context).is_empty());
    let late = context.register_completion(&handle, 8).unwrap();
    assert_eq!(entries(&context)[0].key, late);
    context.deregister(key).unwrap();
    context.deregister(late).unwrap();
}

#[test]
fn publication_during_subscription_is_not_lost() {
    let context = NotificationContext::new(1).unwrap();
    let (cell, handle) = completion();
    let (arrived, release) = gate(&context.0.after_subscribe);
    let registering = context.clone();
    let worker = std::thread::spawn(move || registering.register_completion(&handle, 1).unwrap());
    arrived.recv_timeout(DEADLINE).unwrap();
    cell.complete(Ok(Completion::Acknowledged));
    release.send(()).unwrap();
    let key = worker.join().unwrap();
    assert_eq!(entries(&context)[0].key, key);
    assert!(context.prepare_wait().unwrap());
}

#[test]
fn publication_after_os_drain_before_reset_is_not_lost() {
    let context = NotificationContext::new(1).unwrap();
    let (cell, handle) = completion();
    context.register_completion(&handle, 1).unwrap();
    let (arrived, release) = gate(&context.0.after_drain);
    let preparing = context.clone();
    let worker = std::thread::spawn(move || preparing.prepare_wait().unwrap());
    arrived.recv_timeout(DEADLINE).unwrap();
    cell.complete(Ok(Completion::Acknowledged));
    release.send(()).unwrap();
    assert!(worker.join().unwrap());
    assert_eq!(entries(&context).len(), 1);
}

#[test]
fn partial_event_drain_and_newer_publications_survive_acknowledgement() {
    let context = NotificationContext::new(1).unwrap();
    let (source, view, receiver) = event_source();
    let key = context.register_events(&source, 1).unwrap();
    for _ in 0..2 {
        view.0
            .send(WrapperEvent::ImmediateShutdownCompleted)
            .unwrap();
        source.0.publish();
    }
    let old = entries(&context)[0];
    receiver.try_recv().unwrap();
    context.acknowledge(key, old.revision).unwrap();
    assert!(context.prepare_wait().unwrap());
    assert_eq!(entries(&context).len(), 1);
    receiver.try_recv().unwrap();
    view.0
        .send(WrapperEvent::ImmediateShutdownCompleted)
        .unwrap();
    source.0.publish();
    context.acknowledge(key, old.revision).unwrap();
    receiver.try_recv().unwrap();
    assert!(context.prepare_wait().unwrap());
    let new = entries(&context)[0];
    assert!(new.revision > old.revision);
    context.acknowledge(key, new.revision).unwrap();
    assert!(!context.prepare_wait().unwrap());
}

#[test]
fn terminal_only_and_consumer_invalidation_have_separate_readiness() {
    let context = NotificationContext::new(1).unwrap();
    let (source, view, _) = event_source();
    let key = context.register_events(&source, 1).unwrap();
    source.0.terminal_published();
    let terminal = entries(&context)[0];
    context.acknowledge(key, terminal.revision).unwrap();
    assert!(context.prepare_wait().unwrap());
    source.0.terminal_consumed();
    assert!(!context.prepare_wait().unwrap());
    drop(view);
    source.0.invalidate();
    let invalid = entries(&context)[0];
    assert!(invalid.invalidated);
    context.acknowledge(key, invalid.revision).unwrap();
    assert!(!context.prepare_wait().unwrap());
}

#[test]
fn invalidation_during_collection_survives_acknowledging_the_returned_revision() {
    for published in [false, true] {
        let context = NotificationContext::new(1).unwrap();
        let (source, view, _receiver) = event_source();
        let key = context.register_events(&source, 0).unwrap();
        view.0
            .send(WrapperEvent::ImmediateShutdownCompleted)
            .unwrap();
        if published {
            source.0.publish();
        }
        // Also cover an enqueue visible before its first publication hook.
        let (arrived, release) = gate(&source.0.before_entry);
        let collecting = context.clone();
        let worker = std::thread::spawn(move || entries(&collecting));
        arrived.recv_timeout(DEADLINE).unwrap();
        drop(view);
        source.0.invalidate();
        release.send(()).unwrap();
        let observed = worker.join().unwrap()[0];
        assert!(!observed.invalidated);
        assert!(observed.revision < source.0.revision.load(SeqCst));
        context.acknowledge(key, observed.revision).unwrap();
        assert!(context.prepare_wait().unwrap());
        let invalidation = entries(&context)[0];
        assert!(invalidation.invalidated);
        context.acknowledge(key, invalidation.revision).unwrap();
        assert!(!context.prepare_wait().unwrap());
    }
}

#[test]
fn collection_visitor_can_close_the_context_and_stops_further_visits() {
    let context = NotificationContext::new(2).unwrap();
    let (cell, handle) = completion();
    cell.complete(Ok(Completion::Acknowledged));
    for tag in 0..2 {
        context.register_completion(&handle, tag).unwrap();
    }
    let (finished, receive) = flume::bounded(1);
    let worker = std::thread::spawn(move || {
        let visited = context.collect(2, |_| context.request_close()).unwrap();
        finished.send((visited, context.status().unwrap())).unwrap();
    });
    let (visited, status) = receive.recv_timeout(DEADLINE).unwrap();
    worker.join().unwrap();
    assert_eq!(visited, 1);
    assert_eq!(status.state, NotificationState::Quiescent);
    assert_eq!(status.registrations, 0);
}

#[test]
fn collection_visitors_can_observe_sources_but_reject_nested_and_concurrent_collection() {
    let context = NotificationContext::new(2).unwrap();
    let (cell, handle) = completion();
    cell.complete(Ok(Completion::Acknowledged));
    for tag in 0..2 {
        context.register_completion(&handle, tag).unwrap();
    }
    let visited = context
        .collect(2, |entry| {
            assert_eq!(
                context.collect(1, |_| {}).unwrap_err().code(),
                ErrorCode::InvalidState
            );
            let concurrent = context.clone();
            assert_eq!(
                std::thread::spawn(move || concurrent.collect(1, |_| {}))
                    .join()
                    .unwrap()
                    .unwrap_err()
                    .code(),
                ErrorCode::InvalidState
            );
            context.acknowledge(entry.key, entry.revision).unwrap();
            context.deregister(entry.key).unwrap();
        })
        .unwrap();
    assert_eq!(visited, 2);
    assert_eq!(context.usage().unwrap(), (0, 2));
    assert_eq!(context.collect(2, |_| {}).unwrap(), 0);
}

#[test]
fn a_panicking_visitor_releases_collection_ownership_without_poisoning_the_table() {
    let context = NotificationContext::new(1).unwrap();
    let (cell, handle) = completion();
    cell.complete(Ok(Completion::Acknowledged));
    context.register_completion(&handle, 0).unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = context.collect(1, |_| panic!("visitor panic"));
        }))
        .is_err()
    );
    assert_eq!(entries(&context).len(), 1);
    context.request_close();
    assert_eq!(context.state(), NotificationState::Quiescent);
}

#[test]
fn capacity_fanout_and_failed_registration_rollback_are_bounded() {
    let context = NotificationContext::new(1).unwrap();
    let (_, handle) = completion();
    let key = context.register_completion(&handle, 0).unwrap();
    assert_eq!(
        context.register_completion(&handle, 1).unwrap_err().kind(),
        ErrorKind::Backpressure
    );
    context.deregister(key).unwrap();
    let contexts: Vec<_> = (0..MAX_SOURCE_SUBSCRIBERS)
        .map(|_| {
            let context = NotificationContext::new(1).unwrap();
            context.register_completion(&handle, 0).unwrap();
            context
        })
        .collect();
    assert_eq!(
        context.register_completion(&handle, 0).unwrap_err().kind(),
        ErrorKind::Backpressure
    );
    assert_eq!(context.usage().unwrap(), (0, 1));
    drop(contexts);
    assert!(context.register_completion(&handle, 0).is_ok());
}

#[test]
fn reused_slots_reject_old_keys_and_acknowledgements() {
    let context = NotificationContext::new(1).unwrap();
    let (first, handle) = completion();
    let old = context.register_completion(&handle, 0).unwrap();
    let (arrived, release) = gate(&handle.notification_source().after_snapshot);
    let worker = std::thread::spawn(move || first.complete(Ok(Completion::Acknowledged)));
    arrived.recv_timeout(DEADLINE).unwrap();
    context.deregister(old).unwrap();
    let (second, handle) = completion();
    let new = context.register_completion(&handle, 0).unwrap();
    assert_eq!(old.slot, new.slot);
    assert_ne!(old.generation, new.generation);
    release.send(()).unwrap();
    worker.join().unwrap();
    assert!(entries(&context).is_empty());
    assert!(context.deregister(old).is_err());
    assert!(context.acknowledge(old, 1).is_err());
    second.complete(Ok(Completion::Acknowledged));
    assert_eq!(entries(&context)[0].key, new);
}

#[test]
fn close_observes_inflight_signaling_and_keeps_os_resources_owned() {
    let context = NotificationContext::new(1).unwrap();
    let (cell, handle) = completion();
    context.register_completion(&handle, 0).unwrap();
    let (arrived, release) = gate(&context.0.signal.before_write);
    let worker = std::thread::spawn(move || cell.complete(Ok(Completion::Acknowledged)));
    arrived.recv_timeout(DEADLINE).unwrap();
    context.request_close();
    assert_eq!(context.state(), NotificationState::Closing);
    assert_eq!(context.usage().unwrap(), (0, 1));
    assert!(context.register_completion(&handle, 0).is_err());
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(context.state(), NotificationState::Quiescent);
    assert_eq!(handle.try_wait().unwrap(), Some(Completion::Acknowledged));
    context.request_close();
    assert!(context.prepare_wait().is_err());
}

#[test]
fn rotating_scan_services_more_ready_sources_than_the_output_buffer() {
    let context = NotificationContext::new(4).unwrap();
    for tag in 0..4 {
        let (cell, handle) = completion();
        cell.complete(Ok(Completion::Acknowledged));
        context.register_completion(&handle, tag).unwrap();
    }
    let mut tags = Vec::new();
    for _ in 0..4 {
        context.collect(1, |entry| tags.push(entry.tag)).unwrap();
    }
    assert_eq!(tags, vec![0, 1, 2, 3]);
}

#[test]
fn counter_exhaustion_is_observable_without_wrapping() {
    let context = NotificationContext::new(1).unwrap();
    let (_, handle) = completion();
    context.register_completion(&handle, 0).unwrap();
    handle
        .notification_source()
        .revision
        .store(u64::MAX, SeqCst);
    handle.notification_source().publish();
    assert_eq!(context.signal_error(), -1);
    assert!(context.prepare_wait().is_err());
    let context = NotificationContext::new(1).unwrap();
    lock(&context.0.table).next_generation = u64::MAX;
    assert!(context.register_completion(&handle, 0).is_err());
    assert_eq!(context.usage().unwrap(), (0, 1));
}

#[cfg(unix)]
#[test]
fn saturated_os_buffer_does_not_erase_ready_state_or_fail_mqtt() {
    use std::io::Write;
    use std::os::fd::AsFd;
    let context = NotificationContext::new(1).unwrap();
    // Fill through a duplicate of the writer, deliberately bypassing coalescing.
    // A real host never writes either exported resource.
    let writer = context.0.signal.primitive.test_writer();
    let mut bytes = 0;
    while let Ok(n) = (&writer).write(&[1; 4096]) {
        bytes += n;
    }
    assert!(bytes > 0);
    let (cell, handle) = completion();
    context.register_completion(&handle, 0).unwrap();
    cell.complete(Ok(Completion::Acknowledged));
    assert_eq!(context.signal_error(), 0);
    assert_eq!(entries(&context).len(), 1);
    assert!(context.prepare_wait().unwrap());
    let duplicate = context.duplicate_fd().unwrap();
    context.request_close();
    assert!(duplicate.as_fd().try_clone_to_owned().is_ok());
}

#[test]
fn releasing_the_context_during_a_signal_retains_resources_until_the_lease_finishes() {
    let context = NotificationContext::new(1).unwrap();
    let signal = Arc::downgrade(&context.0.signal);
    let (cell, handle) = completion();
    context.register_completion(&handle, 0).unwrap();
    let (arrived, release) = gate(&context.0.signal.before_write);
    let worker = std::thread::spawn(move || cell.complete(Ok(Completion::Acknowledged)));
    arrived.recv_timeout(DEADLINE).unwrap();
    drop(context);
    assert!(signal.upgrade().is_some());
    release.send(()).unwrap();
    worker.join().unwrap();
    assert!(signal.upgrade().is_none());
    assert_eq!(handle.try_wait().unwrap(), Some(Completion::Acknowledged));
}
