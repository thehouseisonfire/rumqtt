//! Owned, nonblocking wake adapters. Platform handles are never interchangeable.
#[cfg(unix)]
mod adapter {
    use std::io::{self, Read, Write};
    use std::os::fd::{AsFd, BorrowedFd};
    use std::os::unix::net::UnixStream;

    pub struct Primitive {
        reader: UnixStream,
        writer: UnixStream,
    }
    impl Primitive {
        pub fn new() -> io::Result<Self> {
            let (reader, writer) = UnixStream::pair()?;
            reader.set_nonblocking(true)?;
            writer.set_nonblocking(true)?;
            Ok(Self { reader, writer })
        }
        pub fn signal(&self) -> io::Result<()> {
            match (&self.writer).write(&[1]) {
                Ok(1) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
                Err(error) => Err(error),
                Ok(_) => Err(io::Error::from(io::ErrorKind::WriteZero)),
            }
        }
        pub fn drain(&self) -> io::Result<bool> {
            let mut buffer = [0; 4096];
            for _ in 0..16 {
                match (&self.reader).read(&mut buffer) {
                    Ok(0) => return Err(io::Error::from(io::ErrorKind::BrokenPipe)),
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(false)
        }
        pub fn wake_failure(&self) {
            // EOF wakes a sleeping poll consumer even when writing a wake byte failed.
            let _ = self.writer.shutdown(std::net::Shutdown::Write);
        }
        #[cfg(test)]
        pub fn test_writer(&self) -> UnixStream {
            self.writer.try_clone().unwrap()
        }
        pub fn borrowed_fd(&self) -> BorrowedFd<'_> {
            self.reader.as_fd()
        }
    }
}

#[cfg(windows)]
mod adapter {
    use std::io;
    use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::DuplicateHandle;
    use windows_sys::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, ResetEvent, SYNCHRONIZATION_SYNCHRONIZE, SetEvent,
    };

    pub struct Primitive {
        event: OwnedHandle,
    }
    impl Primitive {
        pub fn new() -> io::Result<Self> {
            // SAFETY: Null security/name creates an unnamed, non-inheritable event.
            let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: The successful call transfers one owned kernel handle.
            Ok(Self {
                event: unsafe { OwnedHandle::from_raw_handle(event) },
            })
        }
        pub fn signal(&self) -> io::Result<()> {
            // SAFETY: OwnedHandle remains live throughout this operation.
            if unsafe { SetEvent(self.event.as_raw_handle()) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        pub fn drain(&self) -> io::Result<bool> {
            // SAFETY: OwnedHandle remains live throughout this operation.
            if unsafe { ResetEvent(self.event.as_raw_handle()) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(true)
            }
        }
        pub fn wake_failure(&self) {
            let _ = self.signal();
        }
        pub fn borrowed_handle(&self) -> BorrowedHandle<'_> {
            self.event.as_handle()
        }
        pub fn duplicate_handle(&self) -> io::Result<OwnedHandle> {
            let mut duplicate = std::ptr::null_mut();
            // SAFETY: Source is live, destination writable, and the duplicate belongs to this process.
            let result = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    self.event.as_raw_handle(),
                    GetCurrentProcess(),
                    &raw mut duplicate,
                    SYNCHRONIZATION_SYNCHRONIZE,
                    0,
                    0,
                )
            };
            if result == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(unsafe { OwnedHandle::from_raw_handle(duplicate) })
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod adapter {
    use std::io;
    pub struct Primitive;
    impl Primitive {
        pub fn new() -> io::Result<Self> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
        pub fn signal(&self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
        pub fn drain(&self) -> io::Result<bool> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
        pub fn wake_failure(&self) {}
    }
}
pub use adapter::Primitive;
