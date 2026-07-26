use super::*;
#[cfg(any(unix, windows))]
pub(crate) type WorkerJob = Box<dyn FnOnce() + Send + 'static>;

#[cfg(any(unix, windows))]
pub(crate) struct WorkerPool {
    sender: Option<Sender<WorkerJob>>,
    receiver: Receiver<WorkerJob>,
    handles: Vec<std::thread::JoinHandle<()>>,
    capacity: usize,
    #[cfg(all(test, any(unix, windows)))]
    hook: Option<Arc<dyn Fn(TestStage) -> io::Result<()> + Send + Sync>>,
}

#[cfg(any(unix, windows))]
impl WorkerPool {
    pub(crate) fn new(
        capacity: usize,
        #[cfg(all(test, any(unix, windows)))] hook: Option<
            Arc<dyn Fn(TestStage) -> io::Result<()> + Send + Sync>,
        >,
    ) -> Result<Self, AtomicBlobStoreError> {
        let (sender, receiver) = flume::unbounded::<WorkerJob>();
        Ok(Self {
            sender: Some(sender),
            receiver,
            handles: Vec::with_capacity(capacity),
            capacity,
            #[cfg(all(test, any(unix, windows)))]
            hook,
        })
    }

    pub(crate) fn prepare(&mut self, required_workers: usize) -> Result<(), AtomicBlobStoreError> {
        while self.handles.len() < required_workers.min(self.capacity) {
            #[cfg(all(test, any(unix, windows)))]
            if self
                .hook
                .as_ref()
                .is_some_and(|hook| hook(TestStage::WorkerStart).is_err())
            {
                return Err(AtomicBlobStoreError::WorkerUnavailable);
            }
            let receiver = self.receiver.clone();
            let index = self.handles.len();
            #[cfg(all(test, any(unix, windows)))]
            let hook = self.hook.clone();
            let handle = std::thread::Builder::new()
                .name(format!("atomic-blob-store-worker-{index}"))
                .spawn(move || {
                    #[cfg(all(test, any(unix, windows)))]
                    struct WorkerExitNotification(
                        Option<Arc<dyn Fn(TestStage) -> io::Result<()> + Send + Sync>>,
                    );
                    #[cfg(all(test, any(unix, windows)))]
                    impl Drop for WorkerExitNotification {
                        fn drop(&mut self) {
                            if let Some(hook) = &self.0 {
                                let _ = hook(TestStage::WorkerStopped);
                            }
                        }
                    }
                    #[cfg(all(test, any(unix, windows)))]
                    let _exit_notification = WorkerExitNotification(hook.clone());
                    while let Ok(job) = receiver.recv() {
                        job();
                    }
                    #[cfg(all(test, any(unix, windows)))]
                    if let Some(hook) = hook {
                        hook(TestStage::WorkerExit).expect("test-requested worker exit panic");
                    }
                })
                .map_err(|_| AtomicBlobStoreError::WorkerUnavailable)?;
            self.handles.push(handle);
        }
        Ok(())
    }

    pub(crate) fn execute(&self, job: WorkerJob) -> Result<(), AtomicBlobStoreError> {
        self.sender
            .as_ref()
            .ok_or(AtomicBlobStoreError::WorkerUnavailable)?
            .send(job)
            .map_err(|_| AtomicBlobStoreError::WorkerUnavailable)
    }

    pub(crate) fn prepare_dispatch(&self) -> Result<(), AtomicBlobStoreError> {
        #[cfg(all(test, any(unix, windows)))]
        if self
            .hook
            .as_ref()
            .is_some_and(|hook| hook(TestStage::WorkerDispatch).is_err())
        {
            return Err(AtomicBlobStoreError::WorkerUnavailable);
        }
        Ok(())
    }

    pub(crate) fn shutdown(&mut self) -> Result<(), AtomicBlobStoreError> {
        self.sender.take();
        let mut panicked = false;
        for handle in self.handles.drain(..) {
            panicked |= handle.join().is_err();
        }
        if panicked {
            Err(AtomicBlobStoreError::ShutdownFailure)
        } else {
            Ok(())
        }
    }
}

#[cfg(any(unix, windows))]
impl Drop for WorkerPool {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
