//! Read-only PnP discovery for the private filter interface. Enumeration does
//! not install, enable, restart, or open a disk for data access.
use crate::store::Result;
use std::ffi::c_void;

#[repr(C)]
struct Guid {
    a: u32,
    b: u16,
    c: u16,
    d: [u8; 8],
}
const FILTER: Guid = Guid {
    a: 0x79320e2b,
    b: 0x6b29,
    c: 0x4df1,
    d: [0xa8, 0x27, 0x86, 0x91, 0x05, 0x41, 0x2c, 0x47],
};
const DISK: Guid = Guid {
    a: 0x53f56307,
    b: 0xb6bf,
    c: 0x11d0,
    d: [0x94, 0xf2, 0x00, 0xa0, 0xc9, 0x1e, 0xfb, 0x8b],
};
#[repr(C)]
struct InterfaceData {
    size: u32,
    guid: Guid,
    flags: u32,
    reserved: usize,
}
#[repr(C)]
struct DeviceData {
    size: u32,
    guid: Guid,
    devinst: u32,
    reserved: usize,
}
struct DeviceSet(*mut c_void);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}
#[derive(Debug)]
pub struct FilterInterface {
    pub path: String,
    pub instance_id: String,
    pub parent_instance_id: Option<String>,
}
impl FilterInterface {
    pub fn is_lab_target(&self) -> bool {
        let matches = |id: &str| {
            id.to_ascii_lowercase()
                .starts_with(r"scsi\disk&ven_svramlab&prod_pagingdisk\")
        };
        matches(&self.instance_id) || self.parent_instance_id.as_deref().is_some_and(matches)
    }
}
pub fn wait_lab_filter() -> Result<FilterInterface> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut devices = filters()?;
        if devices.len() == 1 && devices[0].is_lab_target() {
            return Ok(devices.remove(0));
        }
        if !devices.is_empty() {
            return Err(format!(
                "Private interface has an unexpected/ambiguous parent: {devices:?}"
            ));
        }
        if std::time::Instant::now() >= deadline {
            return Err("Private lab interface did not start".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
fn win32(context: &str) -> String {
    format!("{context}: Win32 {}", unsafe { GetLastError() })
}
fn wide_string(buffer: &[u16]) -> Result<String> {
    let end = buffer
        .iter()
        .position(|&c| c == 0)
        .ok_or("PnP string lacks terminator")?;
    String::from_utf16(&buffer[..end]).map_err(|e| format!("PnP UTF-16: {e}"))
}
pub fn filters() -> Result<Vec<FilterInterface>> {
    interfaces(&FILTER)
}
pub fn disks() -> Result<Vec<FilterInterface>> {
    interfaces(&DISK)
}
fn interfaces(class: &Guid) -> Result<Vec<FilterInterface>> {
    let raw = unsafe { SetupDiGetClassDevsW(class, std::ptr::null(), std::ptr::null_mut(), 0x12) }; // DIGCF_PRESENT | DIGCF_DEVICEINTERFACE
    if raw == (-1_isize) as *mut c_void {
        return Err(win32("Enumerate private filter interfaces"));
    }
    let set = DeviceSet(raw);
    let mut result = Vec::new();
    for index in 0..1024 {
        let mut interface: InterfaceData = unsafe { std::mem::zeroed() };
        interface.size = std::mem::size_of::<InterfaceData>() as u32;
        if unsafe {
            SetupDiEnumDeviceInterfaces(set.0, std::ptr::null(), class, index, &mut interface)
        } == 0
        {
            return if unsafe { GetLastError() } == 259 {
                Ok(result)
            } else {
                Err(win32("Enumerate interface"))
            };
        }
        let mut required = 0;
        let sizing = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                std::ptr::null_mut(),
                0,
                &mut required,
                std::ptr::null_mut(),
            )
        };
        if sizing != 0 || unsafe { GetLastError() } != 122 || !(8..=65536).contains(&required) {
            return Err(win32("Size private interface path"));
        }
        // SP_DEVICE_INTERFACE_DETAIL_DATA_W has cbSize=8 on x64, with its
        // UTF-16 DevicePath at byte offset 4. u64 storage supplies alignment.
        let mut detail = vec![0_u64; (required as usize).div_ceil(8)];
        let pointer = detail.as_mut_ptr().cast::<u8>();
        unsafe { pointer.cast::<u32>().write(8) };
        let mut device: DeviceData = unsafe { std::mem::zeroed() };
        device.size = std::mem::size_of::<DeviceData>() as u32;
        if unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                pointer.cast(),
                required,
                &mut required,
                &mut device,
            )
        } == 0
        {
            return Err(win32("Read private interface path"));
        }
        let path_units = unsafe {
            std::slice::from_raw_parts(pointer.add(4).cast::<u16>(), (detail.len() * 8 - 4) / 2)
        };
        let path = wide_string(path_units)?;
        let mut instance = [0_u16; 512];
        if unsafe {
            SetupDiGetDeviceInstanceIdW(
                set.0,
                &device,
                instance.as_mut_ptr(),
                instance.len() as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(win32("Read filter device instance"));
        }
        result.push(FilterInterface {
            path,
            instance_id: wide_string(&instance)?,
            parent_instance_id: parent_instance(device.devinst)?,
        });
    }
    Err("Unexpectedly many private filter interfaces; enumeration stopped".into())
}

fn parent_instance(device: u32) -> Result<Option<String>> {
    let mut parent = 0;
    if unsafe { CM_Get_Parent(&mut parent, device, 0) } != 0 {
        return Ok(None);
    }
    let mut text = [0_u16; 512];
    let status = unsafe { CM_Get_Device_IDW(parent, text.as_mut_ptr(), text.len() as u32, 0) };
    if status != 0 {
        return Err(format!("Read PnP parent identity: CONFIGRET {status}"));
    }
    Ok(Some(wide_string(&text)?))
}
#[link(name = "cfgmgr32")]
unsafe extern "system" {
    fn CM_Get_Parent(parent: *mut u32, device: u32, flags: u32) -> u32;
    fn CM_Get_Device_IDW(device: u32, buffer: *mut u16, length: u32, flags: u32) -> u32;
}
#[link(name = "setupapi")]
unsafe extern "system" {
    fn SetupDiGetClassDevsW(
        guid: *const Guid,
        enumerator: *const u16,
        parent: *mut c_void,
        flags: u32,
    ) -> *mut c_void;
    fn SetupDiEnumDeviceInterfaces(
        set: *mut c_void,
        device: *const DeviceData,
        guid: *const Guid,
        index: u32,
        data: *mut InterfaceData,
    ) -> i32;
    fn SetupDiGetDeviceInterfaceDetailW(
        set: *mut c_void,
        interface: *const InterfaceData,
        detail: *mut c_void,
        length: u32,
        required: *mut u32,
        device: *mut DeviceData,
    ) -> i32;
    fn SetupDiGetDeviceInstanceIdW(
        set: *mut c_void,
        device: *const DeviceData,
        buffer: *mut u16,
        length: u32,
        required: *mut u32,
    ) -> i32;
    fn SetupDiDestroyDeviceInfoList(set: *mut c_void) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn x64_setupapi_layout_matches_sdk() {
        assert_eq!(std::mem::size_of::<Guid>(), 16);
        assert_eq!(std::mem::size_of::<InterfaceData>(), 32);
        assert_eq!(std::mem::size_of::<DeviceData>(), 32);
        assert_eq!(std::mem::offset_of!(InterfaceData, reserved), 24);
    }
    #[test]
    fn malformed_device_strings_are_rejected() {
        assert!(wide_string(&[65]).is_err());
        assert!(wide_string(&[0xd800, 0]).is_err());
        assert_eq!(wide_string(&[65, 0, 66]).unwrap(), "A");
    }
}
