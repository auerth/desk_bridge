//! Blocking pipe client with timeouts (used by the agent and the tests).
//!
//! Plain `ReadFile`/`WriteFile` on a pipe cannot time out, and a wedged helper must never stall
//! the agent's input path. The handle is therefore opened `FILE_FLAG_OVERLAPPED` and every call
//! waits on an event with a deadline, cancelling the I/O when it expires.

use crate::protocol::{FrameBuffer, ProtocolError, Request, Response};
use crate::winutil::wide;
use std::io;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED,
    HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING, ReadFile,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::PCWSTR;

/// One connection to a service pipe.
pub struct PipeClient {
    handle: HANDLE,
    event: HANDLE,
    frames: FrameBuffer,
}

// SAFETY: both handles are owned by the client and only used through `&mut self`.
unsafe impl Send for PipeClient {}

impl Drop for PipeClient {
    fn drop(&mut self) {
        // SAFETY: handles are owned and closed once; pending I/O has completed or been cancelled
        // before every call returns.
        unsafe {
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.event);
        }
    }
}

fn win_err(e: &windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

fn remaining(deadline: Instant) -> Option<u32> {
    let left = deadline.checked_duration_since(Instant::now())?;
    Some(left.as_millis().clamp(1, u128::from(u32::MAX - 1)) as u32)
}

impl PipeClient {
    /// Opens `name` (e.g. `\\.\pipe\deskbridge-helper`), waiting up to `timeout` if all
    /// instances are busy. Fails with `NotFound` when the server does not exist.
    pub fn connect(name: &str, timeout: Duration) -> io::Result<Self> {
        let deadline = Instant::now() + timeout;
        let wname = wide(name);
        loop {
            // SECURITY_IDENTIFICATION: the server may identify us but not impersonate us.
            let flags = FILE_FLAGS_AND_ATTRIBUTES(
                FILE_ATTRIBUTE_NORMAL.0
                    | FILE_FLAG_OVERLAPPED.0
                    | SECURITY_SQOS_PRESENT.0
                    | SECURITY_IDENTIFICATION.0,
            );
            // SAFETY: `wname` is NUL-terminated and outlives the call.
            let r = unsafe {
                CreateFileW(
                    PCWSTR(wname.as_ptr()),
                    FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    flags,
                    None,
                )
            };
            match r {
                Ok(handle) => {
                    // SAFETY: plain event creation (manual reset, initially non-signalled).
                    let event = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
                        Ok(e) => e,
                        Err(e) => {
                            // SAFETY: handle was just opened by us.
                            unsafe {
                                let _ = CloseHandle(handle);
                            }
                            return Err(win_err(&e));
                        }
                    };
                    return Ok(Self {
                        handle,
                        event,
                        frames: FrameBuffer::default(),
                    });
                }
                Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() => {
                    let Some(ms) = remaining(deadline) else {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "pipe busy"));
                    };
                    // SAFETY: `wname` is NUL-terminated.
                    let _ = unsafe { WaitNamedPipeW(PCWSTR(wname.as_ptr()), ms.min(500)) };
                }
                Err(e) => return Err(win_err(&e)),
            }
        }
    }

    /// Runs one overlapped operation to completion or deadline.
    fn io(
        &mut self,
        deadline: Instant,
        start: impl FnOnce(HANDLE, *mut OVERLAPPED) -> windows::core::Result<()>,
    ) -> io::Result<usize> {
        let mut ov = OVERLAPPED {
            hEvent: self.event,
            ..Default::default()
        };
        // SAFETY: `ov` lives on this stack frame until the operation has completed or been
        // cancelled and drained below; `self.event` is a valid event handle.
        unsafe {
            let _ = windows::Win32::System::Threading::ResetEvent(self.event);
            match start(self.handle, &raw mut ov) {
                Ok(()) => {}
                Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
                Err(e) if e.code() == ERROR_BROKEN_PIPE.to_hresult() => {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                Err(e) => return Err(win_err(&e)),
            }
            let wait = WaitForSingleObject(self.event, remaining(deadline).unwrap_or(0));
            let mut n = 0u32;
            if wait == WAIT_OBJECT_0 {
                return match GetOverlappedResult(self.handle, &raw const ov, &mut n, false) {
                    Ok(()) => Ok(n as usize),
                    Err(e)
                        if e.code() == ERROR_BROKEN_PIPE.to_hresult()
                            || e.code() == ERROR_PIPE_NOT_CONNECTED.to_hresult() =>
                    {
                        Err(io::ErrorKind::BrokenPipe.into())
                    }
                    Err(e) => Err(win_err(&e)),
                };
            }
            // Timed out (or wait failed): cancel and wait for the cancellation so the kernel no
            // longer references `ov` when this frame returns.
            let _ = CancelIoEx(self.handle, Some(&raw const ov));
            let _ = GetOverlappedResult(self.handle, &raw const ov, &mut n, true);
            if wait == WAIT_TIMEOUT {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "pipe operation timed out",
                ))
            } else {
                Err(io::Error::other("wait failed"))
            }
        }
    }

    fn write_all(&mut self, data: &[u8], deadline: Instant) -> io::Result<()> {
        let mut off = 0;
        while off < data.len() {
            let chunk = &data[off..];
            let n = self.io(deadline, |h, ov| {
                // SAFETY: `chunk` and `ov` stay valid until `io` returns (see there).
                unsafe { WriteFile(h, Some(chunk), None, Some(ov)) }
            })?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            off += n;
        }
        Ok(())
    }

    fn read_frame(&mut self, deadline: Instant) -> io::Result<Vec<u8>> {
        loop {
            match self.frames.next_frame() {
                Ok(Some(p)) => return Ok(p),
                Ok(None) => {}
                Err(e) => return Err(proto_err(e)),
            }
            let mut buf = [0u8; 256];
            let n = self.io(deadline, |h, ov| {
                // SAFETY: `buf` and `ov` stay valid until `io` returns.
                unsafe { ReadFile(h, Some(&mut buf), None, Some(ov)) }
            })?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            self.frames.push(&buf[..n]);
        }
    }

    /// Sends a request without waiting for an answer (injection commands).
    pub fn send(&mut self, req: &Request, timeout: Duration) -> io::Result<()> {
        self.write_all(&req.encode(), Instant::now() + timeout)
    }

    /// Sends a request and waits for its response. Must only be used for requests with
    /// [`Request::expects_response`].
    pub fn call(&mut self, req: &Request, timeout: Duration) -> io::Result<Response> {
        let deadline = Instant::now() + timeout;
        self.write_all(&req.encode(), deadline)?;
        let payload = self.read_frame(deadline)?;
        Response::decode(&payload).map_err(proto_err)
    }
}

fn proto_err(e: ProtocolError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// One-shot `Ping`; true if the server answers with a matching protocol version.
#[must_use]
pub fn ping(name: &str, timeout: Duration) -> bool {
    let Ok(mut c) = PipeClient::connect(name, timeout) else {
        return false;
    };
    matches!(
        c.call(&Request::Ping, timeout),
        Ok(Response::Pong { version, .. }) if version == crate::protocol::PROTOCOL_VERSION
    )
}
