use super::*;

#[cfg(any(unix, windows))]
pub(crate) fn fail_queued_after_coordinator_panic(receiver: &Receiver<CoordinatorEvent>) {
    while let Ok(event) = receiver.try_recv() {
        match event {
            CoordinatorEvent::Submission(submission) => {
                deliver_error(submission.operation, AtomicBlobStoreError::EngineFailed);
            }
            CoordinatorEvent::Maintenance(submission) => {
                let _ = submission
                    .sender
                    .send(Err(AtomicBlobStoreError::EngineFailed));
            }
            CoordinatorEvent::MaintenanceCompletion(completion) => {
                if let Some((sender, _)) = completion.outcome {
                    let _ = sender.send(Err(AtomicBlobStoreError::EngineFailed));
                }
            }
            CoordinatorEvent::Flush(sender) => {
                let _ = sender.send(Err(AtomicBlobStoreError::EngineFailed));
            }
            CoordinatorEvent::Close(close) => {
                let _ = close
                    .sender
                    .send(Err(AtomicBlobStoreError::ShutdownFailure));
            }
            CoordinatorEvent::Completion(completion) => {
                if let Some((operation, _)) = completion.outcome {
                    deliver_error(operation, AtomicBlobStoreError::EngineFailed);
                }
            }
        }
    }
}

#[cfg(any(unix, windows))]
#[allow(clippy::too_many_lines)]
pub(crate) fn run_scheduler(
    config: &Arc<StoreConfig>,
    receiver: &Receiver<CoordinatorEvent>,
    lifecycle: &Arc<Mutex<Lifecycle>>,
    mut worker_pool: WorkerPool,
    #[cfg(all(test, any(unix, windows)))] registry_entries: &std::sync::atomic::AtomicUsize,
) {
    let mut queues: HashMap<[u8; 32], VecDeque<QueuedOperation>> = HashMap::new();
    let mut active = HashSet::new();
    let mut pending = VecDeque::new();
    let mut maintenance_active = false;

    while let Ok(event) = receiver.recv() {
        #[cfg(all(test, any(unix, windows)))]
        if let Some(hook) = &config.hook {
            hook(TestStage::CoordinatorEvent).expect("test-requested coordinator failure");
        }
        match event {
            CoordinatorEvent::Submission(submission) => {
                if !matches!(
                    *lifecycle.lock().expect("lifecycle lock poisoned"),
                    Lifecycle::Open
                ) {
                    deliver_error(submission.operation, AtomicBlobStoreError::StoreClosed);
                    continue;
                }
                if maintenance_active || !pending.is_empty() {
                    pending.push_back(PendingEvent::Submission(submission));
                    continue;
                }
                let key_hash = submission.key_hash;
                queues
                    .entry(key_hash)
                    .or_default()
                    .push_back(QueuedOperation {
                        operation: submission.operation,
                        completion_sender: submission.completion_sender,
                    });
                #[cfg(all(test, any(unix, windows)))]
                registry_entries.store(queues.len(), std::sync::atomic::Ordering::SeqCst);
                dispatch_if_idle(key_hash, config, &mut queues, &mut active, &mut worker_pool);
                dispatch_available(config, &mut queues, &mut active, &mut worker_pool);
                #[cfg(all(test, any(unix, windows)))]
                registry_entries.store(queues.len(), std::sync::atomic::Ordering::SeqCst);
            }
            CoordinatorEvent::Completion(completion) => {
                let outcome = completion.outcome;
                active.remove(&completion.key_hash);
                dispatch_if_idle(
                    completion.key_hash,
                    config,
                    &mut queues,
                    &mut active,
                    &mut worker_pool,
                );
                dispatch_available(config, &mut queues, &mut active, &mut worker_pool);
                if !active.contains(&completion.key_hash)
                    && queues
                        .get(&completion.key_hash)
                        .is_some_and(VecDeque::is_empty)
                {
                    queues.remove(&completion.key_hash);
                }
                #[cfg(all(test, any(unix, windows)))]
                registry_entries.store(queues.len(), std::sync::atomic::Ordering::SeqCst);
                if let Some((operation, result)) = outcome {
                    deliver(operation, result);
                }
                advance_pending_if_ready(
                    config,
                    &mut queues,
                    &mut active,
                    &mut pending,
                    &mut maintenance_active,
                    &mut worker_pool,
                );
            }
            CoordinatorEvent::Maintenance(submission) => {
                if !matches!(
                    *lifecycle.lock().expect("lifecycle lock poisoned"),
                    Lifecycle::Open
                ) {
                    let _ = submission
                        .sender
                        .send(Err(AtomicBlobStoreError::StoreClosed));
                    continue;
                }
                pending.push_back(PendingEvent::Maintenance(submission));
                advance_pending_if_ready(
                    config,
                    &mut queues,
                    &mut active,
                    &mut pending,
                    &mut maintenance_active,
                    &mut worker_pool,
                );
            }
            CoordinatorEvent::MaintenanceCompletion(completion) => {
                maintenance_active = false;
                if let Some((sender, result)) = completion.outcome {
                    let _send_result = sender.send(result);
                }
                advance_pending_if_ready(
                    config,
                    &mut queues,
                    &mut active,
                    &mut pending,
                    &mut maintenance_active,
                    &mut worker_pool,
                );
            }
            CoordinatorEvent::Flush(sender) => {
                if !matches!(
                    *lifecycle.lock().expect("lifecycle lock poisoned"),
                    Lifecycle::Open
                ) {
                    let _ = sender.send(Err(AtomicBlobStoreError::StoreClosed));
                    continue;
                }
                #[cfg(feature = "bench-instrumentation")]
                emit_benchmark_event(
                    config,
                    crate::bench_instrumentation::BenchmarkEvent::FlushAccepted,
                );
                pending.push_back(PendingEvent::Flush(sender));
                advance_pending_if_ready(
                    config,
                    &mut queues,
                    &mut active,
                    &mut pending,
                    &mut maintenance_active,
                    &mut worker_pool,
                );
            }
            CoordinatorEvent::Close(submission) => {
                let mut state = lifecycle.lock().expect("lifecycle lock poisoned");
                match *state {
                    Lifecycle::Open => {
                        *state = Lifecycle::Closing;
                        drop(state);
                        pending.push_back(PendingEvent::Close(submission));
                    }
                    Lifecycle::Closing => {
                        drop(state);
                        pending.push_back(PendingEvent::Close(submission));
                    }
                    Lifecycle::Closed => {
                        drop(state);
                        let _ = submission.sender.send(Ok(()));
                    }
                    Lifecycle::ShutdownFailed | Lifecycle::Failed => {
                        drop(state);
                        let _ = submission
                            .sender
                            .send(Err(AtomicBlobStoreError::ShutdownFailure));
                    }
                }
                advance_pending_if_ready(
                    config,
                    &mut queues,
                    &mut active,
                    &mut pending,
                    &mut maintenance_active,
                    &mut worker_pool,
                );
            }
        }
        if pending
            .front()
            .is_some_and(|event| matches!(event, PendingEvent::Close(_)))
            && active.is_empty()
            && queues.values().all(VecDeque::is_empty)
            && !maintenance_active
        {
            let outcome = worker_pool.shutdown();
            let mut state = lifecycle.lock().expect("lifecycle lock poisoned");
            *state = if outcome.is_ok() {
                Lifecycle::Closed
            } else {
                Lifecycle::ShutdownFailed
            };
            drop(state);
            while pending
                .front()
                .is_some_and(|event| matches!(event, PendingEvent::Close(_)))
            {
                let Some(PendingEvent::Close(close)) = pending.pop_front() else {
                    unreachable!("the pending event kind was inspected above");
                };
                let result = if outcome.is_ok() {
                    Ok(())
                } else {
                    Err(AtomicBlobStoreError::ShutdownFailure)
                };
                let _ = close.sender.send(result);
            }
            while let Ok(event) = receiver.try_recv() {
                match event {
                    CoordinatorEvent::Close(close) => {
                        let result = if outcome.is_ok() {
                            Ok(())
                        } else {
                            Err(AtomicBlobStoreError::ShutdownFailure)
                        };
                        let _ = close.sender.send(result);
                    }
                    CoordinatorEvent::Submission(submission) => {
                        deliver_error(submission.operation, AtomicBlobStoreError::StoreClosed);
                    }
                    CoordinatorEvent::Maintenance(submission) => {
                        let _ = submission
                            .sender
                            .send(Err(AtomicBlobStoreError::StoreClosed));
                    }
                    CoordinatorEvent::Flush(sender) => {
                        let _ = sender.send(Err(AtomicBlobStoreError::StoreClosed));
                    }
                    CoordinatorEvent::Completion(_)
                    | CoordinatorEvent::MaintenanceCompletion(_) => {}
                }
            }
            #[cfg(all(test, any(unix, windows)))]
            if let Some(hook) = &config.hook {
                hook(TestStage::CoordinatorStopping)
                    .expect("test-requested coordinator stopping failure");
            }
            return;
        }
    }
    let _ = worker_pool.shutdown();
    let mut state = lifecycle.lock().expect("lifecycle lock poisoned");
    if !matches!(*state, Lifecycle::Closed) {
        *state = Lifecycle::Closed;
    }
}

#[cfg(any(unix, windows))]
pub(crate) fn advance_pending_if_ready(
    config: &Arc<StoreConfig>,
    queues: &mut HashMap<[u8; 32], VecDeque<QueuedOperation>>,
    active: &mut HashSet<[u8; 32]>,
    pending: &mut VecDeque<PendingEvent>,
    maintenance_active: &mut bool,
    worker_pool: &mut WorkerPool,
) {
    if *maintenance_active {
        return;
    }

    loop {
        match pending.front() {
            Some(PendingEvent::Maintenance(_)) => {
                if !active.is_empty() || queues.values().any(|queue| !queue.is_empty()) {
                    return;
                }
                let Some(PendingEvent::Maintenance(submission)) = pending.pop_front() else {
                    unreachable!("the pending event kind was inspected above");
                };
                *maintenance_active = true;
                dispatch_maintenance(config, submission, worker_pool);
                return;
            }
            Some(PendingEvent::Submission(_)) => {
                let Some(PendingEvent::Submission(submission)) = pending.pop_front() else {
                    unreachable!("the pending event kind was inspected above");
                };
                let key_hash = submission.key_hash;
                queues
                    .entry(key_hash)
                    .or_default()
                    .push_back(QueuedOperation {
                        operation: submission.operation,
                        completion_sender: submission.completion_sender,
                    });
                dispatch_if_idle(key_hash, config, queues, active, worker_pool);
            }
            Some(PendingEvent::Flush(_)) => {
                if !active.is_empty() || queues.values().any(|queue| !queue.is_empty()) {
                    return;
                }
                let Some(PendingEvent::Flush(sender)) = pending.pop_front() else {
                    unreachable!("the pending event kind was inspected above");
                };
                #[cfg(all(test, any(unix, windows)))]
                if let Some(hook) = &config.hook {
                    hook(TestStage::FlushCompleted)
                        .expect("test-requested flush completion failure");
                }
                let _ = sender.send(Ok(()));
            }
            Some(PendingEvent::Close(_)) => {
                return;
            }
            None => return,
        }
    }
}

#[cfg(any(unix, windows))]
pub(crate) fn dispatch_maintenance(
    config: &Arc<StoreConfig>,
    submission: MaintenanceSubmission,
    worker_pool: &mut WorkerPool,
) {
    if let Err(error) = worker_pool.prepare(1) {
        let _ = submission.sender.send(Err(error));
        let _ = submission
            .completion_sender
            .send(CoordinatorEvent::MaintenanceCompletion(
                MaintenanceCompletion { outcome: None },
            ));
        return;
    }
    if let Err(error) = worker_pool.prepare_dispatch() {
        let _ = submission.sender.send(Err(error));
        let _ = submission
            .completion_sender
            .send(CoordinatorEvent::MaintenanceCompletion(
                MaintenanceCompletion { outcome: None },
            ));
        return;
    }
    let config = Arc::clone(config);
    let fallback_sender = submission.completion_sender.clone();
    if worker_pool
        .execute(Box::new(move || {
            let MaintenanceSubmission {
                minimum_age,
                sender,
                completion_sender,
            } = submission;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                #[cfg(all(test, any(unix, windows)))]
                hit_test_stage(
                    &config,
                    TestStage::MaintenanceStarted,
                    StoreOperation::EnumerateTemporaryFiles,
                )?;
                minimum_age.map_or_else(
                    || Ok(CleanupReport::default()),
                    |minimum_age| cleanup_stale_files(&config, minimum_age),
                )
            }));
            let outcome = result.ok().map(|result| (sender, result));
            let _send_result = completion_sender.send(CoordinatorEvent::MaintenanceCompletion(
                MaintenanceCompletion { outcome },
            ));
        }))
        .is_err()
    {
        let _ = fallback_sender.send(CoordinatorEvent::MaintenanceCompletion(
            MaintenanceCompletion { outcome: None },
        ));
    }
}

#[cfg(any(unix, windows))]
pub(crate) fn dispatch_if_idle(
    key_hash: [u8; 32],
    config: &Arc<StoreConfig>,
    queues: &mut HashMap<[u8; 32], VecDeque<QueuedOperation>>,
    active: &mut HashSet<[u8; 32]>,
    worker_pool: &mut WorkerPool,
) {
    if active.contains(&key_hash) || active.len() >= config.max_concurrent_operations {
        return;
    }

    let Some(next_operation) = queues.get_mut(&key_hash).and_then(VecDeque::pop_front) else {
        return;
    };
    let QueuedOperation {
        operation,
        completion_sender,
    } = next_operation;
    if let Err(error) = worker_pool.prepare(active.len() + 1) {
        deliver_error(operation, error);
        if queues.get(&key_hash).is_some_and(VecDeque::is_empty) {
            queues.remove(&key_hash);
        }
        return;
    }
    if let Err(error) = worker_pool.prepare_dispatch() {
        deliver_error(operation, error);
        if queues.get(&key_hash).is_some_and(VecDeque::is_empty) {
            queues.remove(&key_hash);
        }
        return;
    }
    let config = Arc::clone(config);
    active.insert(key_hash);
    let fallback_sender = completion_sender.clone();
    if worker_pool
        .execute(Box::new(move || {
            let path = config.namespace.join(format!(
                "{}{}",
                blake3::Hash::from_bytes(key_hash).to_hex(),
                config.format.filename_suffix()
            ));
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                #[cfg(all(test, any(unix, windows)))]
                hit_test_stage(
                    &config,
                    TestStage::OperationStarted,
                    StoreOperation::ReadEnvelope,
                )?;
                Ok::<_, AtomicBlobStoreError>(run_owned_operation(&config, &path, operation))
            }))
            .ok()
            .and_then(Result::ok);
            let _send_result = completion_sender.send(CoordinatorEvent::Completion(Completion {
                key_hash,
                outcome,
            }));
        }))
        .is_err()
    {
        let _ = fallback_sender.send(CoordinatorEvent::Completion(Completion {
            key_hash,
            outcome: None,
        }));
    }
}

#[cfg(any(unix, windows))]
pub(crate) fn dispatch_available(
    config: &Arc<StoreConfig>,
    queues: &mut HashMap<[u8; 32], VecDeque<QueuedOperation>>,
    active: &mut HashSet<[u8; 32]>,
    worker_pool: &mut WorkerPool,
) {
    while active.len() < config.max_concurrent_operations {
        let Some(key_hash) = queues
            .iter()
            .find_map(|(key, queue)| (!queue.is_empty() && !active.contains(key)).then_some(*key))
        else {
            break;
        };
        dispatch_if_idle(key_hash, config, queues, active, worker_pool);
    }
}
