//! Managed request admission. Queue insertion and lifecycle changes share one
//! transaction across lanes; capacity waits never hold the admission lock.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// A request could not be admitted because its receiver is unavailable.
#[derive(Debug, thiserror::Error)]
#[error("admission receiver disconnected")]
pub struct SendError<T>(pub T);
impl<T> SendError<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}
#[derive(Debug, thiserror::Error)]
pub enum TrySendError<T> {
    #[error("admission queue full")]
    Full(T),
    #[error("admission receiver disconnected")]
    Disconnected(T),
}
impl<T> TrySendError<T> {
    pub fn into_inner(self) -> T {
        match self {
            Self::Full(item) | Self::Disconnected(item) => item,
        }
    }
}
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RecvError {
    #[error("admission senders disconnected")]
    Disconnected,
}
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TryRecvError {
    #[error("admission queue empty")]
    Empty,
    #[error("admission senders disconnected")]
    Disconnected,
}

pub trait Item {
    fn fence(&self) -> Option<Option<Duration>>;
    fn admitted(&mut self, sequence: u64, deadline: Option<Instant>);
    fn closing(&mut self);
    fn invalid_timeout(&mut self) {}
}

#[derive(Debug, Default)]
struct Lifecycle {
    sequence: u64,
    fence: Option<u64>,
    deadline: Option<Instant>,
    terminated: bool,
}

#[derive(Debug, Default)]
pub struct Gate {
    state: Mutex<Lifecycle>,
    changed: Notify,
    fenced: AtomicBool,
    terminated: AtomicBool,
    lanes: Mutex<Vec<Weak<SendWake>>>,
}

impl Gate {
    /// Permanently close every admission lane.
    ///
    /// This shares the lifecycle mutex with queue admission, so a sender either
    /// commits before termination or observes the closed gate.
    pub fn terminate(&self) {
        let mut state = self.state.lock().unwrap();
        if state.terminated {
            return;
        }
        state.terminated = true;
        self.terminated.store(true, Ordering::Release);
        drop(state);
        self.close_lanes();
    }
    pub fn clear_deadline(&self) {
        self.state.lock().unwrap().deadline = None;
        self.changed.notify_waiters();
    }
    pub async fn expired(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(deadline) = self.snapshot().1 {
                tokio::select! {
                    () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => return,
                    () = &mut changed => continue,
                }
            }
            changed.await;
        }
    }
    pub fn has_fence(&self) -> bool {
        self.fenced.load(Ordering::Acquire)
    }
    pub fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::Acquire)
    }
    fn close_lanes(&self) {
        self.changed.notify_waiters();
        // Do not run task wakers under the registry lock.
        let lanes: Vec<_> = self
            .lanes
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for lane in lanes {
            lane.wake();
        }
    }
    pub fn snapshot(&self) -> (Option<u64>, Option<Instant>) {
        if !self.has_fence() {
            return (None, None);
        }
        let state = self.state.lock().unwrap();
        (state.fence, state.deadline)
    }
}

#[derive(Debug)]
struct Queue<T> {
    items: VecDeque<T>,
    senders: usize,
    receivers: usize,
    waiting_receivers: usize,
}

/// Lane-local sender notifications. Blocking registration uses the same mutex
/// as wakeup, avoiding a lost wake between checking progress and sleeping.
#[derive(Debug, Default)]
struct SendWake {
    changed: Notify,
    progress: Mutex<u64>,
    blocking: Condvar,
    blocking_waiters: AtomicUsize,
}
impl SendWake {
    fn wake(&self) {
        // A registering sender retries admission before sleeping. If this
        // precedes registration, that retry observes the newly available slot.
        if self.blocking_waiters.load(Ordering::SeqCst) != 0 {
            let mut progress = self.progress.lock().unwrap();
            *progress = progress.wrapping_add(1);
            drop(progress);
            self.blocking.notify_all();
        }
        self.changed.notify_waiters();
    }
}

struct BlockingSender<'a>(&'a SendWake);
impl Drop for BlockingSender<'_> {
    fn drop(&mut self) {
        self.0.blocking_waiters.fetch_sub(1, Ordering::SeqCst);
    }
}
#[derive(Debug)]
struct Shared<T> {
    queue: Mutex<Queue<T>>,
    capacity: Option<usize>,
    priority: bool,
    gate: Arc<Gate>,
    readable: Notify,
    writable: Arc<SendWake>,
}

#[derive(Debug)]
pub struct Sender<T>(Arc<Shared<T>>);
#[derive(Debug)]
pub struct Receiver<T>(Arc<Shared<T>>);

pub fn channel<T>(
    capacity: Option<usize>,
    gate: Arc<Gate>,
    priority: bool,
) -> (Sender<T>, Receiver<T>) {
    let writable = Arc::new(SendWake::default());
    if !priority {
        let mut lanes = gate.lanes.lock().unwrap();
        lanes.retain(|lane| lane.strong_count() != 0);
        lanes.push(Arc::downgrade(&writable));
    }
    let shared = Arc::new(Shared {
        queue: Mutex::new(Queue {
            items: VecDeque::new(),
            senders: 1,
            receivers: 1,
            waiting_receivers: 0,
        }),
        capacity,
        priority,
        gate,
        readable: Notify::new(),
        writable,
    });
    (Sender(shared.clone()), Receiver(shared))
}

#[must_use]
pub fn bounded<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    channel(Some(capacity), Arc::default(), false)
}

#[must_use]
pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    channel(None, Arc::default(), false)
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.0.queue.lock().unwrap().senders += 1;
        Self(self.0.clone())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let last = {
            let mut queue = self.0.queue.lock().unwrap();
            queue.senders -= 1;
            queue.senders == 0
        };
        if last {
            self.0.readable.notify_waiters();
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let discarded = {
            let mut queue = self.0.queue.lock().unwrap();
            queue.receivers -= 1;
            std::mem::take(&mut queue.items)
        };
        self.0.writable.wake();
        drop(discarded);
    }
}

impl<T: Item> Sender<T> {
    #[must_use]
    pub fn is_closing(&self) -> bool {
        self.0.gate.has_fence() && !self.0.priority
    }
    pub fn try_send(&self, mut item: T) -> Result<(), TrySendError<T>> {
        let mut lifecycle = self.0.gate.state.lock().unwrap();
        let mut queue = self.0.queue.lock().unwrap();
        if queue.receivers == 0 {
            return Err(TrySendError::Disconnected(item));
        }
        if lifecycle.terminated {
            return Err(TrySendError::Disconnected(item));
        }
        if lifecycle.fence.is_some() && !self.0.priority {
            item.closing();
            return Err(TrySendError::Disconnected(item));
        }
        let available = match self.0.capacity {
            None => true,
            Some(0) => queue.waiting_receivers > queue.items.len(),
            Some(capacity) => queue.items.len() < capacity,
        };
        if !available {
            return Err(TrySendError::Full(item));
        }
        let fence = item.fence();
        let deadline = fence
            .flatten()
            .and_then(|duration| Instant::now().checked_add(duration));
        if fence.flatten().is_some() && deadline.is_none() {
            item.invalid_timeout();
            return Err(TrySendError::Disconnected(item));
        }
        lifecycle.sequence = lifecycle
            .sequence
            .checked_add(1)
            .expect("admission sequence exhausted");
        let sequence = lifecycle.sequence;
        item.admitted(sequence, deadline);
        queue.items.push_back(item);
        if fence.is_some() {
            lifecycle.fence = Some(sequence);
            lifecycle.deadline = deadline;
            self.0.gate.fenced.store(true, Ordering::Release);
        }
        drop(queue);
        drop(lifecycle);
        self.0.readable.notify_waiters();
        if fence.is_some() {
            self.0.gate.close_lanes();
        }
        Ok(())
    }

    pub async fn send_async(&self, mut item: T) -> Result<(), SendError<T>> {
        match self.try_send(item) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Disconnected(value)) => return Err(SendError(value)),
            Err(TrySendError::Full(value)) => item = value,
        }
        loop {
            let changed = self.0.writable.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match self.try_send(item) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(value)) => return Err(SendError(value)),
                Err(TrySendError::Full(value)) => item = value,
            }
            changed.await;
        }
    }

    pub fn send(&self, mut item: T) -> Result<(), SendError<T>> {
        match self.try_send(item) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Disconnected(value)) => return Err(SendError(value)),
            Err(TrySendError::Full(value)) => item = value,
        }
        self.0
            .writable
            .blocking_waiters
            .fetch_add(1, Ordering::SeqCst);
        let _registration = BlockingSender(&self.0.writable);
        loop {
            let version = *self.0.writable.progress.lock().unwrap();
            match self.try_send(item) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(value)) => return Err(SendError(value)),
                Err(TrySendError::Full(value)) => item = value,
            }
            let progress = self.0.writable.progress.lock().unwrap();
            let _progress = self
                .0
                .writable
                .blocking
                .wait_while(progress, |current| *current == version)
                .unwrap();
        }
    }

    #[must_use]
    pub fn capacity(&self) -> Option<usize> {
        self.0.capacity
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.queue.lock().unwrap().items.len()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[must_use]
    pub fn is_disconnected(&self) -> bool {
        self.0.queue.lock().unwrap().receivers == 0
    }
}

struct WaitingReceiver<'a, T> {
    shared: &'a Shared<T>,
    active: bool,
}
impl<T> Drop for WaitingReceiver<'_, T> {
    fn drop(&mut self) {
        if self.active {
            self.shared.queue.lock().unwrap().waiting_receivers -= 1;
        }
    }
}

impl<T> Receiver<T> {
    #[must_use]
    pub fn gate(&self) -> &Arc<Gate> {
        &self.0.gate
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.queue.lock().unwrap().items.len()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[must_use]
    pub fn is_disconnected(&self) -> bool {
        self.0.queue.lock().unwrap().senders == 0
    }
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let mut queue = self.0.queue.lock().unwrap();
        let result = queue.items.pop_front().ok_or(if queue.senders == 0 {
            TryRecvError::Disconnected
        } else {
            TryRecvError::Empty
        });
        drop(queue);
        if result.is_ok() && self.0.capacity.is_some() {
            self.0.writable.wake();
        }
        result
    }
    #[must_use]
    pub fn drain(&self) -> std::collections::vec_deque::IntoIter<T> {
        let items = std::mem::take(&mut self.0.queue.lock().unwrap().items);
        if !items.is_empty() && self.0.capacity.is_some() {
            self.0.writable.wake();
        }
        items.into_iter()
    }
    pub async fn recv_async(&self) -> Result<T, RecvError> {
        match self.try_recv() {
            Ok(item) => return Ok(item),
            Err(TryRecvError::Disconnected) => return Err(RecvError::Disconnected),
            Err(TryRecvError::Empty) => {}
        }
        let rendezvous = self.0.capacity == Some(0);
        if rendezvous {
            self.0.queue.lock().unwrap().waiting_receivers += 1;
        }
        let mut waiting = WaitingReceiver {
            shared: &self.0,
            active: rendezvous,
        };
        if rendezvous {
            self.0.writable.wake();
        }
        loop {
            let changed = self.0.readable.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut queue = self.0.queue.lock().unwrap();
                if let Some(item) = queue.items.pop_front() {
                    // Retire this rendezvous slot before releasing the queue
                    // lock, so a second send cannot use the completed receive.
                    if waiting.active {
                        queue.waiting_receivers -= 1;
                        waiting.active = false;
                    }
                    drop(queue);
                    if self.0.capacity.is_some() {
                        self.0.writable.wake();
                    }
                    return Ok(item);
                }
                if queue.senders == 0 {
                    return Err(RecvError::Disconnected);
                }
            }
            changed.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    #[derive(Default)]
    struct WakeCount(AtomicUsize);
    impl std::task::Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn queue_activity_does_not_wake_lifecycle_or_other_lanes() {
        let gate = Arc::default();
        let (normal, rx) = channel(Some(2), Arc::clone(&gate), false);
        let (control, _control_rx) = channel(Some(1), Arc::clone(&gate), false);
        control.try_send(Message::publish()).unwrap();
        let lifecycle_wakes = Arc::new(WakeCount::default());
        let lifecycle_waker = Waker::from(Arc::clone(&lifecycle_wakes));
        let mut changed = Box::pin(gate.changed.notified());
        assert!(
            changed
                .as_mut()
                .poll(&mut Context::from_waker(&lifecycle_waker))
                .is_pending()
        );
        let lane_wakes = Arc::new(WakeCount::default());
        let lane_waker = Waker::from(Arc::clone(&lane_wakes));
        let mut blocked = Box::pin(control.send_async(Message::publish()));
        assert!(
            blocked
                .as_mut()
                .poll(&mut Context::from_waker(&lane_waker))
                .is_pending()
        );
        normal.try_send(Message::publish()).unwrap();
        rx.try_recv().unwrap();
        drop(normal.clone());
        assert_eq!(lifecycle_wakes.0.load(Ordering::Relaxed), 0);
        assert_eq!(lane_wakes.0.load(Ordering::Relaxed), 0);
        normal.try_send(Message::fence()).unwrap();
        assert!(lifecycle_wakes.0.load(Ordering::Relaxed) > 0);
        assert!(lane_wakes.0.load(Ordering::Relaxed) > 0);
        assert!(matches!(
            poll(blocked.as_mut()),
            Poll::Ready(Err(SendError(Message { closing: true, .. })))
        ));
    }

    #[test]
    fn fast_paths_do_not_register_rendezvous_or_blocking_waiters() {
        let (tx, rx) = bounded(1);
        let mut send = Box::pin(tx.send_async(Message::publish()));
        assert!(poll(send.as_mut()).is_ready());
        let mut recv = Box::pin(rx.recv_async());
        assert!(poll(recv.as_mut()).is_ready());
        tx.send(Message::publish()).unwrap();
        assert_eq!(rx.0.queue.lock().unwrap().waiting_receivers, 0);
        assert_eq!(tx.0.writable.blocking_waiters.load(Ordering::SeqCst), 0);
        assert_eq!(*tx.0.writable.progress.lock().unwrap(), 0);
        // Even a pending buffered receive must not advertise rendezvous capacity.
        rx.try_recv().unwrap();
        let mut recv = Box::pin(rx.recv_async());
        assert!(poll(recv.as_mut()).is_pending());
        assert_eq!(rx.0.queue.lock().unwrap().waiting_receivers, 0);
    }

    #[test]
    fn registered_waiters_retry_after_capacity_and_data_become_available() {
        let (tx, rx) = bounded(1);
        tx.try_send(Message::publish()).unwrap();
        let mut send = Box::pin(tx.send_async(Message::publish()));
        assert!(poll(send.as_mut()).is_pending());
        rx.try_recv().unwrap();
        assert!(poll(send.as_mut()).is_ready());
        rx.try_recv().unwrap();
        let mut recv = Box::pin(rx.recv_async());
        assert!(poll(recv.as_mut()).is_pending());
        tx.try_send(Message::publish()).unwrap();
        assert!(poll(recv.as_mut()).is_ready());
    }

    #[test]
    fn fence_wakes_registered_blocking_sender_on_other_lane() {
        let gate = Arc::default();
        let (normal, _rx) = channel(Some(1), Arc::clone(&gate), false);
        let (control, _control_rx) = channel(Some(1), gate, false);
        control.try_send(Message::publish()).unwrap();
        let wake = Arc::clone(&control.0.writable);
        let (done, result) = std::sync::mpsc::channel();
        let worker =
            std::thread::spawn(move || done.send(control.send(Message::publish())).unwrap());
        let limit = Instant::now() + Duration::from_secs(2);
        while wake.blocking_waiters.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < limit, "sender did not register");
            std::thread::yield_now();
        }
        normal.try_send(Message::fence()).unwrap();
        assert!(
            result
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap_err()
                .0
                .closing
        );
        worker.join().unwrap();
    }

    #[test]
    fn capacity_and_receiver_loss_wake_registered_blocking_senders() {
        for disconnect in [false, true] {
            let (tx, rx) = bounded(1);
            tx.try_send(Message::publish()).unwrap();
            let wake = Arc::clone(&tx.0.writable);
            let (done, result) = std::sync::mpsc::channel();
            let worker =
                std::thread::spawn(move || done.send(tx.send(Message::publish())).unwrap());
            let limit = Instant::now() + Duration::from_secs(2);
            while wake.blocking_waiters.load(Ordering::SeqCst) == 0 {
                assert!(Instant::now() < limit, "sender did not register");
                std::thread::yield_now();
            }
            if disconnect {
                drop(rx);
            } else {
                rx.try_recv().unwrap();
            }
            assert_eq!(
                result
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .is_err(),
                disconnect
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn only_final_sender_loss_wakes_an_empty_receiver() {
        let (tx, rx) = bounded::<Message>(1);
        let other = tx.clone();
        let wakes = Arc::new(WakeCount::default());
        let waker = Waker::from(Arc::clone(&wakes));
        let mut receive = Box::pin(rx.recv_async());
        assert!(
            receive
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        drop(other);
        assert_eq!(wakes.0.load(Ordering::Relaxed), 0);
        drop(tx);
        assert!(wakes.0.load(Ordering::Relaxed) > 0);
        assert!(matches!(
            poll(receive.as_mut()),
            Poll::Ready(Err(RecvError::Disconnected))
        ));
    }

    #[tokio::test]
    async fn active_deadline_waiter_observes_new_fence_and_deadline_clear() {
        let (tx, rx) = bounded(1);
        let mut expired = Box::pin(rx.gate().expired());
        assert!(poll(expired.as_mut()).is_pending());
        tx.try_send(Message {
            timeout: Some(Duration::ZERO),
            ..Message::fence()
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), &mut expired)
            .await
            .unwrap();
        rx.gate().clear_deadline();
        let mut expired = Box::pin(rx.gate().expired());
        assert!(poll(expired.as_mut()).is_pending());
    }

    #[derive(Debug)]
    struct Message {
        fence: bool,
        timeout: Option<Duration>,
        sequence: u64,
        closing: bool,
    }
    impl Message {
        fn publish() -> Self {
            Self {
                fence: false,
                timeout: None,
                sequence: 0,
                closing: false,
            }
        }
        fn fence() -> Self {
            Self {
                fence: true,
                ..Self::publish()
            }
        }
    }
    impl Item for Message {
        fn fence(&self) -> Option<Option<Duration>> {
            self.fence.then_some(self.timeout)
        }
        fn admitted(&mut self, sequence: u64, _: Option<Instant>) {
            self.sequence = sequence;
        }
        fn closing(&mut self) {
            self.closing = true;
        }
    }
    fn poll<F: Future>(future: std::pin::Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn failed_try_fence_leaves_admission_open() {
        let (tx, rx) = bounded(1);
        tx.try_send(Message::publish()).unwrap();
        assert!(matches!(
            tx.try_send(Message::fence()),
            Err(TrySendError::Full(_))
        ));
        assert_eq!(rx.gate().snapshot().0, None);
        rx.try_recv().unwrap();
        tx.try_send(Message::publish()).unwrap();
    }

    #[tokio::test]
    async fn deadline_starts_after_capacity_wait() {
        let (tx, rx) = bounded(1);
        tx.try_send(Message::publish()).unwrap();
        let timeout = Duration::from_millis(10);
        let mut send = Box::pin(tx.send_async(Message {
            timeout: Some(timeout),
            ..Message::fence()
        }));
        assert!(poll(send.as_mut()).is_pending());
        tokio::time::sleep(timeout * 2).await;
        assert_eq!(rx.gate().snapshot(), (None, None));
        rx.try_recv().unwrap();
        let before_admission = Instant::now();
        send.await.unwrap();
        assert!(rx.gate().snapshot().1.unwrap() >= before_admission + timeout);
    }

    #[test]
    fn completed_rendezvous_cannot_be_reused() {
        let (tx, rx) = bounded(0);
        let mut receive = Box::pin(rx.recv_async());
        assert!(poll(receive.as_mut()).is_pending());
        tx.try_send(Message::publish()).unwrap();
        assert!(poll(receive.as_mut()).is_ready());
        assert!(matches!(
            tx.try_send(Message::fence()),
            Err(TrySendError::Full(_))
        ));
        assert_eq!(rx.gate().snapshot().0, None);
    }

    #[test]
    fn cancelled_capacity_wait_does_not_install_fence() {
        let (tx, rx) = bounded(1);
        tx.try_send(Message::publish()).unwrap();
        let mut send = Box::pin(tx.send_async(Message::fence()));
        assert!(poll(send.as_mut()).is_pending());
        drop(send);
        assert_eq!(rx.gate().snapshot().0, None);
        rx.try_recv().unwrap();
        tx.try_send(Message::publish()).unwrap();
    }

    #[test]
    fn fence_closes_both_lanes_and_first_admission_wins() {
        let gate = Arc::default();
        let (normal, rx) = channel(Some(2), Arc::clone(&gate), false);
        let (control, _control_rx) = channel(Some(2), Arc::clone(&gate), false);
        let (immediate, _immediate_rx) = channel(None, gate, true);
        normal.try_send(Message::publish()).unwrap();
        normal.try_send(Message::fence()).unwrap();
        for tx in [&normal, &control] {
            assert!(matches!(
                tx.try_send(Message::publish()),
                Err(TrySendError::Disconnected(Message { closing: true, .. }))
            ));
        }
        assert!(normal.try_send(Message::fence()).is_err());
        assert_eq!(rx.try_recv().unwrap().sequence, 1);
        assert_eq!(rx.try_recv().unwrap().sequence, 2);
        immediate.try_send(Message::publish()).unwrap();
    }

    #[test]
    fn termination_closes_every_lane() {
        let gate = Arc::default();
        let (normal, _rx) = channel(Some(1), Arc::clone(&gate), false);
        let (control, _control_rx) = channel(Some(1), Arc::clone(&gate), false);
        let (immediate, _immediate_rx) = channel(None, Arc::clone(&gate), true);

        gate.terminate();

        assert!(gate.is_terminated());
        for sender in [&normal, &control, &immediate] {
            assert!(matches!(
                sender.try_send(Message::publish()),
                Err(TrySendError::Disconnected(Message { closing: false, .. }))
            ));
        }
    }

    #[test]
    fn termination_wakes_capacity_waiters() {
        let (tx, _rx) = bounded(1);
        tx.try_send(Message::publish()).unwrap();
        let mut send = Box::pin(tx.send_async(Message::publish()));
        assert!(poll(send.as_mut()).is_pending());

        tx.0.gate.terminate();

        assert!(matches!(
            poll(send.as_mut()),
            Poll::Ready(Err(SendError(Message { closing: false, .. })))
        ));
    }

    #[test]
    fn zero_capacity_requires_a_live_receiver_wait() {
        let (tx, rx) = bounded(0);
        assert!(matches!(
            tx.try_send(Message::fence()),
            Err(TrySendError::Full(_))
        ));
        let mut receive = Box::pin(rx.recv_async());
        assert!(poll(receive.as_mut()).is_pending());
        drop(receive);
        assert!(matches!(
            tx.try_send(Message::fence()),
            Err(TrySendError::Full(_))
        ));
        let mut receive = Box::pin(rx.recv_async());
        assert!(poll(receive.as_mut()).is_pending());
        tx.try_send(Message::fence()).unwrap();
        assert!(matches!(
            poll(receive.as_mut()),
            Poll::Ready(Ok(Message { fence: true, .. }))
        ));
        assert_eq!(rx.gate().snapshot().0, Some(1));
    }

    #[tokio::test]
    async fn fence_wakes_a_sender_waiting_on_another_full_lane() {
        let gate = Arc::default();
        let (normal, _rx) = channel(Some(1), Arc::clone(&gate), false);
        let (control, _control_rx) = channel(Some(1), gate, false);
        control.try_send(Message::publish()).unwrap();
        let mut send = Box::pin(control.send_async(Message::publish()));
        assert!(poll(send.as_mut()).is_pending());
        normal.try_send(Message::fence()).unwrap();
        assert!(send.await.unwrap_err().0.closing);
    }

    #[test]
    fn blocking_rendezvous_and_receiver_loss_wake_senders() {
        let (tx, rx) = bounded(0);
        let worker = std::thread::spawn(move || tx.send(Message::publish()));
        drop(rx);
        assert!(worker.join().unwrap().is_err());
    }
}
