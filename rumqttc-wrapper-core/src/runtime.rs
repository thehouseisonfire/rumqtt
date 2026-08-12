use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use flume::Receiver;
use parking_lot::Mutex;

use crate::{Error, ErrorKind, Result};

/// Join ownership shared by the native owner and host-neutral close coordinator.
pub(crate) struct ThreadOwner {
    pub(crate) join: Mutex<Option<JoinHandle<()>>>,
    pub(crate) done: Receiver<()>,
}

impl ThreadOwner {
    pub(crate) fn join(&self, timeout: Duration) -> Result<()> {
        let started = Instant::now();
        match self.done.recv_timeout(timeout) {
            Ok(()) | Err(flume::RecvTimeoutError::Disconnected) => {}
            Err(flume::RecvTimeoutError::Timeout) => {
                return Err(Error::new(
                    ErrorKind::Timeout,
                    "driver did not terminate before join timeout",
                ));
            }
        }
        let join = self
            .join
            .try_lock_for(timeout.saturating_sub(started.elapsed()))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Timeout,
                    "driver join coordination did not complete before timeout",
                )
            })?
            .take();
        if let Some(join) = join {
            join.join()
                .map_err(|_| Error::new(ErrorKind::Internal, "driver thread panicked"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn join_coordination_honors_the_shared_timeout_budget() {
        let (done_tx, done) = flume::bounded(1);
        drop(done_tx);
        let owner = Arc::new(ThreadOwner {
            join: Mutex::new(None),
            done,
        });
        let join_guard = owner.join.lock();
        let waiter = Arc::clone(&owner);
        let started = Instant::now();
        let result = thread::spawn(move || waiter.join(Duration::from_millis(25)))
            .join()
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(result.unwrap_err().kind(), ErrorKind::Timeout);
        assert!(elapsed < Duration::from_millis(300), "elapsed: {elapsed:?}");
        drop(join_guard);
        owner.join(Duration::from_secs(1)).unwrap();
    }
}
