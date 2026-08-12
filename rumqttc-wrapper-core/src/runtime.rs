use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use flume::Receiver;

use crate::{Error, ErrorKind, Result};

/// Join ownership shared by the native owner and host-neutral close coordinator.
pub(crate) struct ThreadOwner {
    pub(crate) join: Mutex<Option<JoinHandle<()>>>,
    pub(crate) done: Receiver<()>,
}

impl ThreadOwner {
    pub(crate) fn join(&self, timeout: Duration) -> Result<()> {
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
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "join mutex poisoned"))?
            .take();
        if let Some(join) = join {
            join.join()
                .map_err(|_| Error::new(ErrorKind::Internal, "driver thread panicked"))?;
        }
        Ok(())
    }
}
