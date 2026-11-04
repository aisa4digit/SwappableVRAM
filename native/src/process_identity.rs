//! TokenUser stays stable across UAC split tokens; TokenOwner does not.
use crate::store::Result;
use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
pub(crate) struct ProcessIdentity {
    pub pid: u32,
    pub session: u32,
    pub sid: Vec<u8>,
    pub elevated: bool,
    handle: OwnedHandle,
}
impl ProcessIdentity {
    pub(crate) fn read(pid: u32) -> Result<Self> {
        let raw = unsafe { OpenProcess(0x00101000, 0, pid) }; // SYNCHRONIZE | QUERY_LIMITED_INFORMATION
        if raw.is_null() {
            return Err(format!(
                "Open control peer process: {}",
                std::io::Error::last_os_error()
            ));
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut session = 0;
        if unsafe { ProcessIdToSessionId(pid, &mut session) } == 0 {
            return Err("Read peer session identity".into());
        }
        let mut raw = std::ptr::null_mut();
        if unsafe { OpenProcessToken(handle.as_raw_handle(), 8, &mut raw) } == 0 {
            return Err("Read peer token identity".into());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut size = 0;
        unsafe {
            GetTokenInformation(token.as_raw_handle(), 1, std::ptr::null_mut(), 0, &mut size);
        }
        if unsafe { GetLastError() } != 122 || !(16..=65536).contains(&size) {
            return Err("Invalid peer token size".into());
        }
        let mut data = vec![0u64; (size as usize).div_ceil(8)];
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                1,
                data.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        } == 0
        {
            return Err("Read peer TokenUser".into());
        }
        let sid = unsafe { data.as_ptr().cast::<*const c_void>().read() };
        let length = unsafe { GetLengthSid(sid) } as usize;
        let start = data.as_ptr() as usize;
        let end = start + data.len() * 8;
        if !(8..=68).contains(&length)
            || (sid as usize) < start
            || (sid as usize)
                .checked_add(length)
                .is_none_or(|last| last > end)
        {
            return Err("Invalid peer SID bounds".into());
        }
        let sid = unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), length) }.to_vec();
        let mut elevation = 0u32;
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                20,
                (&mut elevation as *mut u32).cast(),
                4,
                &mut size,
            )
        } == 0
        {
            return Err("Read peer elevation".into());
        }
        Ok(Self {
            pid,
            session,
            sid,
            elevated: elevation != 0,
            handle,
        })
    }
    pub(crate) fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) == 258 }
    }
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn GetLastError() -> u32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(
        token: *mut c_void,
        class: u32,
        data: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
    fn GetLengthSid(sid: *const c_void) -> u32;
}
