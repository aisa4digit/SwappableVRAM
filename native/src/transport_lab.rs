//! Cross-process CUDA broker transport experiment. This is NOT a Windows paging
//! driver. Its one-in-flight mailbox is deliberately poisoned after a timeout;
//! late replies can never be mistaken for replies to a reused request slot.
use crate::cache::PAGE;
use crate::page_cache::{PageCache, PageKey};
use crate::store::{MIB, Result, VramStore};
use std::ffi::c_void;
use std::sync::atomic::{Ordering, fence};

const MAGIC: u64 = 0x535652414d4c4142;
const VERSION: u32 = 1;
const MAPPING_BYTES: usize = 8192;
pub const OP_READ: u32 = 1;
pub const OP_FILL: u32 = 2;
pub const OP_REVOKE: u32 = 3;
pub const OP_STOP: u32 = 4;
pub const OP_STALL_THEN_EXIT: u32 = 5;
pub const OK: u32 = 1;
pub const HIT: u32 = 2;
pub const MISS: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Frame {
    magic: u64,
    version: u32,
    pub operation: u32,
    sequence: u64,
    pub key: PageKey,
    pub status: u32,
    reserved: u32,
    pub data: [u8; PAGE],
}
impl Frame {
    pub fn request(operation: u32, key: PageKey) -> Self {
        Self {
            magic: MAGIC,
            version: VERSION,
            operation,
            sequence: 0,
            key,
            status: 0,
            reserved: 0,
            data: [0; PAGE],
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if self.magic != MAGIC || self.version != VERSION || self.reserved != 0 {
            return Err("Invalid broker frame ABI".into());
        }
        Ok(())
    }
}
struct Handle(*mut c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
pub struct Mailbox {
    _mapping: Handle,
    request: Handle,
    response: Handle,
    view: *mut c_void,
    sequence: u64,
    poisoned: bool,
}
impl Mailbox {
    fn names(session: &str) -> Result<[Vec<u16>; 3]> {
        if session.is_empty()
            || session.len() > 64
            || !session
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err("Invalid lab session name".into());
        }
        Ok(["Map", "Request", "Response"].map(|suffix| {
            format!("Local\\SwappableVRAM-Lab-{session}-{suffix}\0")
                .encode_utf16()
                .collect()
        }))
    }
    pub fn create(session: &str) -> Result<Self> {
        let names = Self::names(session)?;
        let mapping = Handle(unsafe {
            CreateFileMappingW(
                (-1isize) as *mut c_void,
                std::ptr::null(),
                4,
                0,
                MAPPING_BYTES as u32,
                names[0].as_ptr(),
            )
        });
        check_created(mapping.0)?;
        let request = Handle(unsafe { CreateEventW(std::ptr::null(), 0, 0, names[1].as_ptr()) });
        check_created(request.0)?;
        let response = Handle(unsafe { CreateEventW(std::ptr::null(), 0, 0, names[2].as_ptr()) });
        check_created(response.0)?;
        Self::map(mapping, request, response)
    }
    pub fn open(session: &str) -> Result<Self> {
        let names = Self::names(session)?;
        let mapping = Handle(unsafe { OpenFileMappingW(0xF001F, 0, names[0].as_ptr()) });
        check_handle(mapping.0)?;
        let request = Handle(unsafe { OpenEventW(0x100002, 0, names[1].as_ptr()) });
        check_handle(request.0)?;
        let response = Handle(unsafe { OpenEventW(0x100002, 0, names[2].as_ptr()) });
        check_handle(response.0)?;
        Self::map(mapping, request, response)
    }
    fn map(mapping: Handle, request: Handle, response: Handle) -> Result<Self> {
        let view = unsafe { MapViewOfFile(mapping.0, 0xF001F, 0, 0, MAPPING_BYTES) };
        check_handle(view)?;
        Ok(Self {
            _mapping: mapping,
            request,
            response,
            view,
            sequence: 0,
            poisoned: false,
        })
    }
    fn read(&self) -> Frame {
        fence(Ordering::SeqCst);
        // SAFETY: the mapping has 8192 bytes, Frame is <=8192, mapping is page
        // aligned, and the event protocol gives this process exclusive access.
        unsafe { std::ptr::read_volatile(self.view.cast::<Frame>()) }
    }
    fn write(&mut self, value: Frame) {
        unsafe {
            std::ptr::write_volatile(self.view.cast::<Frame>(), value);
        }
        fence(Ordering::SeqCst);
    }
    /// Wait for the private child broker. No request has been issued yet.
    /// The raw handle is borrowed from a live std::process::Child.
    ///
    /// # Safety
    /// process must be a valid process handle with SYNCHRONIZE rights.
    pub unsafe fn wait_ready(&mut self, process: *mut c_void) -> Result<()> {
        self.wait_response(process, 10_000)?;
        let response = self.read();
        response.validate()?;
        if response.sequence != 0 || response.status != OK {
            return Err("Broker readiness failed".into());
        }
        Ok(())
    }
    /// # Safety
    /// process must remain a valid SYNCHRONIZE-capable process handle until return.
    pub unsafe fn call(
        &mut self,
        mut request: Frame,
        process: *mut c_void,
        timeout_ms: u32,
    ) -> Result<Frame> {
        if self.poisoned {
            return Err("Timed-out mailbox cannot be reused".into());
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Request sequence exhausted")?;
        request.sequence = self.sequence;
        self.write(request);
        set_event(self.request.0)?;
        if let Err(error) = self.wait_response(process, timeout_ms) {
            self.poisoned = true;
            return Err(error);
        }
        let response = self.read();
        if response.validate().is_err()
            || response.sequence != self.sequence
            || response.operation != request.operation
        {
            self.poisoned = true;
            return Err("Stale or invalid broker response".into());
        }
        Ok(response)
    }
    fn wait_response(&self, process: *mut c_void, timeout_ms: u32) -> Result<()> {
        let handles = [self.response.0, process];
        match unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, timeout_ms) } {
            0 => Ok(()),
            1 => Err("Private CUDA broker exited; bypass cache".into()),
            258 => Err("Private CUDA broker timed out; bypass cache".into()),
            _ => Err(last_error("WaitForMultipleObjects")),
        }
    }
}
impl Drop for Mailbox {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.view);
        }
    }
}

pub fn serve(session: &str, device: i32) -> Result<()> {
    let mut mailbox = Mailbox::open(session)?;
    let mut cache = PageCache::new(VramStore::new(device, 16 * MIB)?)?;
    let empty_key = PageKey {
        volume_epoch: 0,
        page_index: 0,
        generation: 0,
    };
    let mut ready = Frame::request(0, empty_key);
    ready.status = OK;
    mailbox.write(ready);
    set_event(mailbox.response.0)?;
    loop {
        match unsafe { WaitForSingleObject(mailbox.request.0, 30_000) } {
            0 => (),
            258 => return Err("Unattended lab broker expired".into()),
            _ => return Err(last_error("WaitForSingleObject")),
        }
        let mut frame = mailbox.read();
        frame.validate()?;
        frame.status = match frame.operation {
            OP_READ => {
                if cache.lookup(frame.key, &mut frame.data) {
                    HIT
                } else {
                    frame.data.fill(0);
                    MISS
                }
            }
            OP_FILL => {
                if cache.fill(frame.key, &frame.data) {
                    OK
                } else {
                    MISS
                }
            }
            OP_REVOKE => {
                cache.revoke();
                OK
            }
            OP_STOP => OK,
            OP_STALL_THEN_EXIT => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                OK
            }
            _ => return Err("Invalid lab operation".into()),
        };
        mailbox.write(frame);
        set_event(mailbox.response.0)?;
        if frame.operation == OP_STOP || frame.operation == OP_STALL_THEN_EXIT {
            return Ok(());
        }
    }
}
fn check_created(handle: *mut c_void) -> Result<()> {
    check_handle(handle)?;
    if unsafe { GetLastError() } == 183 {
        return Err("Lab IPC name already exists".into());
    }
    Ok(())
}
fn check_handle(handle: *mut c_void) -> Result<()> {
    if handle.is_null() {
        Err(last_error("IPC object"))
    } else {
        Ok(())
    }
}
fn set_event(event: *mut c_void) -> Result<()> {
    if unsafe { SetEvent(event) } == 0 {
        Err(last_error("SetEvent"))
    } else {
        Ok(())
    }
}
fn last_error(operation: &str) -> String {
    format!("{operation}: Win32 {}", unsafe { GetLastError() })
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileMappingW(
        file: *mut c_void,
        security: *const c_void,
        protect: u32,
        high: u32,
        low: u32,
        name: *const u16,
    ) -> *mut c_void;
    fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> *mut c_void;
    fn MapViewOfFile(
        mapping: *mut c_void,
        access: u32,
        high: u32,
        low: u32,
        bytes: usize,
    ) -> *mut c_void;
    fn UnmapViewOfFile(view: *mut c_void) -> i32;
    fn CreateEventW(
        security: *const c_void,
        manual: i32,
        initial: i32,
        name: *const u16,
    ) -> *mut c_void;
    fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> *mut c_void;
    fn SetEvent(event: *mut c_void) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, timeout: u32) -> u32;
    fn WaitForMultipleObjects(
        count: u32,
        handles: *const *mut c_void,
        all: i32,
        timeout: u32,
    ) -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetLastError() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_fits_in_mailbox() {
        assert!(std::mem::size_of::<Frame>() <= MAPPING_BYTES);
    }
    #[test]
    fn names_cannot_escape_lab_namespace() {
        assert!(Mailbox::names("abc123-42").is_ok());
        for bad in ["", "Global\\other", "..\\foo", "a/b"] {
            assert!(Mailbox::names(bad).is_err());
        }
    }
}
