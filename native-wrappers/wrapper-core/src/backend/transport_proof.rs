//! Actual C callbacks linked statically into unit tests. This file lives inside
//! the backend boundary because it exercises both native protocol clients.
//! None of these types or function pointers are a proposed public ABI.

use std::ffi::c_void;
use std::io;
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::oneshot;

use super::{CHUNK, IoFuture, OwnedIo, OwnedStream};

const READ: u32 = 0;
const WRITE: u32 = 1;
const FLUSH: u32 = 2;
const SHUTDOWN: u32 = 3;
const CONNECT: u32 = 4;
const ACCEPTED: u32 = 0;
const INVALID_STATE: u32 = 1;

type BytesDone = unsafe extern "C" fn(*mut c_void, *const u8, usize, u32) -> u32;
type CountDone = unsafe extern "C" fn(*mut c_void, usize, u32) -> u32;
type UnitDone = unsafe extern "C" fn(*mut c_void, u32) -> u32;
type Release = unsafe extern "C" fn(*mut c_void);

unsafe extern "C" {
    fn proof_new(chunk: usize, deferred: u32) -> *mut c_void;
    fn proof_destroy(fixture: *mut c_void);
    fn proof_read(
        fixture: *mut c_void,
        token: *mut c_void,
        max: usize,
        done: BytesDone,
        release: Release,
    );
    fn proof_write(
        fixture: *mut c_void,
        token: *mut c_void,
        data: *const u8,
        length: usize,
        done: CountDone,
        release: Release,
    );
    fn proof_unit(
        fixture: *mut c_void,
        kind: u32,
        token: *mut c_void,
        done: UnitDone,
        release: Release,
    );
    fn proof_feed(fixture: *mut c_void, data: *const u8, length: usize);
    fn proof_eof(fixture: *mut c_void);
    fn proof_finish(fixture: *mut c_void, kind: u32, result: u32, count: usize, duplicate: i32);
    fn proof_before_read_completion(fixture: *mut c_void, hook: Release, context: *mut c_void);
    fn proof_release_pending(fixture: *mut c_void);
    fn proof_pending(fixture: *mut c_void, kind: u32) -> u32;
    fn proof_starts(fixture: *mut c_void, kind: u32) -> u32;
    fn proof_status(fixture: *mut c_void, first: i32) -> u32;
    fn proof_max_transfer(fixture: *mut c_void, read: i32) -> u32;
    fn proof_outgoing(fixture: *mut c_void, output: *mut u8, capacity: usize) -> usize;
}

#[derive(Default)]
struct Counts {
    streams: AtomicUsize,
    registrations: AtomicUsize,
    operations: AtomicUsize,
}

struct Registration(Arc<Counts>);

impl Drop for Registration {
    fn drop(&mut self) {
        self.0.registrations.fetch_add(1, Ordering::SeqCst);
    }
}

struct Fixture {
    ptr: NonNull<c_void>,
    registration: Arc<Registration>,
}

// SAFETY: C serializes every access to its stream using an atomic lock. Each
// call holds a Fixture owner, and retained operation tokens also own Fixture.
unsafe impl Send for Fixture {}
unsafe impl Sync for Fixture {}

impl Fixture {
    fn new(chunk: usize, deferred: u32, registration: Arc<Registration>) -> Arc<Self> {
        let ptr = NonNull::new(unsafe { proof_new(chunk, deferred) }).expect("allocate C fixture");
        Arc::new(Self { ptr, registration })
    }

    fn feed(&self, data: &[u8]) {
        unsafe { proof_feed(self.ptr.as_ptr(), data.as_ptr(), data.len()) };
    }

    fn eof(&self) {
        unsafe { proof_eof(self.ptr.as_ptr()) };
    }

    fn finish(&self, kind: u32) {
        self.finish_with(kind, 0, usize::MAX, false);
    }

    fn finish_with(&self, kind: u32, result: u32, count: usize, duplicate: bool) {
        unsafe { proof_finish(self.ptr.as_ptr(), kind, result, count, i32::from(duplicate)) };
    }

    fn release_pending(&self) {
        unsafe { proof_release_pending(self.ptr.as_ptr()) };
    }

    fn pending(&self, kind: u32) -> bool {
        unsafe { proof_pending(self.ptr.as_ptr(), kind) != 0 }
    }

    fn starts(&self, kind: u32) -> u32 {
        unsafe { proof_starts(self.ptr.as_ptr(), kind) }
    }

    fn status(&self, first: bool) -> u32 {
        unsafe { proof_status(self.ptr.as_ptr(), i32::from(first)) }
    }

    fn outgoing(&self) -> Vec<u8> {
        let mut bytes = vec![0; 65536];
        let count = unsafe { proof_outgoing(self.ptr.as_ptr(), bytes.as_mut_ptr(), bytes.len()) };
        bytes.truncate(count);
        bytes
    }

    fn connect(self: &Arc<Self>) -> IoFuture<()> {
        self.unit(CONNECT)
    }

    fn unit(self: &Arc<Self>, kind: u32) -> IoFuture<()> {
        let (token, future) = operation(self.clone(), Bytes::new(), 0);
        unsafe { proof_unit(self.ptr.as_ptr(), kind, token, complete_unit, release::<()>) };
        future
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe { proof_destroy(self.ptr.as_ptr()) };
        self.registration.0.streams.fetch_add(1, Ordering::SeqCst);
    }
}

enum Completion<T> {
    Pending(oneshot::Sender<io::Result<T>>),
    Completed,
    Cancelled,
}

struct Operation<T> {
    state: Mutex<Completion<T>>,
    owner: Arc<Fixture>,
    input: Bytes,
    limit: usize,
}

impl<T> Operation<T> {
    fn finish_with(&self, result: impl FnOnce() -> io::Result<T>) -> u32 {
        let (sender, result) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(&*state, Completion::Pending(sender) if !sender.is_closed()) {
                return INVALID_STATE;
            }
            // A cancelled or duplicate completion is rejected before touching
            // its foreign byte views or allocating a copied result.
            let result = result();
            let Completion::Pending(sender) = std::mem::replace(&mut *state, Completion::Completed)
            else {
                unreachable!("state checked under lock")
            };
            drop(state);
            (sender, result)
        };
        if sender.send(result).is_ok() {
            ACCEPTED
        } else {
            INVALID_STATE
        }
    }

    fn cancel(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, Completion::Pending(_)) {
            *state = Completion::Cancelled;
        }
    }
}

impl<T> Drop for Operation<T> {
    fn drop(&mut self) {
        self.owner
            .registration
            .0
            .operations
            .fetch_add(1, Ordering::SeqCst);
    }
}

struct Cancel<T>(Arc<Operation<T>>);

impl<T> Drop for Cancel<T> {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn operation<T: Send + 'static>(
    owner: Arc<Fixture>,
    input: Bytes,
    limit: usize,
) -> (*mut c_void, IoFuture<T>) {
    let (sender, receiver) = oneshot::channel();
    let operation = Arc::new(Operation {
        state: Mutex::new(Completion::Pending(sender)),
        owner,
        input,
        limit,
    });
    // The C callback owns this token until it calls release, including after
    // cancellation. The guard is captured NOW so even an unpolled future cancels.
    let token = Box::into_raw(Box::new(operation.clone())).cast();
    let guard = Cancel(operation);
    let future = Box::pin(async move {
        let _guard = guard;
        receiver
            .await
            .unwrap_or_else(|_| Err(io::ErrorKind::BrokenPipe.into()))
    });
    (token, future)
}

unsafe extern "C" fn release<T>(token: *mut c_void) {
    // SAFETY: C releases each token once, after its final completion attempt.
    let operation = unsafe { Box::from_raw(token.cast::<Arc<Operation<T>>>()) };
    // Releasing unfinished host work must also wake its observer. Completed or
    // cancelled operations reject this implicit abandonment result.
    operation.finish_with(|| Err(io::ErrorKind::BrokenPipe.into()));
    drop(operation);
}

unsafe extern "C" fn complete_read(
    token: *mut c_void,
    data: *const u8,
    len: usize,
    result: u32,
) -> u32 {
    let operation = unsafe { &*token.cast::<Arc<Operation<Bytes>>>() };
    operation.finish_with(|| {
        if result != 0 {
            Err(io::ErrorKind::ConnectionReset.into())
        } else if len > operation.limit || (len > 0 && data.is_null()) {
            Err(io::ErrorKind::InvalidData.into())
        } else if len == 0 {
            Ok(Bytes::new())
        } else {
            // The C completion view is borrowed only for this call.
            Ok(Bytes::copy_from_slice(unsafe {
                std::slice::from_raw_parts(data, len)
            }))
        }
    })
}

unsafe extern "C" fn complete_count(token: *mut c_void, count: usize, result: u32) -> u32 {
    let operation = unsafe { &*token.cast::<Arc<Operation<usize>>>() };
    operation.finish_with(|| {
        if result != 0 {
            Err(io::ErrorKind::BrokenPipe.into())
        } else if count > operation.limit {
            Err(io::ErrorKind::InvalidData.into())
        } else {
            Ok(count)
        }
    })
}

unsafe extern "C" fn complete_unit(token: *mut c_void, result: u32) -> u32 {
    let operation = unsafe { &*token.cast::<Arc<Operation<()>>>() };
    operation.finish_with(|| {
        if result == 0 {
            Ok(())
        } else {
            Err(io::ErrorKind::ConnectionReset.into())
        }
    })
}

struct CStream(Arc<Fixture>);

impl OwnedIo for CStream {
    fn read(&self, max: usize) -> IoFuture<Bytes> {
        let (token, future) = operation(self.0.clone(), Bytes::new(), max);
        unsafe {
            proof_read(
                self.0.ptr.as_ptr(),
                token,
                max,
                complete_read,
                release::<Bytes>,
            );
        };
        future
    }

    fn write(&self, data: Bytes) -> IoFuture<usize> {
        let limit = data.len();
        let (token, future) = operation(self.0.clone(), data, limit);
        let operation = unsafe { &*token.cast::<Arc<Operation<usize>>>() };
        // C may retain the pointer only while retaining this operation token.
        unsafe {
            proof_write(
                self.0.ptr.as_ptr(),
                token,
                operation.input.as_ptr(),
                limit,
                complete_count,
                release::<usize>,
            );
        };
        future
    }

    fn flush(&self) -> IoFuture<()> {
        self.0.unit(FLUSH)
    }
    fn shutdown(&self) -> IoFuture<()> {
        self.0.unit(SHUTDOWN)
    }
}

fn stream(fixture: &Arc<Fixture>) -> OwnedStream {
    OwnedStream::new(Arc::new(CStream(fixture.clone())))
}

fn fixture(chunk: usize, deferred: u32) -> (Arc<Fixture>, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    let owner = Arc::new(Registration(counts.clone()));
    (Fixture::new(chunk, deferred, owner), counts)
}

#[derive(Default)]
struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn poll_read(
    io: &mut OwnedStream,
    cx: &mut Context<'_>,
    output: &mut [u8],
) -> Poll<io::Result<usize>> {
    let mut buf = ReadBuf::new(output);
    Pin::new(io)
        .poll_read(cx, &mut buf)
        .map(|result| result.map(|()| buf.filled().len()))
}

fn ready<T>(poll: Poll<io::Result<T>>) -> T {
    match poll {
        Poll::Ready(result) => result.unwrap(),
        Poll::Pending => panic!("unexpected pending I/O"),
    }
}

fn error<T>(poll: Poll<io::Result<T>>) -> io::ErrorKind {
    match poll {
        Poll::Ready(Err(error)) => error.kind(),
        _ => panic!("expected an I/O failure"),
    }
}

fn assert_released(counts: &Counts, streams: usize, registrations: usize) {
    assert_eq!(counts.streams.load(Ordering::SeqCst), streams);
    assert_eq!(counts.registrations.load(Ordering::SeqCst), registrations);
}

#[tokio::test]
async fn short_io_eof_and_shutdown_release_every_owner() {
    let (fixture, counts) = fixture(2, 0);
    fixture.feed(b"hello");
    let mut io = stream(&fixture);
    let mut input = [0; 5];
    io.read_exact(&mut input).await.unwrap();
    assert_eq!(&input, b"hello");
    io.write_all(b"world").await.unwrap();
    io.shutdown().await.unwrap();
    assert_eq!(fixture.outgoing(), b"world");
    assert_eq!(fixture.starts(WRITE), 3);
    assert_eq!(fixture.starts(FLUSH), 1);
    assert_eq!(fixture.starts(SHUTDOWN), 1);
    io.shutdown().await.unwrap();
    assert_eq!(fixture.starts(SHUTDOWN), 1);
    fixture.eof();
    assert_eq!(io.read(&mut input).await.unwrap(), 0);
    assert_eq!(io.read(&mut input).await.unwrap(), 0);
    assert_eq!(fixture.starts(READ), 4);
    drop(io);
    drop(fixture);
    assert_released(&counts, 1, 1);
    assert_eq!(counts.operations.load(Ordering::SeqCst), 9);
}

#[test]
fn pending_read_survives_a_changed_caller_buffer_and_copies_completion_bytes() {
    let (fixture, counts) = fixture(16, 0);
    let mut io = stream(&fixture);
    let notifications = Arc::new(Wakes::default());
    let waker = Waker::from(notifications.clone());
    let mut cx = Context::from_waker(&waker);
    let mut original = vec![0; 10];
    assert!(poll_read(&mut io, &mut cx, &mut original).is_pending());
    drop(original);
    let completion = fixture.clone();
    std::thread::spawn(move || completion.feed(b"0123456789"))
        .join()
        .unwrap();
    assert!(notifications.0.load(Ordering::SeqCst) > 0);
    // C's completion stack allocation has already disappeared.
    let mut replacement = [0; 3];
    assert_eq!(ready(poll_read(&mut io, &mut cx, &mut replacement)), 3);
    assert_eq!(&replacement, b"012");
    let mut rest = [0; 7];
    assert_eq!(ready(poll_read(&mut io, &mut cx, &mut rest)), 7);
    assert_eq!(&rest, b"3456789");
    assert_eq!(fixture.starts(READ), 1);
    drop(io);
    drop(fixture);
    assert_released(&counts, 1, 1);
}

struct ReadPause {
    reached: std::sync::mpsc::SyncSender<()>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
}

unsafe extern "C" fn pause_read_completion(context: *mut c_void) {
    // SAFETY: The worker retains an Arc<ReadPause> until the C call returns.
    let pause = unsafe { &*context.cast::<ReadPause>() };
    let _ = pause.reached.send(());
    // A failing test must not leave a worker blocked indefinitely.
    let _ = pause
        .resume
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .recv_timeout(Duration::from_secs(10));
}

fn assert_ready_read_cannot_complete_a_later_read(ready_on_feed: bool) {
    let (fixture, counts) = fixture(1, 0);
    let first = if ready_on_feed {
        Some(CStream(fixture.clone()).read(1))
    } else {
        fixture.feed(b"a");
        None
    };
    let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    let pause = Arc::new(ReadPause {
        reached: reached_tx,
        resume: Mutex::new(resume_rx),
    });
    unsafe {
        proof_before_read_completion(
            fixture.ptr.as_ptr(),
            pause_read_completion,
            Arc::as_ptr(&pause).cast_mut().cast(),
        );
    }
    let worker_fixture = fixture.clone();
    let worker = std::thread::spawn(move || {
        let _pause = pause;
        if ready_on_feed {
            worker_fixture.feed(b"a");
            None
        } else {
            Some(CStream(worker_fixture).read(1))
        }
    });
    reached_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    // Pause between the readiness decision and callback delivery. A competing
    // completion must find the original operation already detached, and later
    // reads must not consume the data captured for that original operation.
    let detached = !fixture.pending(READ);
    fixture.finish(READ);
    let mut next = stream(&fixture);
    let waker = Waker::from(Arc::new(Wakes::default()));
    let mut cx = Context::from_waker(&waker);
    let mut output = [0; 1];
    let initially_pending = poll_read(&mut next, &mut cx, &mut output).is_pending();
    fixture.feed(b"b");
    let second = poll_read(&mut next, &mut cx, &mut output);
    let second_byte = output[0];
    let third_pending = poll_read(&mut next, &mut cx, &mut output).is_pending();

    resume_tx.send(()).unwrap();
    let installed = worker.join().unwrap();
    let mut first = first.or(installed).unwrap();
    let first_bytes = ready(first.as_mut().poll(&mut cx));
    let still_pending = poll_read(&mut next, &mut cx, &mut output).is_pending();
    fixture.eof();
    let actual_eof = poll_read(&mut next, &mut cx, &mut output);

    assert!(
        detached,
        "read readiness must claim the operation under the lock"
    );
    assert!(
        initially_pending,
        "the first read's data must already be captured"
    );
    assert_eq!(&first_bytes[..], b"a");
    assert_eq!(ready(second), 1);
    assert_eq!(second_byte, b'b');
    assert!(third_pending);
    assert!(
        still_pending,
        "the delayed completion must not create false EOF"
    );
    assert_eq!(ready(actual_eof), 0);
    drop(first_bytes);
    drop(first);
    drop(next);
    drop(fixture);
    assert_released(&counts, 1, 1);
    assert_eq!(counts.operations.load(Ordering::SeqCst), 3);
}

#[test]
fn feeding_claims_the_ready_read_before_delivering_its_callback() {
    assert_ready_read_cannot_complete_a_later_read(true);
}

#[test]
fn reading_claims_buffered_input_before_delivering_its_callback() {
    assert_ready_read_cannot_complete_a_later_read(false);
}

#[test]
fn buffered_writes_are_bounded_and_pending_accepts_no_new_bytes() {
    let (fixture, counts) = fixture(CHUNK, 1 << WRITE);
    let mut io = stream(&fixture);
    let notifications = Arc::new(Wakes::default());
    let waker = Waker::from(notifications.clone());
    let mut cx = Context::from_waker(&waker);
    let mut input = vec![7; CHUNK * 2];
    assert_eq!(ready(Pin::new(&mut io).poll_write(&mut cx, &input)), CHUNK);
    input.fill(9);
    assert!(Pin::new(&mut io).poll_write(&mut cx, b"next").is_pending());
    assert!(fixture.outgoing().is_empty());
    assert_eq!(fixture.starts(WRITE), 1);
    let completion = fixture.clone();
    std::thread::spawn(move || completion.finish_with(WRITE, 0, usize::MAX, true))
        .join()
        .unwrap();
    assert_eq!(fixture.status(true), ACCEPTED);
    assert_eq!(fixture.status(false), INVALID_STATE);
    assert_eq!(fixture.outgoing(), vec![7; CHUNK]);
    assert!(notifications.0.load(Ordering::SeqCst) > 0);
    assert_eq!(ready(Pin::new(&mut io).poll_write(&mut cx, b"next")), 4);
    fixture.finish(WRITE);
    ready(Pin::new(&mut io).poll_flush(&mut cx));
    assert_eq!(
        unsafe { proof_max_transfer(fixture.ptr.as_ptr(), 0) } as usize,
        CHUNK
    );
    assert_eq!(&fixture.outgoing()[CHUNK..], b"next");
    drop(io);
    drop(fixture);
    assert_released(&counts, 1, 1);
}

#[test]
fn reading_progresses_short_buffered_writes_without_an_explicit_flush() {
    let (fixture, counts) = fixture(2, 1 << WRITE);
    let mut io = stream(&fixture);
    let notifications = Arc::new(Wakes::default());
    let waker = Waker::from(notifications.clone());
    let mut cx = Context::from_waker(&waker);
    assert_eq!(ready(Pin::new(&mut io).poll_write(&mut cx, b"hello")), 5);
    let mut output = [0; 2];
    assert!(poll_read(&mut io, &mut cx, &mut output).is_pending());
    for _ in 0..3 {
        fixture.finish(WRITE);
        assert!(poll_read(&mut io, &mut cx, &mut output).is_pending());
    }
    assert_eq!(fixture.outgoing(), b"hello");
    assert_eq!(fixture.starts(WRITE), 3);
    assert!(!fixture.pending(WRITE));
    fixture.feed(b"ok");
    assert_eq!(ready(poll_read(&mut io, &mut cx, &mut output)), 2);
    assert_eq!(&output, b"ok");
    assert!(notifications.0.load(Ordering::SeqCst) > 0);
    drop(io);
    drop(fixture);
    assert_released(&counts, 1, 1);
}

#[test]
fn shutdown_serializes_write_flush_and_close_while_read_may_overlap() {
    let (fixture, counts) = fixture(16, (1 << WRITE) | (1 << FLUSH) | (1 << SHUTDOWN));
    let mut io = stream(&fixture);
    let waker = Waker::from(Arc::new(Wakes::default()));
    let mut cx = Context::from_waker(&waker);
    assert_eq!(ready(Pin::new(&mut io).poll_write(&mut cx, b"data")), 4);
    assert!(Pin::new(&mut io).poll_shutdown(&mut cx).is_pending());
    assert_eq!(fixture.starts(FLUSH), 0);
    let mut input = [0; 4];
    assert!(poll_read(&mut io, &mut cx, &mut input).is_pending());
    fixture.feed(b"read");
    assert_eq!(ready(poll_read(&mut io, &mut cx, &mut input)), 4);
    fixture.finish(WRITE);
    assert!(Pin::new(&mut io).poll_shutdown(&mut cx).is_pending());
    assert_eq!(fixture.starts(FLUSH), 1);
    assert_eq!(fixture.starts(SHUTDOWN), 0);
    fixture.finish(FLUSH);
    assert!(Pin::new(&mut io).poll_shutdown(&mut cx).is_pending());
    assert!(Pin::new(&mut io).poll_shutdown(&mut cx).is_pending());
    assert_eq!(fixture.starts(FLUSH), 1);
    assert_eq!(fixture.starts(SHUTDOWN), 1);
    fixture.finish(SHUTDOWN);
    ready(Pin::new(&mut io).poll_shutdown(&mut cx));
    assert_eq!(
        error(Pin::new(&mut io).poll_write(&mut cx, b"late")),
        io::ErrorKind::BrokenPipe
    );
    drop(io);
    drop(fixture);
    assert_released(&counts, 1, 1);
}

#[test]
fn dropping_an_unpolled_connect_future_cancels_its_retained_token() {
    let (fixture, counts) = fixture(16, 1 << CONNECT);
    let future = fixture.connect();
    assert!(fixture.pending(CONNECT));
    let weak = Arc::downgrade(&fixture);
    drop(future);
    drop(fixture);
    assert_released(&counts, 0, 0);
    let retained = weak
        .upgrade()
        .expect("C retained operation owns the stream");
    retained.finish_with(CONNECT, 0, usize::MAX, true);
    assert_eq!(retained.status(true), INVALID_STATE);
    assert_eq!(retained.status(false), INVALID_STATE);
    drop(retained);
    assert_released(&counts, 1, 1);
    assert!(weak.upgrade().is_none());
}

#[test]
fn abandoned_stream_keeps_retained_operations_alive_and_cannot_contaminate_a_new_stream() {
    let (old, counts) = fixture(16, 1 << WRITE);
    let mut io = stream(&old);
    let waker = Waker::from(Arc::new(Wakes::default()));
    let mut cx = Context::from_waker(&waker);
    let mut input = [0; 4];
    assert!(poll_read(&mut io, &mut cx, &mut input).is_pending());
    ready(Pin::new(&mut io).poll_write(&mut cx, b"old"));
    let weak = Arc::downgrade(&old);
    drop(io);
    drop(old);
    assert_released(&counts, 0, 0);
    let (fresh, fresh_counts) = fixture(16, 0);
    fresh.feed(b"new");
    let mut next = stream(&fresh);
    let retained = weak.upgrade().unwrap();
    // This completion cannot touch its deliberately unusable byte view: the
    // state check must reject it before inspecting length or copying bytes.
    retained.finish_with(READ, 0, usize::MAX - 1, true);
    assert_eq!(retained.status(true), INVALID_STATE);
    retained.finish(WRITE);
    assert_eq!(retained.status(false), INVALID_STATE);
    drop(retained);
    assert_released(&counts, 1, 1);
    assert_eq!(ready(poll_read(&mut next, &mut cx, &mut input)), 3);
    assert_eq!(&input[..3], b"new");
    drop(next);
    drop(fresh);
    assert_released(&fresh_counts, 1, 1);
}

#[test]
fn malformed_and_permanent_read_failures_do_not_restart_io() {
    for (result, count, expected) in [
        (0, usize::MAX - 1, io::ErrorKind::InvalidData),
        (1, 0, io::ErrorKind::ConnectionReset),
    ] {
        let (fixture, counts) = fixture(16, 1 << READ);
        let mut io = stream(&fixture);
        let waker = Waker::from(Arc::new(Wakes::default()));
        let mut cx = Context::from_waker(&waker);
        let mut input = [0; 4];
        assert!(poll_read(&mut io, &mut cx, &mut input).is_pending());
        fixture.finish_with(READ, result, count, false);
        assert_eq!(error(poll_read(&mut io, &mut cx, &mut input)), expected);
        assert_eq!(error(poll_read(&mut io, &mut cx, &mut input)), expected);
        assert_eq!(fixture.starts(READ), 1);
        drop(io);
        drop(fixture);
        assert_released(&counts, 1, 1);
    }
}

#[test]
fn invalid_write_counts_and_errors_surface_on_flush_and_remain_terminal() {
    for (result, count, expected) in [
        (0, 0, io::ErrorKind::WriteZero),
        (0, 5, io::ErrorKind::InvalidData),
        (1, 0, io::ErrorKind::BrokenPipe),
    ] {
        let (fixture, counts) = fixture(16, 1 << WRITE);
        let mut io = stream(&fixture);
        let waker = Waker::from(Arc::new(Wakes::default()));
        let mut cx = Context::from_waker(&waker);
        assert_eq!(ready(Pin::new(&mut io).poll_write(&mut cx, b"data")), 4);
        fixture.finish_with(WRITE, result, count, false);
        assert_eq!(error(Pin::new(&mut io).poll_flush(&mut cx)), expected);
        assert_eq!(
            error(Pin::new(&mut io).poll_write(&mut cx, b"again")),
            expected
        );
        assert_eq!(fixture.starts(WRITE), 1);
        assert_eq!(fixture.starts(FLUSH), 0);
        drop(io);
        drop(fixture);
        assert_released(&counts, 1, 1);
    }
}

#[test]
fn flush_and_shutdown_errors_do_not_repeat_foreign_callbacks() {
    for kind in [FLUSH, SHUTDOWN] {
        let (fixture, counts) = fixture(16, 1 << kind);
        let mut io = stream(&fixture);
        let waker = Waker::from(Arc::new(Wakes::default()));
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut io).poll_shutdown(&mut cx).is_pending());
        fixture.finish_with(kind, 1, usize::MAX, false);
        assert_eq!(
            error(Pin::new(&mut io).poll_shutdown(&mut cx)),
            io::ErrorKind::ConnectionReset
        );
        assert_eq!(
            error(Pin::new(&mut io).poll_shutdown(&mut cx)),
            io::ErrorKind::ConnectionReset
        );
        assert_eq!(fixture.starts(kind), 1);
        if kind == FLUSH {
            assert_eq!(fixture.starts(SHUTDOWN), 0);
        }
        drop(io);
        drop(fixture);
        assert_released(&counts, 1, 1);
    }
}

#[test]
fn empty_buffers_start_no_operations_and_large_reads_are_bounded() {
    let (fixture, counts) = fixture(CHUNK, 0);
    let mut io = stream(&fixture);
    let waker = Waker::from(Arc::new(Wakes::default()));
    let mut cx = Context::from_waker(&waker);
    assert_eq!(ready(poll_read(&mut io, &mut cx, &mut [])), 0);
    assert_eq!(ready(Pin::new(&mut io).poll_write(&mut cx, &[])), 0);
    assert_eq!(fixture.starts(READ), 0);
    assert_eq!(fixture.starts(WRITE), 0);
    fixture.feed(&vec![5; CHUNK * 2]);
    let mut input = vec![0; CHUNK * 2];
    assert_eq!(ready(poll_read(&mut io, &mut cx, &mut input)), CHUNK);
    assert_eq!(
        unsafe { proof_max_transfer(fixture.ptr.as_ptr(), 1) } as usize,
        CHUNK
    );
    drop(io);
    drop(fixture);
    assert_released(&counts, 1, 1);
}

#[test]
fn released_unfinished_host_work_wakes_its_observer_with_an_error() {
    let (fixture, counts) = fixture(16, 0);
    let mut io = stream(&fixture);
    let notifications = Arc::new(Wakes::default());
    let waker = Waker::from(notifications.clone());
    let mut cx = Context::from_waker(&waker);
    let mut input = [0; 1];
    assert!(poll_read(&mut io, &mut cx, &mut input).is_pending());
    fixture.release_pending();
    assert!(notifications.0.load(Ordering::SeqCst) > 0);
    assert_eq!(
        error(poll_read(&mut io, &mut cx, &mut input)),
        io::ErrorKind::BrokenPipe
    );
    drop(io);
    drop(fixture);
    assert_released(&counts, 1, 1);
}

#[test]
fn completion_racing_stream_drop_releases_owners_once() {
    for _ in 0..64 {
        let (fixture, counts) = fixture(16, 0);
        let mut io = stream(&fixture);
        let waker = Waker::from(Arc::new(Wakes::default()));
        let mut cx = Context::from_waker(&waker);
        let mut input = [0; 1];
        assert!(poll_read(&mut io, &mut cx, &mut input).is_pending());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_barrier = barrier.clone();
        let worker_fixture = fixture.clone();
        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            worker_fixture.feed(b"x");
        });
        barrier.wait();
        drop(io);
        worker.join().unwrap();
        assert!(matches!(fixture.status(true), ACCEPTED | INVALID_STATE));
        drop(fixture);
        assert_released(&counts, 1, 1);
        assert_eq!(counts.operations.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn completion_racing_waker_registration_never_loses_a_wakeup() {
    for _ in 0..64 {
        let (fixture, counts) = fixture(16, 1 << READ);
        fixture.feed(b"x");
        let mut read = CStream(fixture.clone()).read(1);
        let notifications = Arc::new(Wakes::default());
        let waker = Waker::from(notifications.clone());
        let mut cx = Context::from_waker(&waker);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_barrier = barrier.clone();
        let worker_fixture = fixture.clone();
        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            worker_fixture.finish(READ);
        });
        barrier.wait();
        let first_poll = read.as_mut().poll(&mut cx);
        worker.join().unwrap();
        let result = if first_poll.is_pending() {
            assert!(notifications.0.load(Ordering::SeqCst) > 0);
            ready(read.as_mut().poll(&mut cx))
        } else {
            ready(first_poll)
        };
        assert_eq!(&result[..], b"x");
        drop(read);
        drop(fixture);
        assert_released(&counts, 1, 1);
    }
}

#[tokio::test]
async fn synchronous_one_byte_writes_yield_and_preserve_byte_order() {
    let (fixture, counts) = fixture(1, 0);
    let mut io = stream(&fixture);
    let data: Vec<u8> = (0..1024)
        .map(|index| u8::try_from(index % 256).unwrap())
        .collect();
    tokio::time::timeout(Duration::from_secs(2), async {
        io.write_all(&data).await.unwrap();
        io.flush().await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(fixture.outgoing(), data);
    assert_eq!(fixture.starts(WRITE), 1024);
    drop(io);
    drop(fixture);
    assert_released(&counts, 1, 1);
}

#[tokio::test]
async fn connection_timeout_rejects_late_completion_and_failed_connect_releases_owners() {
    let (fixture, counts) = fixture(16, 1 << CONNECT);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), fixture.connect())
            .await
            .is_err()
    );
    fixture.finish(CONNECT);
    assert_eq!(fixture.status(true), INVALID_STATE);
    let mut failed = fixture.connect();
    fixture.finish_with(CONNECT, 1, usize::MAX, false);
    assert_eq!(
        failed.as_mut().await.unwrap_err().kind(),
        io::ErrorKind::ConnectionReset
    );
    drop(failed);
    drop(fixture);
    assert_released(&counts, 1, 1);
    assert_eq!(counts.operations.load(Ordering::SeqCst), 2);
}

fn publish_id(wire: &[u8]) -> u16 {
    let mut frames = wire;
    while !frames.is_empty() {
        let header = frames[0];
        let mut remaining = 0usize;
        let mut offset = 1;
        for shift in [0, 7, 14, 21] {
            let byte = frames[offset];
            offset += 1;
            remaining |= usize::from(byte & 127) << shift;
            if byte & 128 == 0 {
                break;
            }
        }
        let body = &frames[offset..offset + remaining];
        if header >> 4 == 3 {
            assert_eq!((header >> 1) & 3, 1);
            let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
            assert_eq!(&body[2..2 + topic_len], b"proof/topic");
            assert!(body.ends_with(b"data"));
            return u16::from_be_bytes([body[2 + topic_len], body[3 + topic_len]]);
        }
        frames = &frames[offset + remaining..];
    }
    panic!("C fixture did not receive a complete PUBLISH");
}

macro_rules! native_protocol_proof {
    ($name:ident, $native:ident, $configure:expr, $connack:expr, $timeout:pat) => {
        #[tokio::test]
        #[allow(
            clippy::too_many_lines,
            reason = "Keep the protocol scenario setup, actions, and assertions together"
        )]
        async fn $name() {
            let counts = Arc::new(Counts::default());
            let registration = Arc::new(Registration(counts.clone()));
            let stalled = Fixture::new(2, 0, registration.clone());
            let connected = Fixture::new(2, 0, registration.clone());
            let reconnected = Fixture::new(2, 0, registration.clone());
            connected.feed($connack);
            reconnected.feed($connack);
            // The registration handle can be released independently of streams.
            drop(registration);
            let targets = Arc::new(Mutex::new(std::collections::VecDeque::from([
                stalled.clone(),
                connected.clone(),
                reconnected.clone(),
            ])));
            let attempts = Arc::new(AtomicUsize::new(0));
            let observed_attempts = attempts.clone();
            let mut options = $native::MqttOptions::new("proof", "unused.invalid");
            options.set_socket_connector(move |target, _network| {
                assert_eq!(target, "unused.invalid:1883");
                observed_attempts.fetch_add(1, Ordering::SeqCst);
                let target = targets.lock().unwrap().pop_front();
                async move {
                    let fixture =
                        target.ok_or_else(|| io::Error::other("unexpected fourth attempt"))?;
                    fixture.connect().await?;
                    Ok::<_, io::Error>(stream(&fixture))
                }
            });
            let (client, mut eventloop) =
                $native::AsyncClient::builder(options).capacity(10).build();
            ($configure)(&mut eventloop);

            let timeout_error = tokio::time::timeout(Duration::from_secs(3), eventloop.poll())
                .await
                .unwrap()
                .unwrap_err();
            assert!(matches!(timeout_error, $timeout));
            assert!(stalled.pending(READ));
            stalled.finish_with(READ, 0, usize::MAX - 1, false);
            assert_eq!(stalled.status(true), INVALID_STATE);

            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if matches!(
                        eventloop.poll().await.unwrap(),
                        $native::Event::Incoming($native::Packet::ConnAck(_))
                    ) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            let notice = client
                .try_publish_tracked(
                    "proof/topic",
                    b"data",
                    $native::PublishOptions::new($native::QoS::AtLeastOnce),
                )
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if matches!(
                        eventloop.poll().await.unwrap(),
                        $native::Event::Outgoing($native::Outgoing::Publish(_))
                    ) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            let id = publish_id(&connected.outgoing()).to_be_bytes();
            connected.feed(&[0x40, 2, id[0], id[1]]);
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if matches!(
                        eventloop.poll().await.unwrap(),
                        $native::Event::Incoming($native::Packet::PubAck(_))
                    ) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(2), notice.wait_async())
                .await
                .unwrap()
                .unwrap();

            connected.eof();
            tokio::time::timeout(Duration::from_secs(2), async {
                while eventloop.poll().await.is_ok() {}
            })
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if matches!(
                        eventloop.poll().await.unwrap(),
                        $native::Event::Incoming($native::Packet::ConnAck(_))
                    ) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(attempts.load(Ordering::SeqCst), 3);
            assert_eq!(stalled.starts(CONNECT), 1);
            assert_eq!(connected.starts(CONNECT), 1);
            assert_eq!(reconnected.starts(CONNECT), 1);
            drop(eventloop);
            drop(client);
            for fixture in [&stalled, &connected, &reconnected] {
                fixture.release_pending();
            }
            drop(stalled);
            drop(connected);
            drop(reconnected);
            assert_released(&counts, 3, 1);
        }
    };
}

native_protocol_proof!(
    v4_c_stream_connects_publishes_times_out_and_reconnects,
    rumqttc_v4,
    |eventloop: &mut rumqttc_v4::EventLoop| {
        eventloop.network_options.set_connection_timeout(1);
    },
    &[0x20, 2, 0, 0],
    rumqttc_v4::ConnectionError::NetworkTimeout
);

native_protocol_proof!(
    v5_c_stream_connects_publishes_times_out_and_reconnects,
    rumqttc_v5,
    |eventloop: &mut rumqttc_v5::EventLoop| {
        eventloop
            .options
            .set_connect_timeout(Duration::from_millis(250));
    },
    &[0x20, 3, 0, 0, 0],
    rumqttc_v5::ConnectionError::Timeout(_)
);
