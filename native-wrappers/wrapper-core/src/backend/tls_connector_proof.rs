//! Private TODO49 prototype. No declarations here are a public C ABI.
use std::ffi::{CString, c_char, c_int, c_void};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::Stream;
use futures_util::task::AtomicWaker;
use rumqttc_core::{DynAsyncReadWrite, TlsHandshakeConnector};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::oneshot;

use crate::transport::{IoFuture, OwnedIo, OwnedStream};

struct Allocator;
static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_CALLS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATION_BYTES: AtomicUsize = AtomicUsize::new(0);
// SAFETY: Every operation delegates the identical pointer/layout to System.
unsafe impl std::alloc::GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let pointer = unsafe { std::alloc::System.alloc(layout) };
        if !pointer.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOCATION_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        let pointer = unsafe { std::alloc::System.realloc(pointer, layout, size) };
        if !pointer.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOCATION_BYTES.fetch_add(size, Ordering::Relaxed);
        }
        pointer
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

const CHUNK: usize = 16 * 1024;
const MAX_CHAINS: usize = 64;
const MAX_OPERATIONS: usize = 256;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const WARMUP: usize = 100;

unsafe extern "C" {
    fn proof_tls_new(ca: *const u8, ca_len: c_int, host: *const c_char) -> *mut c_void;
    fn proof_tls_free(ssl: *mut c_void);
    fn proof_tls_arm_update_on_read(ssl: *mut c_void) -> c_int;
    fn proof_tls_handshake(ssl: *mut c_void) -> c_int;
    fn proof_tls_read(ssl: *mut c_void, out: *mut u8, cap: usize, count: *mut usize) -> c_int;
    fn proof_tls_write(ssl: *mut c_void, data: *const u8, size: usize, count: *mut usize) -> c_int;
    fn proof_tls_shutdown(ssl: *mut c_void) -> c_int;
    fn proof_tls_input(ssl: *mut c_void, data: *const u8, size: c_int) -> c_int;
    fn proof_tls_output(ssl: *mut c_void, data: *mut u8, cap: c_int) -> c_int;
    fn proof_tls_input_eof(ssl: *mut c_void);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
enum Failure {
    #[error("external TLS authentication failed")]
    Authentication,
    #[error("external TLS policy or protocol failed")]
    Policy,
    #[error("external TLS input was truncated")]
    Truncated,
    #[error("external TLS resource limit reached")]
    ResourceLimit,
    #[error("external TLS chain was cancelled")]
    Cancelled,
    #[error("external TLS operation is already pending")]
    Busy,
    #[error("external TLS result is invalid")]
    InvalidResult,
    #[error("external TLS handshake timed out")]
    Timeout,
    #[error("external TLS peer closed the stream")]
    Closed,
}
impl Failure {
    fn io(self) -> io::Error {
        let kind = match self {
            Self::Authentication | Self::Policy => io::ErrorKind::PermissionDenied,
            Self::Truncated => io::ErrorKind::UnexpectedEof,
            Self::ResourceLimit => io::ErrorKind::OutOfMemory,
            Self::Cancelled => io::ErrorKind::Interrupted,
            Self::Busy => io::ErrorKind::WouldBlock,
            Self::InvalidResult => io::ErrorKind::InvalidData,
            Self::Timeout => io::ErrorKind::TimedOut,
            Self::Closed => io::ErrorKind::BrokenPipe,
        };
        let error = io::Error::new(kind, self);
        if matches!(self, Self::Timeout | Self::Closed) {
            error
        } else {
            rumqttc_core::TerminalTransportError::new(error).into_io()
        }
    }
}

#[derive(Default)]
struct Usage {
    chains: usize,
    operations: usize,
    bytes: usize,
    peak_chains: usize,
    peak_operations: usize,
    peak_bytes: usize,
    copied_bytes: usize,
}
#[derive(Default)]
struct Budget(Mutex<Usage>);
impl Budget {
    fn chain(self: &Arc<Self>) -> io::Result<ChainTicket> {
        let mut usage = self.0.lock().unwrap();
        if usage.chains == MAX_CHAINS {
            return Err(Failure::ResourceLimit.io());
        }
        usage.chains += 1;
        usage.peak_chains = usage.peak_chains.max(usage.chains);
        drop(usage);
        Ok(ChainTicket(self.clone()))
    }
    fn reserve(self: &Arc<Self>, bytes: usize) -> io::Result<Ticket> {
        let mut usage = self.0.lock().unwrap();
        if usage.operations == MAX_OPERATIONS || bytes > MAX_BYTES - usage.bytes {
            return Err(Failure::ResourceLimit.io());
        }
        usage.operations += 1;
        usage.bytes += bytes;
        usage.peak_operations = usage.peak_operations.max(usage.operations);
        usage.peak_bytes = usage.peak_bytes.max(usage.bytes);
        drop(usage);
        Ok(Ticket(self.clone(), bytes))
    }
    fn copied(&self, bytes: usize) {
        // Timing runs retain admission accounting, but copy telemetry belongs
        // to the separate instrumented runs alongside allocation counters.
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            self.0.lock().unwrap().copied_bytes += bytes;
        }
    }
}
struct ChainTicket(Arc<Budget>);
impl Drop for ChainTicket {
    fn drop(&mut self) {
        self.0.0.lock().unwrap().chains -= 1;
    }
}
struct Ticket(Arc<Budget>, usize);
impl Drop for Ticket {
    fn drop(&mut self) {
        let mut usage = self.0.0.lock().unwrap();
        usage.operations -= 1;
        usage.bytes -= self.1;
    }
}

#[derive(Default)]
struct Cancellation {
    cancelled: AtomicBool,
    lower: AtomicWaker,
    host: AtomicWaker,
}
impl Cancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.lower.wake();
        self.host.wake();
    }
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Read,
    Write,
    Flush,
    Shutdown,
}
struct Operation {
    kind: Kind,
    input: Bytes,
    limit: usize,
    _ticket: Ticket,
    slot: Arc<AtomicBool>,
    completed: AtomicBool,
}
impl Operation {
    fn complete(&self) {
        if !self.completed.swap(true, Ordering::AcqRel) {
            self.slot.store(false, Ordering::Release);
        }
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.complete();
    }
}
enum Value {
    Bytes(Bytes),
    Count(usize),
    Unit,
}
struct ReadOwner {
    data: Vec<u8>,
    _operation: Arc<Operation>,
}
impl AsRef<[u8]> for ReadOwner {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}
struct Request {
    op: Arc<Operation>,
    reply: oneshot::Sender<io::Result<Value>>,
}
impl Request {
    fn finish(self, value: io::Result<Value>) {
        self.op.complete();
        let _ = self.reply.send(value);
    }
    fn read_result(self, mut data: Vec<u8>, count: usize) {
        data.truncate(count);
        self.op.complete();
        let result = Bytes::from_owner(ReadOwner {
            data,
            _operation: self.op,
        });
        let _ = self.reply.send(Ok(Value::Bytes(result)));
    }
}
struct Observation {
    cancellation: Arc<Cancellation>,
    _operation: Arc<Operation>,
    armed: bool,
}
impl Observation {
    const fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}
struct Endpoint {
    requests: flume::Sender<Request>,
    cancellation: Arc<Cancellation>,
    budget: Arc<Budget>,
    reader: Arc<AtomicBool>,
    writer: Arc<AtomicBool>,
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}
impl Endpoint {
    fn new(
        budget: Arc<Budget>,
        cancellation: Arc<Cancellation>,
    ) -> (Arc<Self>, flume::Receiver<Request>) {
        let (requests, receive) = flume::bounded(2);
        (
            Arc::new(Self {
                requests,
                cancellation,
                budget,
                reader: Arc::default(),
                writer: Arc::default(),
            }),
            receive,
        )
    }
    fn submit(&self, kind: Kind, input: Bytes, limit: usize) -> IoFuture<Value> {
        let make = || {
            if self.cancellation.is_cancelled() {
                return Err(Failure::Cancelled.io());
            }
            if limit > CHUNK || matches!(kind, Kind::Read | Kind::Write) && limit == 0 {
                return Err(Failure::InvalidResult.io());
            }
            // Reserve before copying or allocating any operation buffer.
            let ticket = self.budget.reserve(limit)?;
            let slot = if matches!(kind, Kind::Read) {
                &self.reader
            } else {
                &self.writer
            };
            if slot.swap(true, Ordering::AcqRel) {
                return Err(Failure::Busy.io());
            }
            let op = Arc::new(Operation {
                kind,
                input,
                limit,
                _ticket: ticket,
                slot: slot.clone(),
                completed: AtomicBool::new(false),
            });
            let guard = Observation {
                cancellation: self.cancellation.clone(),
                _operation: op.clone(),
                armed: true,
            };
            let (reply, receive) = oneshot::channel();
            self.requests
                .try_send(Request { op, reply })
                .map_err(|_| Failure::Cancelled.io())?;
            Ok((guard, receive))
        };
        match make() {
            Err(error) => Box::pin(async { Err(error) }),
            Ok((mut guard, receive)) => Box::pin(async move {
                let value = receive.await.map_err(|_| Failure::Cancelled.io());
                guard.disarm();
                value?
            }),
        }
    }
}
impl OwnedIo for Endpoint {
    fn read(&self, max: usize) -> IoFuture<Bytes> {
        let future = self.submit(Kind::Read, Bytes::new(), max);
        Box::pin(async {
            match future.await? {
                Value::Bytes(bytes) => Ok(bytes),
                _ => Err(Failure::InvalidResult.io()),
            }
        })
    }
    fn write(&self, input: Bytes) -> IoFuture<usize> {
        let length = input.len();
        let future = self.submit(Kind::Write, input, length);
        Box::pin(async {
            match future.await? {
                Value::Count(count) => Ok(count),
                _ => Err(Failure::InvalidResult.io()),
            }
        })
    }
    fn flush(&self) -> IoFuture<()> {
        let future = self.submit(Kind::Flush, Bytes::new(), 0);
        Box::pin(async {
            match future.await? {
                Value::Unit => Ok(()),
                _ => Err(Failure::InvalidResult.io()),
            }
        })
    }
    fn shutdown(&self) -> IoFuture<()> {
        let future = self.submit(Kind::Shutdown, Bytes::new(), 0);
        Box::pin(async {
            match future.await? {
                Value::Unit => Ok(()),
                _ => Err(Failure::InvalidResult.io()),
            }
        })
    }
}

struct LowerDriver {
    stream: DynAsyncReadWrite,
    requests: Pin<Box<flume::r#async::RecvStream<'static, Request>>>,
    cancellation: Arc<Cancellation>,
    read: Option<(Request, Vec<u8>)>,
    write: Option<Request>,
    _work: Option<crate::execution::BlockingWork>,
}
impl Future for LowerDriver {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = &mut *self;
        this.cancellation.lower.register(cx.waker());
        if this.cancellation.is_cancelled() {
            return Poll::Ready(());
        }
        // Bounded work per poll; a ready host cannot monopolize shared execution.
        for _ in 0..16 {
            match this.requests.as_mut().poll_next(cx) {
                Poll::Ready(Some(request)) => {
                    if matches!(request.op.kind, Kind::Read) {
                        let length = request.op.limit;
                        this.read = Some((request, vec![0; length]));
                    } else {
                        this.write = Some(request);
                    }
                }
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => break,
            }
        }
        if let Some((request, bytes)) = &mut this.read {
            let mut buffer = ReadBuf::new(bytes);
            match Pin::new(&mut this.stream).poll_read(cx, &mut buffer) {
                Poll::Ready(result) => {
                    let count = buffer.filled().len();
                    let (request, bytes) = this.read.take().unwrap();
                    match result {
                        Ok(()) => request.read_result(bytes, count),
                        Err(error) => request.finish(Err(error)),
                    }
                }
                Poll::Pending => {
                    let _ = request;
                }
            }
        }
        if let Some(request) = &this.write {
            let result = match request.op.kind {
                Kind::Write => Pin::new(&mut this.stream)
                    .poll_write(cx, &request.op.input)
                    .map(|r| r.map(Value::Count)),
                Kind::Flush => Pin::new(&mut this.stream)
                    .poll_flush(cx)
                    .map(|r| r.map(|()| Value::Unit)),
                Kind::Shutdown => Pin::new(&mut this.stream)
                    .poll_shutdown(cx)
                    .map(|r| r.map(|()| Value::Unit)),
                Kind::Read => unreachable!(),
            };
            if let Poll::Ready(result) = result {
                this.write.take().unwrap().finish(result);
            }
        }
        Poll::Pending
    }
}
fn lower(
    stream: DynAsyncReadWrite,
    budget: Arc<Budget>,
    cancellation: Arc<Cancellation>,
) -> Arc<Endpoint> {
    let (endpoint, requests) = Endpoint::new(budget, cancellation.clone());
    let driver = LowerDriver {
        stream,
        requests: Box::pin(requests.into_stream()),
        cancellation,
        read: None,
        write: None,
        _work: crate::execution::register_blocking_work(),
    };
    tokio::spawn(driver);
    endpoint
}

struct Engine(*mut c_void);
// SAFETY: Each engine belongs exclusively to one actor on the host reactor.
// It is never accessed concurrently and owns its OpenSSL context and BIOs.
unsafe impl Send for Engine {}
impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { proof_tls_free(self.0) };
    }
}
impl Engine {
    fn new(roots: &[u8], hostname: &str) -> io::Result<Self> {
        let hostname = CString::new(hostname).map_err(|_| Failure::InvalidResult.io())?;
        let length = c_int::try_from(roots.len()).map_err(|_| Failure::ResourceLimit.io())?;
        let engine = unsafe { proof_tls_new(roots.as_ptr(), length, hostname.as_ptr()) };
        if engine.is_null() {
            Err(Failure::Policy.io())
        } else {
            Ok(Self(engine))
        }
    }
    fn checked(code: c_int) -> io::Result<c_int> {
        match code {
            1..=4 => Ok(code),
            10 => Err(Failure::Authentication.io()),
            12 => Err(Failure::Truncated.io()),
            _ => Err(Failure::Policy.io()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Handshaking,
    Established,
}

#[derive(Default)]
struct LowerWriter {
    write: Option<IoFuture<usize>>,
    flush: Option<IoFuture<()>>,
    shutdown: Option<IoFuture<()>>,
    dirty: bool,
}

struct HostDriver {
    engine: Engine,
    lower: Arc<Endpoint>,
    requests: Pin<Box<flume::r#async::RecvStream<'static, Request>>>,
    cancellation: Arc<Cancellation>,
    handshake: Option<oneshot::Sender<io::Result<()>>>,
    phase: Phase,
    read: Option<Request>,
    write: Option<Request>,
    lower_read: Option<IoFuture<Bytes>>,
    lower_writer: LowerWriter,
    ciphertext: Bytes,
    want_read: bool,
    shutdown_sent: bool,
    peer_closed: bool,
    scratch: Vec<u8>,
    _scratch: Ticket,
    _chain: ChainTicket,
}
impl HostDriver {
    fn fail(&mut self, error: io::Error) {
        if let Some(handshake) = self.handshake.take() {
            let _ = handshake.send(Err(error));
        } else if let Some(request) = self.read.take() {
            request.finish(Err(error));
        } else if let Some(request) = self.write.take() {
            request.finish(Err(error));
        }
        self.cancellation.cancel();
    }
    fn poll_lower(&mut self, cx: &mut Context<'_>) -> io::Result<bool> {
        let read_progress = if let Some(read) = &mut self.lower_read
            && let Poll::Ready(result) = read.as_mut().poll(cx)
        {
            self.lower_read = None;
            let bytes = result?;
            if bytes.is_empty() {
                unsafe { proof_tls_input_eof(self.engine.0) };
            } else {
                let count = unsafe {
                    proof_tls_input(
                        self.engine.0,
                        bytes.as_ptr(),
                        c_int::try_from(bytes.len()).map_err(|_| Failure::InvalidResult.io())?,
                    )
                };
                if count != c_int::try_from(bytes.len()).map_err(|_| Failure::InvalidResult.io())? {
                    return Err(Failure::InvalidResult.io());
                }
                self.lower.budget.copied(bytes.len());
            }
            true
        } else {
            false
        };
        let write_progress = if let Some(write) = &mut self.lower_writer.write
            && let Poll::Ready(result) = write.as_mut().poll(cx)
        {
            self.lower_writer.write = None;
            let count = result?;
            if count == 0 || count > self.ciphertext.len() {
                return Err(Failure::InvalidResult.io());
            }
            self.ciphertext = self.ciphertext.slice(count..);
            self.lower_writer.dirty = true;
            true
        } else {
            false
        };
        Ok(read_progress | write_progress)
    }
    fn poll_tls(&mut self) -> io::Result<bool> {
        let mut progress = false;
        if self.phase == Phase::Handshaking {
            let code = Engine::checked(unsafe { proof_tls_handshake(self.engine.0) })?;
            if code == 4 {
                return Err(Failure::Closed.io());
            }
            if code == 1 {
                self.phase = Phase::Established;
            }
            self.want_read = code == 2;
            progress |= self.phase == Phase::Established;
        } else {
            self.want_read = false;
            if let Some(request) = &self.write {
                match request.op.kind {
                    Kind::Write => {
                        let mut count = 0;
                        let code = Engine::checked(unsafe {
                            proof_tls_write(
                                self.engine.0,
                                request.op.input.as_ptr(),
                                request.op.input.len(),
                                &raw mut count,
                            )
                        })?;
                        if code == 4 {
                            return Err(Failure::Closed.io());
                        }
                        self.want_read |= code == 2;
                        if code == 1 {
                            if count == 0 || count > request.op.input.len() {
                                return Err(Failure::InvalidResult.io());
                            }
                            self.lower.budget.copied(count);
                            self.write.take().unwrap().finish(Ok(Value::Count(count)));
                            progress = true;
                        }
                    }
                    Kind::Shutdown if !self.shutdown_sent => {
                        let code = Engine::checked(unsafe { proof_tls_shutdown(self.engine.0) })?;
                        self.want_read |= code == 2;
                        self.shutdown_sent = code == 1;
                        if self.shutdown_sent
                            && let Some(read) = self.read.take()
                        {
                            read.finish(Err(Failure::Cancelled.io()));
                        }
                    }
                    Kind::Read => unreachable!(),
                    _ => {}
                }
            }
            if let Some(request) = &self.read {
                if self.peer_closed {
                    self.read.take().unwrap().read_result(Vec::new(), 0);
                } else {
                    let mut count = 0;
                    let code = Engine::checked(unsafe {
                        proof_tls_read(
                            self.engine.0,
                            self.scratch.as_mut_ptr(),
                            request.op.limit,
                            &raw mut count,
                        )
                    })?;
                    self.want_read |= code == 2;
                    if code == 1 || code == 4 {
                        self.peer_closed = code == 4;
                        if count > request.op.limit {
                            return Err(Failure::InvalidResult.io());
                        }
                        self.lower.budget.copied(count);
                        let data = self.scratch[..count].to_vec();
                        self.read.take().unwrap().read_result(data, count);
                        progress = true;
                    }
                }
            }
        }
        Ok(progress)
    }
    fn step(&mut self, cx: &mut Context<'_>) -> io::Result<Poll<()>> {
        self.cancellation.host.register(cx.waker());
        if self.cancellation.is_cancelled() {
            return Ok(Poll::Ready(()));
        }
        let mut progress = false;
        for _ in 0..2 {
            match self.requests.as_mut().poll_next(cx) {
                Poll::Ready(Some(request)) => {
                    if matches!(request.op.kind, Kind::Read) {
                        self.read = Some(request);
                    } else {
                        self.write = Some(request);
                    }
                    progress = true;
                }
                Poll::Ready(None) => return Ok(Poll::Ready(())),
                Poll::Pending => break,
            }
        }
        progress |= self.poll_lower(cx)?;
        progress |= self.poll_tls()?;
        // Drain the entire TLS output flight before flushing the lower stream.
        if self.ciphertext.is_empty() && self.lower_writer.write.is_none() {
            let count = unsafe {
                proof_tls_output(
                    self.engine.0,
                    self.scratch.as_mut_ptr(),
                    c_int::try_from(CHUNK).unwrap(),
                )
            };
            if count > 0 {
                self.ciphertext =
                    Bytes::copy_from_slice(&self.scratch[..usize::try_from(count).unwrap()]);
                self.lower.budget.copied(usize::try_from(count).unwrap());
                progress = true;
            }
        }
        if !self.ciphertext.is_empty()
            && self.lower_writer.write.is_none()
            && self.lower_writer.flush.is_none()
            && self.lower_writer.shutdown.is_none()
        {
            self.lower_writer.write = Some(self.lower.write(self.ciphertext.clone()));
            progress = true;
        }
        let drained = self.ciphertext.is_empty() && self.lower_writer.write.is_none();
        if self.want_read && self.lower_read.is_none() {
            self.lower_read = Some(self.lower.read(CHUNK));
            progress = true;
        }
        if drained
            && (self.lower_writer.dirty
                || self
                    .write
                    .as_ref()
                    .is_some_and(|request| matches!(request.op.kind, Kind::Flush | Kind::Shutdown)))
            && self.lower_writer.flush.is_none()
            && self.lower_writer.shutdown.is_none()
        {
            // Handshake and read-generated control output must reach buffered
            // transports without waiting for an application flush. Reads keep
            // progressing independently of this serialized writer operation.
            self.lower_writer.flush = Some(self.lower.flush());
            progress = true;
        }
        // Poll an existing flush even if a TLS read has since produced output.
        // New control records wait for this writer-family operation to retire.
        if let Some(flush) = &mut self.lower_writer.flush
            && let Poll::Ready(result) = flush.as_mut().poll(cx)
        {
            self.lower_writer.flush = None;
            result?;
            self.lower_writer.dirty = false;
            progress = true;
            if drained {
                match self.write.as_ref().map(|request| request.op.kind) {
                    Some(Kind::Shutdown) if self.shutdown_sent => {
                        self.lower_writer.shutdown = Some(self.lower.shutdown());
                    }
                    Some(Kind::Flush) => {
                        self.write.take().unwrap().finish(Ok(Value::Unit));
                    }
                    _ => {}
                }
            }
        }
        if drained
            && !self.lower_writer.dirty
            && self.lower_writer.flush.is_none()
            && self.phase == Phase::Established
            && let Some(handshake) = self.handshake.take()
        {
            let _ = handshake.send(Ok(()));
        }
        if let Some(shutdown) = &mut self.lower_writer.shutdown
            && let Poll::Ready(result) = shutdown.as_mut().poll(cx)
        {
            self.lower_writer.shutdown = None;
            result?;
            self.write.take().unwrap().finish(Ok(Value::Unit));
            return Ok(Poll::Ready(()));
        }
        if progress {
            cx.waker().wake_by_ref();
        }
        Ok(Poll::Pending)
    }
}
impl Future for HostDriver {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match self.step(cx) {
            Ok(result) => result,
            Err(error) => {
                self.fail(error);
                Poll::Ready(())
            }
        }
    }
}
impl Drop for HostDriver {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

enum HostMessage {
    Start(Box<HostDriver>),
    Stop,
}
struct Reactor {
    send: flume::Sender<HostMessage>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    budget: Arc<Budget>,
    inline: bool,
    closed: AtomicBool,
    tasks: Mutex<Vec<(Arc<Cancellation>, tokio::task::JoinHandle<()>)>>,
}
impl std::fmt::Debug for Reactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateTlsReactor")
    }
}
impl Reactor {
    fn new() -> Arc<Self> {
        let inline = match std::env::var("RUMQTTC_TLS_PROOF_HOST_MODE").as_deref() {
            Ok("inline") => true,
            Ok("threaded") | Err(_) => false,
            Ok(_) => panic!("RUMQTTC_TLS_PROOF_HOST_MODE must be inline or threaded"),
        };
        let (send, receive) = flume::bounded(MAX_CHAINS);
        if inline {
            return Arc::new(Self {
                send,
                thread: Mutex::new(None),
                budget: Arc::default(),
                inline,
                closed: AtomicBool::new(false),
                tasks: Mutex::default(),
            });
        }
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut tasks = tokio::task::JoinSet::new();
                let mut cancellations: Vec<Arc<Cancellation>> = Vec::new();
                loop {
                    tokio::select! {
                        message = receive.recv_async() => match message {
                            Ok(HostMessage::Start(driver)) => {
                                cancellations.retain(|cancel| !cancel.is_cancelled());
                                cancellations.push(driver.cancellation.clone());
                                tasks.spawn(*driver);
                            }
                            Ok(HostMessage::Stop) | Err(_) => break,
                        },
                        _ = tasks.join_next(), if !tasks.is_empty() => {},
                    }
                }
                for cancel in cancellations {
                    cancel.cancel();
                }
                while tasks.join_next().await.is_some() {}
            });
        });
        Arc::new(Self {
            send,
            thread: Mutex::new(Some(thread)),
            budget: Arc::default(),
            inline,
            closed: AtomicBool::new(false),
            tasks: Mutex::default(),
        })
    }
    fn start(&self, driver: HostDriver) -> io::Result<()> {
        let mut tasks = self.tasks.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return Err(Failure::Cancelled.io());
        }
        if self.inline {
            let cancellation = driver.cancellation.clone();
            let task = tokio::spawn(driver);
            tasks.retain(|(_, task)| !task.is_finished());
            tasks.push((cancellation, task));
            Ok(())
        } else {
            let result = self.send.try_send(HostMessage::Start(Box::new(driver)));
            // A failed send owns a C engine; destroy it after releasing the lock.
            drop(tasks);
            result.map_err(|_| Failure::ResourceLimit.io())
        }
    }
    async fn join(&self) {
        let tasks = {
            let mut tasks = self.tasks.lock().unwrap();
            self.closed.store(true, Ordering::Release);
            std::mem::take(&mut *tasks)
        };
        for (cancellation, _) in &tasks {
            cancellation.cancel();
        }
        for (_, task) in tasks {
            task.await.unwrap();
        }
        let thread = self.thread.lock().unwrap().take();
        if let Some(thread) = thread {
            self.send.send(HostMessage::Stop).unwrap();
            thread.join().unwrap();
        }
    }
}
impl Drop for Reactor {
    fn drop(&mut self) {
        // Explicit join is part of the proof's unload contract. Never join from
        // a driver callback or an arbitrary owner destructor.
        let _ = self.send.try_send(HostMessage::Stop);
    }
}
#[derive(Debug)]
struct Connector {
    reactor: Arc<Reactor>,
    roots: Bytes,
    update_on_read: bool,
}
impl Connector {
    fn open(
        &self,
        server_name: String,
        deadline: Option<Instant>,
        stream: DynAsyncReadWrite,
    ) -> Pin<Box<dyn Future<Output = io::Result<Arc<Endpoint>>> + Send>> {
        let reactor = self.reactor.clone();
        let roots = self.roots.clone();
        let update_on_read = self.update_on_read;
        Box::pin(async move {
            if deadline.is_some_and(|time| Instant::now() >= time) {
                return Err(Failure::Timeout.io());
            }
            let cancellation = Arc::new(Cancellation::default());
            let chain = reactor.budget.chain()?;
            // Account for scratch, ciphertext staging, and the existing adapter write buffer.
            let scratch = reactor.budget.reserve(CHUNK * 3)?;
            let lower = lower(stream, reactor.budget.clone(), cancellation.clone());
            let (plain, requests) = Endpoint::new(reactor.budget.clone(), cancellation.clone());
            let (handshake, receive) = oneshot::channel();
            let engine = Engine::new(&roots, &server_name)?;
            if update_on_read && unsafe { proof_tls_arm_update_on_read(engine.0) } != 1 {
                return Err(Failure::Policy.io());
            }
            let driver = HostDriver {
                engine,
                lower,
                requests: Box::pin(requests.into_stream()),
                cancellation,
                handshake: Some(handshake),
                phase: Phase::Handshaking,
                read: None,
                write: None,
                lower_read: None,
                lower_writer: LowerWriter::default(),
                ciphertext: Bytes::new(),
                want_read: true,
                shutdown_sent: false,
                peer_closed: false,
                scratch: vec![0; CHUNK],
                _scratch: scratch,
                _chain: chain,
            };
            reactor.start(driver)?;
            let result = async { receive.await.map_err(|_| Failure::Cancelled.io())? };
            if let Some(deadline) = deadline {
                tokio::time::timeout_at(deadline.into(), result)
                    .await
                    .map_err(|_| Failure::Timeout.io())??;
                if Instant::now() >= deadline {
                    return Err(Failure::Timeout.io());
                }
            } else {
                result.await?;
            }
            if plain.cancellation.is_cancelled() {
                return Err(Failure::Cancelled.io());
            }
            Ok(plain)
        })
    }
}

impl TlsHandshakeConnector for Connector {
    fn connect(
        &self,
        server_name: String,
        _port: u16,
        deadline: Option<Instant>,
        stream: DynAsyncReadWrite,
    ) -> Pin<Box<dyn Future<Output = io::Result<DynAsyncReadWrite>> + Send>> {
        let open = self.open(server_name, deadline, stream);
        let registration = self.reactor.clone();
        Box::pin(async move {
            Ok(Box::new(OwnedStream::new(Arc::new(ReturnedIo {
                endpoint: open.await?,
                _registration: registration,
            }))) as DynAsyncReadWrite)
        })
    }
}

struct ReturnedIo {
    endpoint: Arc<Endpoint>,
    _registration: Arc<Reactor>,
}
impl OwnedIo for ReturnedIo {
    fn read(&self, max: usize) -> IoFuture<Bytes> {
        self.endpoint.read(max)
    }
    fn write(&self, bytes: Bytes) -> IoFuture<usize> {
        self.endpoint.write(bytes)
    }
    fn flush(&self) -> IoFuture<()> {
        self.endpoint.flush()
    }
    fn shutdown(&self) -> IoFuture<()> {
        self.endpoint.shutdown()
    }
}

struct Fixture {
    roots: Bytes,
    acceptor: tokio_rustls::TlsAcceptor,
}
impl Fixture {
    fn new() -> Self {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let mut server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
        )
        .unwrap();
        server.send_tls13_tickets = 0;
        Self {
            roots: cert.pem().into(),
            acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(server)),
        }
    }
    fn connector(&self, reactor: &Arc<Reactor>) -> Connector {
        Connector {
            reactor: reactor.clone(),
            roots: self.roots.clone(),
            update_on_read: false,
        }
    }
}
async fn wrap_client(
    fixture: &Fixture,
    reactor: Option<&Arc<Reactor>>,
    stream: DynAsyncReadWrite,
) -> io::Result<DynAsyncReadWrite> {
    if let Some(reactor) = reactor {
        fixture
            .connector(reactor)
            .connect(
                "localhost".into(),
                8883,
                Some(Instant::now() + Duration::from_secs(5)),
                stream,
            )
            .await
    } else {
        let mut builder = native_tls::TlsConnector::builder();
        builder
            .disable_built_in_roots(true)
            .add_root_certificate(native_tls::Certificate::from_pem(&fixture.roots).unwrap())
            .min_protocol_version(Some(native_tls::Protocol::Tlsv13))
            .max_protocol_version(Some(native_tls::Protocol::Tlsv13));
        let config = rumqttc_core::TlsConfiguration::NativeConnector(builder.build().unwrap());
        rumqttc_core::tls_connect("localhost", 8883, &config, stream)
            .await
            .map_err(io::Error::other)
    }
}
async fn settle(budget: &Budget) {
    for _ in 0..100 {
        if budget.0.lock().unwrap().operations == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("lower stream work did not retire");
}

#[tokio::test]
async fn c_tls_handshake_traffic_and_authenticated_shutdown() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = Fixture::new();
    let reactor = Reactor::new();
    let (client, server) = tokio::io::duplex(31);
    let acceptor = fixture.acceptor.clone();
    let server = tokio::spawn(async move {
        let mut stream = acceptor.accept(server).await.unwrap();
        let mut data = vec![0; CHUNK * 2 + 19];
        stream.read_exact(&mut data).await.unwrap();
        stream.write_all(&data).await.unwrap();
        stream.flush().await.unwrap();
        assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
        stream.shutdown().await.unwrap();
    });
    let mut stream = wrap_client(&fixture, Some(&reactor), Box::new(client))
        .await
        .unwrap();
    let message = vec![42; CHUNK * 2 + 19];
    stream.write_all(&message).await.unwrap();
    stream.flush().await.unwrap();
    let mut received = vec![0; message.len()];
    stream.read_exact(&mut received).await.unwrap();
    assert_eq!(received, message);
    stream.shutdown().await.unwrap();
    drop(stream);
    server.await.unwrap();
    reactor.join().await;
    settle(&reactor.budget).await;
    assert_eq!(reactor.budget.0.lock().unwrap().chains, 0);
}

#[tokio::test]
async fn lower_read_does_not_block_writer_and_cancellation_retires_both() {
    use tokio::io::AsyncReadExt;
    let budget = Arc::new(Budget::default());
    let cancellation = Arc::new(Cancellation::default());
    let (client, mut peer) = tokio::io::duplex(1);
    let endpoint = lower(Box::new(client), budget.clone(), cancellation.clone());
    let read = endpoint.read(1);
    let write = endpoint.write(Bytes::from_static(b"x"));
    assert_eq!(write.await.unwrap(), 1);
    assert_eq!(peer.read_u8().await.unwrap(), b'x');
    assert!(endpoint.read(1).await.is_err());
    drop(read);
    assert!(cancellation.is_cancelled());
    drop(endpoint);
    settle(&budget).await;
}

#[tokio::test]
async fn cancelled_unpolled_work_retains_capacity_until_final_release() {
    let budget = Arc::new(Budget::default());
    let cancellation = Arc::new(Cancellation::default());
    let (endpoint, receive) = Endpoint::new(budget.clone(), cancellation.clone());
    let operation = endpoint.write(Bytes::from_static(b"retained"));
    let retained = receive.recv().unwrap();
    drop(operation);
    assert!(cancellation.is_cancelled());
    assert_eq!(budget.0.lock().unwrap().operations, 1);
    assert_eq!(budget.0.lock().unwrap().bytes, 8);
    drop(retained);
    assert_eq!(budget.0.lock().unwrap().operations, 0);
}

#[tokio::test]
async fn hostname_rejection_is_terminal_and_releases_chain() {
    let fixture = Fixture::new();
    let reactor = Reactor::new();
    let (client, server) = tokio::io::duplex(128);
    let acceptor = fixture.acceptor.clone();
    let server = tokio::spawn(async move {
        let _ = acceptor.accept(server).await;
    });
    let failure = fixture
        .connector(&reactor)
        .connect(
            "wrong.invalid".into(),
            8883,
            Some(Instant::now() + Duration::from_secs(5)),
            Box::new(client),
        )
        .await
        .err()
        .unwrap();
    assert!(rumqttc_core::TerminalTransportError::find(&failure).is_some());
    server.await.unwrap();
    reactor.join().await;
    settle(&reactor.budget).await;
}

#[tokio::test]
async fn handshake_timeout_does_not_stall_shared_execution() {
    let fixture = Fixture::new();
    let reactor = Reactor::new();
    let (client, _unresponsive_peer) = tokio::io::duplex(16);
    let connect = fixture.connector(&reactor).connect(
        "localhost".into(),
        8883,
        Some(Instant::now() + Duration::from_millis(30)),
        Box::new(client),
    );
    let progress = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        17
    });
    assert_eq!(connect.await.err().unwrap().kind(), io::ErrorKind::TimedOut);
    assert_eq!(progress.await.unwrap(), 17);
    reactor.join().await;
    settle(&reactor.budget).await;
}

fn native_config(fixture: &Fixture) -> rumqttc_core::TlsConfiguration {
    let mut builder = native_tls::TlsConnector::builder();
    builder
        .disable_built_in_roots(true)
        .add_root_certificate(native_tls::Certificate::from_pem(&fixture.roots).unwrap())
        .min_protocol_version(Some(native_tls::Protocol::Tlsv13))
        .max_protocol_version(Some(native_tls::Protocol::Tlsv13));
    rumqttc_core::TlsConfiguration::NativeConnector(builder.build().unwrap())
}
fn tls_config(fixture: &Fixture, reactor: Option<&Arc<Reactor>>) -> rumqttc_core::TlsConfiguration {
    reactor.map_or_else(
        || native_config(fixture),
        |reactor| rumqttc_core::TlsConfiguration::Connector(Arc::new(fixture.connector(reactor))),
    )
}
async fn mqtt_packet(stream: &mut DynAsyncReadWrite) -> io::Result<(u8, Vec<u8>)> {
    use tokio::io::AsyncReadExt;
    let header = stream.read_u8().await?;
    let mut length = 0;
    for shift in [0, 7, 14, 21] {
        let byte = stream.read_u8().await?;
        length |= usize::from(byte & 127) << shift;
        if byte < 128 {
            if length > 65536 {
                return Err(Failure::InvalidResult.io());
            }
            let mut packet = vec![0; length];
            stream.read_exact(&mut packet).await?;
            return Ok((header, packet));
        }
    }
    Err(Failure::InvalidResult.io())
}
fn puback(header: u8, body: &[u8], mqtt5: bool, payload: usize) -> [u8; 4] {
    assert_eq!(header, 0x32);
    let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
    let id = 2 + topic_len;
    assert_eq!(body.len(), id + 2 + usize::from(mqtt5) + payload);
    if mqtt5 {
        assert_eq!(body[id + 2], 0);
    }
    assert!(body[body.len() - payload..].iter().all(|byte| *byte == 42));
    [0x40, 2, body[id], body[id + 1]]
}

trait PacketLimits {
    fn proof_limits(&mut self);
}
impl PacketLimits for rumqttc_v4::MqttOptions {
    fn proof_limits(&mut self) {
        self.set_max_packet_size(65536, 65536);
    }
}
impl PacketLimits for rumqttc_v5::MqttOptions {
    fn proof_limits(&mut self) {
        self.set_max_packet_size(Some(65536));
    }
}
fn packet_limits(options: &mut impl PacketLimits) {
    options.proof_limits();
}

struct Sample {
    allocation_calls: usize,
    allocation_bytes: usize,
    throughput: f64,
    p99_us: f64,
    copied_bytes: usize,
    peak_bytes: usize,
    peak_operations: usize,
    peak_chains: usize,
}
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent test axes model the native composition matrix"
)]
struct Scenario {
    mqtt5: bool,
    nested: bool,
    websocket: bool,
    memory: bool,
    payload: usize,
    messages: usize,
}
#[allow(
    clippy::too_many_lines,
    reason = "Keep equivalent v4/v5 measured workloads and their fixture setup together"
)]
#[allow(
    clippy::result_large_err,
    reason = "The WebSocket handshake callback uses the dependency response type"
)]
async fn sample(scenario: Scenario, external: bool) -> Sample {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Scenario {
        mqtt5,
        nested,
        websocket,
        memory,
        payload,
        messages,
    } = scenario;
    let broker = Fixture::new();
    let proxy = Fixture::new();
    let reactor = external.then(Reactor::new);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (base_send, base_receive) = flume::bounded(1);
    let acceptor = broker.acceptor.clone();
    let proxy_acceptor = proxy.acceptor.clone();
    let server = tokio::spawn(async move {
        let mut stream: DynAsyncReadWrite = if memory {
            Box::new(base_receive.recv_async().await.unwrap())
        } else {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            Box::new(stream)
        };
        if nested {
            stream = Box::new(proxy_acceptor.accept(stream).await.unwrap());
            let mut connect = Vec::new();
            while !connect.ends_with(b"\r\n\r\n") {
                connect.push(stream.read_u8().await.unwrap());
                assert!(connect.len() < 4096);
            }
            assert!(connect.starts_with(b"CONNECT localhost:"));
            stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
            stream.flush().await.unwrap();
        }
        let mut stream = acceptor.accept(stream).await.unwrap();
        // Every pending application read must also permit TLS control writes.
        stream.get_mut().1.refresh_traffic_keys().unwrap();
        let connack: &[u8] = if mqtt5 {
            &[0x20, 3, 0, 0, 0]
        } else {
            &[0x20, 2, 0, 0]
        };
        if websocket {
            #[cfg(feature = "websocket")]
            {
                use futures_util::StreamExt;
                let mut socket = async_tungstenite::tokio::accept_hdr_async(
                    stream,
                    |_: &tungstenite::handshake::server::Request,
                     mut response: tungstenite::handshake::server::Response| {
                        response
                            .headers_mut()
                            .insert("Sec-WebSocket-Protocol", "mqtt".parse().unwrap());
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                assert_eq!(socket.next().await.unwrap().unwrap().into_data()[0], 0x10);
                socket
                    .send(tungstenite::Message::Binary(Bytes::copy_from_slice(
                        connack,
                    )))
                    .await
                    .unwrap();
                // WsAdapter may split an MQTT packet across WebSocket messages.
                let mut buffered = bytes::BytesMut::new();
                for _ in 0..messages + WARMUP {
                    let (header, body) = loop {
                        if buffered.len() >= 2 {
                            let mut length = 0;
                            let mut prefix = 1;
                            let mut complete = false;
                            for shift in [0, 7, 14, 21] {
                                if prefix == buffered.len() {
                                    break;
                                }
                                let byte = buffered[prefix];
                                prefix += 1;
                                length |= usize::from(byte & 127) << shift;
                                if byte < 128 {
                                    complete = true;
                                    break;
                                }
                            }
                            if complete && buffered.len() >= prefix + length {
                                let packet = buffered.split_to(prefix + length);
                                break (packet[0], packet[prefix..].to_vec());
                            }
                        }
                        let message = socket.next().await.unwrap().unwrap().into_data();
                        buffered.extend_from_slice(&message);
                        assert!(buffered.len() <= 65536);
                    };
                    socket
                        .send(tungstenite::Message::Binary(Bytes::copy_from_slice(
                            &puback(header, &body, mqtt5, payload),
                        )))
                        .await
                        .unwrap();
                }
            }
            #[cfg(not(feature = "websocket"))]
            panic!("WebSocket proof requires its feature");
        } else {
            let mut stream: DynAsyncReadWrite = Box::new(stream);
            assert_eq!(mqtt_packet(&mut stream).await.unwrap().0, 0x10);
            stream.write_all(connack).await.unwrap();
            stream.flush().await.unwrap();
            for _ in 0..messages + WARMUP {
                let (header, body) = mqtt_packet(&mut stream).await.unwrap();
                stream
                    .write_all(&puback(header, &body, mqtt5, payload))
                    .await
                    .unwrap();
                stream.flush().await.unwrap();
            }
        }
    });
    let tls = tls_config(&broker, reactor.as_ref());
    let proxy_tls = tls_config(&proxy, reactor.as_ref());
    let data = Bytes::from(vec![42; payload]);
    let mut latencies = Vec::with_capacity(messages);
    macro_rules! run_client {
        ($mqtt:ident) => {{
            let mut options = if websocket {
                #[cfg(feature = "websocket")]
                {
                    $mqtt::MqttOptions::websocket_with_tls_config(
                        "external-tls-proof",
                        format!("wss://localhost:{port}/mqtt"),
                        tls,
                    )
                    .unwrap()
                }
                #[cfg(not(feature = "websocket"))]
                {
                    unreachable!()
                }
            } else {
                let mut options = $mqtt::MqttOptions::new(
                    "external-tls-proof",
                    $mqtt::Broker::tcp("localhost", port),
                );
                options.set_transport($mqtt::Transport::tls_with_config(tls));
                options
            };
            options.set_keep_alive(0);
            packet_limits(&mut options);
            options.set_socket_connector(move |_target, mut network| {
                network.set_tcp_nodelay(true);
                let base_send = base_send.clone();
                async move {
                    if memory {
                        let (client, server) = tokio::io::duplex(37);
                        base_send
                            .send_async(server)
                            .await
                            .map_err(io::Error::other)?;
                        Ok(Box::new(client) as DynAsyncReadWrite)
                    } else {
                        let stream = rumqttc_core::default_socket_connect(
                            format!("127.0.0.1:{port}"),
                            network,
                        )
                        .await?;
                        Ok(Box::new(stream) as DynAsyncReadWrite)
                    }
                }
            });
            if nested {
                options.set_proxy(rumqttc_core::Proxy::https("localhost", port, proxy_tls));
            }
            let (client, mut eventloop) = $mqtt::AsyncClient::builder(options).capacity(64).build();
            let driver = tokio::spawn(async move {
                while let Ok(event) = eventloop.poll().await {
                    let _ = event;
                }
            });
            for _ in 0..WARMUP {
                client
                    .publish_tracked("a", data.clone(), $mqtt::PublishOptions::at_least_once())
                    .await
                    .unwrap()
                    .wait_async()
                    .await
                    .unwrap();
            }
            ALLOCATION_CALLS.store(0, Ordering::Relaxed);
            ALLOCATION_BYTES.store(0, Ordering::Relaxed);
            COUNT_ALLOCATIONS.store(
                std::env::var_os("RUMQTTC_TLS_PROOF_COUNT_ALLOCATIONS").is_some(),
                Ordering::Relaxed,
            );
            let started = Instant::now();
            for _ in 0..messages {
                let request = Instant::now();
                client
                    .publish_tracked("a", data.clone(), $mqtt::PublishOptions::at_least_once())
                    .await
                    .unwrap()
                    .wait_async()
                    .await
                    .unwrap();
                latencies.push(request.elapsed().as_secs_f64() * 1e6);
            }
            let elapsed = started.elapsed();
            COUNT_ALLOCATIONS.store(false, Ordering::Relaxed);
            driver.abort();
            let _ = driver.await;
            drop(client);
            elapsed
        }};
    }
    let elapsed = if mqtt5 {
        run_client!(rumqttc_v5)
    } else {
        run_client!(rumqttc_v4)
    };
    server.await.unwrap();
    latencies.sort_by(f64::total_cmp);
    let p99_us = latencies[(latencies.len() * 99 / 100).min(latencies.len() - 1)];
    let mut result = Sample {
        allocation_calls: ALLOCATION_CALLS.load(Ordering::Relaxed),
        allocation_bytes: ALLOCATION_BYTES.load(Ordering::Relaxed),
        throughput: f64::from(u32::try_from(messages).unwrap()) / elapsed.as_secs_f64(),
        p99_us,
        copied_bytes: 0,
        peak_bytes: 0,
        peak_operations: 0,
        peak_chains: 0,
    };
    if let Some(reactor) = reactor {
        reactor.join().await;
        settle(&reactor.budget).await;
        let usage = reactor.budget.0.lock().unwrap();
        assert_eq!((usage.chains, usage.operations, usage.bytes), (0, 0, 0));
        result.copied_bytes = usage.copied_bytes;
        result.peak_bytes = usage.peak_bytes;
        result.peak_operations = usage.peak_operations;
        result.peak_chains = usage.peak_chains;
    }
    result
}

#[tokio::test]
async fn native_v4_v5_compose_nested_external_tls_wss_and_custom_base_streams() {
    for mqtt5 in [false, true] {
        for nested in [false, true] {
            for websocket in [false, true] {
                if websocket && !cfg!(feature = "websocket") {
                    continue;
                }
                let scenario = Scenario {
                    mqtt5,
                    nested,
                    websocket,
                    memory: true,
                    payload: 1024,
                    messages: 2,
                };
                tokio::time::timeout(Duration::from_secs(20), sample(scenario, true))
                    .await
                    .unwrap();
            }
        }
    }
}

#[test]
#[ignore = "release-only measurement; emits private prototype CSV"]
fn measure_external_tls_delivery_gate() {
    let messages: usize =
        std::env::var("RUMQTTC_TLS_PROOF_MESSAGES").map_or(10_000, |v| v.parse().unwrap());
    let repetitions: usize =
        std::env::var("RUMQTTC_TLS_PROOF_REPETITIONS").map_or(7, |v| v.parse().unwrap());
    assert!(messages > 0 && repetitions > 0);
    println!(concat!(
        "TLS_PROOF,execution,mqtt,nested,payload,repetition,external,",
        "messages_per_second,p99_us,copied_bytes,peak_bytes,peak_operations,",
        "peak_chains,allocation_calls,allocation_bytes"
    ));
    for shared in [false, true] {
        let runtime = if shared {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap()
        } else {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
        };
        for mqtt5 in [false, true] {
            for nested in [false, true] {
                for payload in [1024, 16384] {
                    for repetition in 0..repetitions {
                        for external in if repetition % 2 == 0 {
                            [false, true]
                        } else {
                            [true, false]
                        } {
                            let scenario = Scenario {
                                mqtt5,
                                nested,
                                websocket: false,
                                memory: false,
                                payload,
                                messages,
                            };
                            let result = runtime.block_on(async {
                                tokio::time::timeout(
                                    Duration::from_secs(120),
                                    sample(scenario, external),
                                )
                                .await
                                .unwrap()
                            });
                            println!(
                                concat!(
                                    "TLS_PROOF,{},{},{},{},{},{},{:.3},{:.3},",
                                    "{},{},{},{},{},{}"
                                ),
                                if shared { "shared" } else { "dedicated" },
                                if mqtt5 { 5 } else { 4 },
                                nested,
                                payload,
                                repetition,
                                external,
                                result.throughput,
                                result.p99_us,
                                result.copied_bytes,
                                result.peak_bytes,
                                result.peak_operations,
                                result.peak_chains,
                                result.allocation_calls,
                                result.allocation_bytes
                            );
                        }
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn retained_cancelled_operations_exhaust_registration_capacity() {
    let budget = Arc::new(Budget::default());
    let mut retained = Vec::new();
    for _ in 0..MAX_OPERATIONS {
        let (endpoint, receive) = Endpoint::new(budget.clone(), Arc::default());
        let future = endpoint.read(CHUNK);
        retained.push(receive.recv().unwrap());
        drop(future);
    }
    assert_eq!(retained.len(), MAX_OPERATIONS);
    let (endpoint, _) = Endpoint::new(budget.clone(), Arc::default());
    assert!(endpoint.read(CHUNK).await.is_err());
    assert_eq!(budget.0.lock().unwrap().operations, MAX_OPERATIONS);
    retained.clear();
    assert_eq!(budget.0.lock().unwrap().operations, 0);
    assert_eq!(budget.0.lock().unwrap().bytes, 0);
}

#[tokio::test]
async fn established_stream_does_not_expire_with_handshake_deadline() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = Fixture::new();
    let reactor = Reactor::new();
    let (client, server) = tokio::io::duplex(1024);
    let acceptor = fixture.acceptor.clone();
    let server = tokio::spawn(async move {
        let mut stream = acceptor.accept(server).await.unwrap();
        let byte = stream.read_u8().await.unwrap();
        stream.write_u8(byte).await.unwrap();
        stream.flush().await.unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut stream = fixture
        .connector(&reactor)
        .connect("localhost".into(), 8883, Some(deadline), Box::new(client))
        .await
        .unwrap();
    tokio::time::sleep_until((deadline + Duration::from_millis(10)).into()).await;
    stream.write_u8(57).await.unwrap();
    stream.flush().await.unwrap();
    assert_eq!(stream.read_u8().await.unwrap(), 57);
    drop(stream);
    server.await.unwrap();
    reactor.join().await;
    settle(&reactor.budget).await;
}

#[tokio::test]
async fn lower_eof_without_close_notify_is_truncation() {
    use tokio::io::AsyncReadExt;
    let fixture = Fixture::new();
    let reactor = Reactor::new();
    let (client, server) = tokio::io::duplex(1024);
    let acceptor = fixture.acceptor.clone();
    let (release, wait) = oneshot::channel();
    let server = tokio::spawn(async move {
        let stream = acceptor.accept(server).await.unwrap();
        wait.await.unwrap();
        drop(stream);
    });
    let mut stream = wrap_client(&fixture, Some(&reactor), Box::new(client))
        .await
        .unwrap();
    release.send(()).unwrap();
    let error = stream.read_u8().await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    drop(stream);
    server.await.unwrap();
    reactor.join().await;
    settle(&reactor.budget).await;
}

#[tokio::test]
async fn shutdown_sends_close_notify_with_an_application_read_pending() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = Fixture::new();
    let reactor = Reactor::new();
    let (client, server) = tokio::io::duplex(128);
    let acceptor = fixture.acceptor.clone();
    let server = tokio::spawn(async move {
        let mut stream = acceptor.accept(server).await.unwrap();
        assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
    });
    let mut stream = wrap_client(&fixture, Some(&reactor), Box::new(client))
        .await
        .unwrap();
    let mut buffer = [0; 1];
    let mut pending_read = Box::pin(stream.read(&mut buffer));
    std::future::poll_fn(|cx| {
        assert!(pending_read.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(pending_read);
    tokio::time::timeout(Duration::from_secs(2), stream.shutdown())
        .await
        .unwrap()
        .unwrap();
    drop(stream);
    server.await.unwrap();
    reactor.join().await;
    settle(&reactor.budget).await;
}

#[tokio::test]
async fn untrusted_certificate_chain_is_terminal() {
    let fixture = Fixture::new();
    let unrelated = Fixture::new();
    let reactor = Reactor::new();
    let (client, server) = tokio::io::duplex(128);
    let acceptor = fixture.acceptor.clone();
    let server = tokio::spawn(async move {
        let _ = acceptor.accept(server).await;
    });
    let error = unrelated
        .connector(&reactor)
        .connect(
            "localhost".into(),
            8883,
            Some(Instant::now() + Duration::from_secs(2)),
            Box::new(client),
        )
        .await
        .err()
        .unwrap();
    assert!(rumqttc_core::TerminalTransportError::find(&error).is_some());
    drop(unrelated);
    server.await.unwrap();
    reactor.join().await;
    settle(&reactor.budget).await;
}

#[derive(Default)]
struct FlushState {
    entered: AtomicBool,
    released: AtomicBool,
    completed: AtomicUsize,
    unblocked: usize,
    fail: AtomicBool,
    wake: AtomicWaker,
}
struct GatedFlush<S> {
    stream: S,
    state: Arc<FlushState>,
}
impl<S: AsyncRead + Unpin> AsyncRead for GatedFlush<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buffer)
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for GatedFlush<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.state.wake.register(cx.waker());
        if self.state.completed.load(Ordering::Acquire) >= self.state.unblocked
            && !self.state.released.load(Ordering::Acquire)
        {
            self.state.entered.store(true, Ordering::Release);
            return Poll::Pending;
        }
        if self.state.fail.load(Ordering::Acquire) {
            return Poll::Ready(Err(io::ErrorKind::PermissionDenied.into()));
        }
        let result = Pin::new(&mut self.stream).poll_flush(cx);
        if matches!(result, Poll::Ready(Ok(()))) {
            self.state.completed.fetch_add(1, Ordering::Release);
        }
        result
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[tokio::test]
async fn buffered_lower_stream_flushes_handshake_and_read_generated_control_output() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::time::timeout(Duration::from_secs(3), async {
        let fixture = Fixture::new();
        let reactor = Reactor::new();
        let (client, server) = tokio::io::duplex(31);
        let state = Arc::new(FlushState {
            released: AtomicBool::new(true),
            ..FlushState::default()
        });
        let acceptor = fixture.acceptor.clone();
        let (send_greeting, greeting_requested) = oneshot::channel();
        let server = tokio::spawn(async move {
            let mut stream = acceptor.accept(server).await.unwrap();
            greeting_requested.await.unwrap();
            stream.write_u8(61).await.unwrap();
            stream.flush().await.unwrap();
            assert_eq!(stream.read_u8().await.unwrap(), 62);
            assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
        });
        let mut connector = fixture.connector(&reactor);
        connector.update_on_read = true;
        let plain = connector
            .open(
                "localhost".into(),
                Some(Instant::now() + Duration::from_secs(2)),
                Box::new(GatedFlush {
                    stream: tokio::io::BufStream::with_capacity(CHUNK * 4, CHUNK * 4, client),
                    state: state.clone(),
                }),
            )
            .await
            .unwrap();
        let handshake_flushes = state.completed.load(Ordering::Acquire);
        send_greeting.send(()).unwrap();
        assert_eq!(plain.read(1).await.unwrap().as_ref(), &[61]);
        // The read generates a key update. It must reach the peer without an
        // application write or flush, even though the lower stream buffers it.
        while state.completed.load(Ordering::Acquire) == handshake_flushes {
            tokio::task::yield_now().await;
        }
        assert_eq!(plain.write(Bytes::from_static(&[62])).await.unwrap(), 1);
        plain.flush().await.unwrap();
        plain.shutdown().await.unwrap();
        server.await.unwrap();
        drop(plain);
        reactor.join().await;
        settle(&reactor.budget).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn handshake_waits_for_final_lower_flush_and_propagates_its_failure() {
    tokio::time::timeout(Duration::from_secs(3), async {
        for fail in [false, true] {
            let fixture = Fixture::new();
            let reactor = Reactor::new();
            let (client, server) = tokio::io::duplex(CHUNK * 4);
            let state = Arc::new(FlushState {
                // Flush ClientHello, then hold back the client's final flight.
                unblocked: 1,
                ..FlushState::default()
            });
            let acceptor = fixture.acceptor.clone();
            let server = tokio::spawn(async move { acceptor.accept(server).await.is_ok() });
            let connector = fixture.connector(&reactor);
            let mut connect = connector.open(
                "localhost".into(),
                Some(Instant::now() + Duration::from_secs(2)),
                Box::new(GatedFlush {
                    stream: tokio::io::BufStream::with_capacity(CHUNK * 4, CHUNK * 4, client),
                    state: state.clone(),
                }),
            );
            tokio::select! {
                _ = &mut connect => panic!("connect completed before the final flush"),
                () = async {
                    while !state.entered.load(Ordering::Acquire) {
                        tokio::task::yield_now().await;
                    }
                } => {}
            }
            assert!(futures_util::poll!(&mut connect).is_pending());
            state.fail.store(fail, Ordering::Release);
            state.released.store(true, Ordering::Release);
            state.wake.wake();
            let result = connect.await;
            if fail {
                assert_eq!(
                    result.err().unwrap().kind(),
                    io::ErrorKind::PermissionDenied
                );
            } else {
                drop(result.unwrap());
            }
            reactor.join().await;
            settle(&reactor.budget).await;
            assert_eq!(server.await.unwrap(), !fail);
            assert_eq!(reactor.budget.0.lock().unwrap().chains, 0);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn handshake_timeout_retires_a_pending_lower_flush() {
    let fixture = Fixture::new();
    let reactor = Reactor::new();
    let (client, _peer) = tokio::io::duplex(CHUNK * 4);
    let state = Arc::new(FlushState::default());
    let error = fixture
        .connector(&reactor)
        .open(
            "localhost".into(),
            Some(Instant::now() + Duration::from_millis(100)),
            Box::new(GatedFlush {
                stream: tokio::io::BufStream::with_capacity(CHUNK * 4, CHUNK * 4, client),
                state: state.clone(),
            }),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(state.entered.load(Ordering::Acquire));
    reactor.join().await;
    settle(&reactor.budget).await;
    assert_eq!(reactor.budget.0.lock().unwrap().chains, 0);
}

#[tokio::test]
async fn tls_control_writes_wait_for_an_existing_lower_flush() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::time::timeout(Duration::from_secs(3), async {
        let fixture = Fixture::new();
        let reactor = Reactor::new();
        let (client, server) = tokio::io::duplex(1024);
        let state = Arc::new(FlushState {
            released: AtomicBool::new(true),
            ..FlushState::default()
        });
        let acceptor = fixture.acceptor.clone();
        let (request_update, update_requested) = oneshot::channel();
        let server = tokio::spawn(async move {
            let mut stream = acceptor.accept(server).await.unwrap();
            update_requested.await.unwrap();
            stream.get_mut().1.refresh_traffic_keys().unwrap();
            stream.write_u8(61).await.unwrap();
            stream.flush().await.unwrap();
            // Keep the connection alive until the host has drained its response.
            assert_eq!(stream.read_u8().await.unwrap(), 62);
        });
        let mut connector = fixture.connector(&reactor);
        connector.update_on_read = true;
        let plain = connector
            .open(
                "localhost".into(),
                Some(Instant::now() + Duration::from_secs(2)),
                Box::new(GatedFlush {
                    stream: client,
                    state: state.clone(),
                }),
            )
            .await
            .unwrap();
        state.released.store(false, Ordering::Release);
        let read = plain.read(1);
        let flush = plain.flush();
        while !state.entered.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        request_update.send(()).unwrap();
        assert_eq!(read.await.unwrap().as_ref(), &[61]);
        // Let the host finish the poll which consumed the key-update request.
        tokio::time::sleep(Duration::from_millis(10)).await;
        state.released.store(true, Ordering::Release);
        state.wake.wake();
        flush.await.unwrap();
        assert_eq!(plain.write(Bytes::from_static(&[62])).await.unwrap(), 1);
        plain.flush().await.unwrap();
        server.await.unwrap();
        drop(plain);
        reactor.join().await;
        settle(&reactor.budget).await;
    })
    .await
    .unwrap();
}
