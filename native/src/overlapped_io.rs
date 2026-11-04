//! Stable, independently owned Win32 I/O buffers. A cancel request never makes
//! their storage reusable: every accepted operation is harvested first.
use crate::driver_broker::{DeviceIoControl, Overlapped};
use crate::paging_protocol::Frame;
use crate::store::Result;
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::fs::File;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
pub(crate) const CAPACITY: usize = std::mem::size_of::<Frame>();
const WORDS: usize = CAPACITY.div_ceil(8);
const IO_PENDING: u32 = 997;
const IO_INCOMPLETE: u32 = 996;
pub(crate) struct Buffer {
    words: Box<UnsafeCell<[u64; WORDS]>>,
}
impl Buffer {
    pub(crate) fn new() -> Self {
        Self {
            words: Box::new(UnsafeCell::new([0; WORDS])),
        }
    }
    fn pointer(&self) -> *mut c_void {
        self.words.get().cast()
    }
    pub(crate) fn frame(&self) -> &Frame {
        // Frame is repr(C), all integer fields; every bit pattern is valid.
        // Box alignment is >=8 and this buffer is not owned by pending I/O.
        unsafe { &*self.pointer().cast::<Frame>() }
    }
    pub(crate) fn frame_mut(&mut self) -> &mut Frame {
        unsafe { &mut *self.pointer().cast::<Frame>() }
    }
    pub(crate) fn bytes(&self, length: usize) -> &[u8] {
        assert!(length <= CAPACITY);
        unsafe { std::slice::from_raw_parts(self.pointer().cast(), length) }
    }
    fn copy_from(&mut self, bytes: &[u8]) {
        assert!(bytes.len() <= CAPACITY);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.pointer().cast(), bytes.len());
        }
    }
}
pub(crate) struct Request<'a> {
    file: &'a File,
    buffer: Buffer,
    overlap: Box<UnsafeCell<Overlapped>>,
    event: OwnedHandle,
    pending: bool,
    completion: Option<std::result::Result<u32, u32>>,
}
impl<'a> Request<'a> {
    pub(crate) fn new(file: &'a File) -> Result<Self> {
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(format!("Create asynchronous I/O event: Win32 {}", unsafe {
                GetLastError()
            }));
        }
        Ok(Self {
            file,
            buffer: Buffer::new(),
            overlap: Box::new(UnsafeCell::new(Overlapped::default())),
            event: unsafe { OwnedHandle::from_raw_handle(event) },
            pending: false,
            completion: None,
        })
    }
    fn prepare(&mut self) -> Result<()> {
        if self.pending {
            return Err("Cannot reuse storage owned by pending I/O".into());
        }
        if unsafe { ResetEvent(self.event.as_raw_handle()) } == 0 {
            return Err(format!("Reset I/O event: Win32 {}", unsafe {
                GetLastError()
            }));
        }
        self.completion = None;
        unsafe {
            self.overlap
                .get()
                .write(Overlapped::with_event(self.event.as_raw_handle()));
        }
        Ok(())
    }
    fn submitted(&mut self, success: i32) {
        let error = if success == 0 {
            unsafe { GetLastError() }
        } else {
            0
        };
        if success != 0 || error == IO_PENDING {
            self.pending = true;
            self.poll();
        } else {
            self.completion = Some(Err(error));
        }
    }
    pub(crate) fn start_ioctl(&mut self, code: u32) -> Result<()> {
        self.prepare()?;
        let success = unsafe {
            DeviceIoControl(
                self.file.as_raw_handle(),
                code,
                std::ptr::null(),
                0,
                self.buffer.pointer(),
                CAPACITY as u32,
                std::ptr::null_mut(),
                self.overlap.get(),
            )
        };
        self.submitted(success);
        Ok(())
    }
    pub(crate) fn start_pipe_read(&mut self) -> Result<()> {
        self.start_pipe_read_length(CAPACITY)
    }
    pub(crate) fn start_pipe_read_length(&mut self, length: usize) -> Result<()> {
        if length == 0 || length > CAPACITY {
            return Err("Invalid bounded pipe read length".into());
        }
        self.prepare()?;
        let success = unsafe {
            ReadFile(
                self.file.as_raw_handle(),
                self.buffer.pointer(),
                length as u32,
                std::ptr::null_mut(),
                self.overlap.get(),
            )
        };
        self.submitted(success);
        Ok(())
    }
    pub(crate) fn start_pipe_write(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() || bytes.len() > CAPACITY {
            return Err("Invalid bounded pipe write length".into());
        }
        self.prepare()?;
        self.buffer.copy_from(bytes);
        let success = unsafe {
            WriteFile(
                self.file.as_raw_handle(),
                self.buffer.pointer(),
                bytes.len() as u32,
                std::ptr::null_mut(),
                self.overlap.get(),
            )
        };
        self.submitted(success);
        Ok(())
    }
    pub(crate) fn start_pipe_connect(&mut self) -> Result<()> {
        self.prepare()?;
        let success = unsafe { ConnectNamedPipe(self.file.as_raw_handle(), self.overlap.get()) };
        if success == 0 && unsafe { GetLastError() } == 535 {
            // ERROR_PIPE_CONNECTED: no outstanding connect I/O.
            self.completion = Some(Ok(0));
        } else {
            self.submitted(success);
        }
        Ok(())
    }
    pub(crate) fn poll(&mut self) {
        if !self.pending {
            return;
        }
        let mut bytes = 0;
        let success = unsafe {
            GetOverlappedResult(self.file.as_raw_handle(), self.overlap.get(), &mut bytes, 0)
        };
        let error = if success == 0 {
            unsafe { GetLastError() }
        } else {
            0
        };
        if success != 0 {
            self.finish(Ok(bytes));
        } else if error != IO_INCOMPLETE {
            self.finish(Err(error));
        }
    }
    fn finish(&mut self, result: std::result::Result<u32, u32>) {
        self.pending = false;
        self.completion = Some(match result {
            Ok(n) if n as usize > CAPACITY => Err(13),
            other => other,
        });
    }
    pub(crate) fn completion(&self) -> Option<std::result::Result<u32, u32>> {
        self.completion
    }
    pub(crate) fn ready_frame(&self) -> Option<&Frame> {
        if matches!(self.completion, Some(Ok(_))) {
            Some(self.buffer.frame())
        } else {
            None
        }
    }
    /// Exchange completed storage for a spare before reposting. No 64 KiB
    /// payload memcpy is needed, and the worker's buffer is no longer OS-owned.
    pub(crate) fn take_buffer(&mut self, spare: &mut Buffer) -> Result<u32> {
        let count = self
            .completion
            .and_then(|r| r.ok())
            .ok_or("No successful completed I/O buffer")?;
        if self.pending {
            return Err("Cannot extract a pending I/O buffer".into());
        }
        std::mem::swap(&mut self.buffer, spare);
        self.completion = None;
        Ok(count)
    }
    pub(crate) fn cancel(&mut self) {
        if self.pending {
            unsafe {
                CancelIoEx(self.file.as_raw_handle(), self.overlap.get());
            }
        }
    }
    pub(crate) fn drain(&mut self) {
        if self.pending {
            let result = unsafe { finish(self.file, self.overlap.get()) };
            self.finish(result);
        }
    }
    pub(crate) fn wait_any(requests: &[Self], milliseconds: u32) -> Result<()> {
        let mut handles = [std::ptr::null_mut(); 16];
        let mut count = 0;
        for request in requests.iter().filter(|r| r.pending) {
            if count == handles.len() {
                return Err("I/O wait set exceeds its bounded window".into());
            }
            handles[count] = request.event.as_raw_handle();
            count += 1;
        }
        if count == 0 {
            return Ok(());
        }
        let result =
            unsafe { WaitForMultipleObjects(count as u32, handles.as_ptr(), 0, milliseconds) };
        if result < count as u32 || result == 258 {
            Ok(())
        } else {
            Err(format!("Wait for asynchronous I/O: Win32 {}", unsafe {
                GetLastError()
            }))
        }
    }
}
impl Drop for Request<'_> {
    fn drop(&mut self) {
        self.cancel();
        self.drain();
    }
}
/// # Safety
/// The operation was accepted. Its OVERLAPPED, input/output storage, file and
/// event must remain alive until this function confirms completion.
pub(crate) unsafe fn finish(
    file: &File,
    overlap: *mut Overlapped,
) -> std::result::Result<u32, u32> {
    loop {
        let mut bytes = 0;
        if unsafe { GetOverlappedResult(file.as_raw_handle(), overlap, &mut bytes, 1) } != 0 {
            return Ok(bytes);
        }
        let error = unsafe { GetLastError() };
        if error != IO_INCOMPLETE {
            return Err(error);
        }
        std::thread::yield_now();
    }
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(
        security: *const c_void,
        manual: i32,
        initial: i32,
        name: *const u16,
    ) -> *mut c_void;
    fn ResetEvent(event: *mut c_void) -> i32;
    fn GetLastError() -> u32;
    fn GetOverlappedResult(
        file: *mut c_void,
        overlap: *mut Overlapped,
        bytes: *mut u32,
        wait: i32,
    ) -> i32;
    fn CancelIoEx(file: *mut c_void, overlap: *mut Overlapped) -> i32;
    fn WaitForMultipleObjects(
        count: u32,
        handles: *const *mut c_void,
        wait_all: i32,
        milliseconds: u32,
    ) -> u32;
    fn ReadFile(
        file: *mut c_void,
        output: *mut c_void,
        length: u32,
        bytes: *mut u32,
        overlap: *mut Overlapped,
    ) -> i32;
    fn WriteFile(
        file: *mut c_void,
        input: *const c_void,
        length: u32,
        bytes: *mut u32,
        overlap: *mut Overlapped,
    ) -> i32;
    fn ConnectNamedPipe(file: *mut c_void, overlap: *mut Overlapped) -> i32;
}
