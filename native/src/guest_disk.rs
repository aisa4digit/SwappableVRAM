//! Storage setup ONLY in the disposable WinPE guest, after a runtime identity
//! guard. Requires one exact SVRAMLAB SCSI PDO, a 1 GiB disk, and free R:.
//! QEMU maps that PDO exclusively to its private target.raw, never a host disk.
use crate::cache::{BlockIo, PAGE};
use crate::driver_broker::DeviceIoControl;
use crate::guest_lab::require_test_signing;
use crate::lab_origin::LabOrigin;
use crate::store::{MIB, Result};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;

#[repr(C)]
#[derive(Default)]
struct DeviceNumber {
    kind: u32,
    disk: u32,
    partition: u32,
}
#[repr(C)]
#[derive(Default)]
struct Extents {
    count: u32,
    reserved: u32,
    disk: u32,
    reserved2: u32,
    offset: u64,
    length: u64,
}
// Only initialized POD integer records (or u64) are used as output buffers.
fn query<T: Default>(file: &std::fs::File, code: u32) -> Result<T> {
    let mut out = T::default();
    let mut bytes = 0;
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            std::ptr::null(),
            0,
            (&mut out as *mut T).cast(),
            std::mem::size_of::<T>() as u32,
            &mut bytes,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(format!(
            "Guest storage query {code:#x}: {}",
            std::io::Error::last_os_error()
        ));
    }
    if bytes != std::mem::size_of::<T>() as u32 {
        return Err("Guest storage query size mismatch".into());
    }
    Ok(out)
}
fn target_number() -> Result<u32> {
    let mut disks: Vec<_> = crate::device_interfaces::disks()?
        .into_iter()
        .filter(|d| {
            d.instance_id
                .to_ascii_lowercase()
                .starts_with(r"scsi\disk&ven_svramlab&prod_pagingdisk\")
        })
        .collect();
    if disks.len() != 1 {
        return Err("Expected exactly one SVRAMLAB PAGINGDISK physical disk interface".into());
    }
    let disk = disks.remove(0);
    let file = OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&disk.path)
        .map_err(|e| e.to_string())?;
    let number: DeviceNumber = query(&file, 0x2d1080)?; // IOCTL_STORAGE_GET_DEVICE_NUMBER
    let length: u64 = query(&file, 0x7405c)?; // IOCTL_DISK_GET_LENGTH_INFO
    if number.kind != 7 || number.partition != 0 || number.disk > 31 || length != 1024 * MIB {
        return Err("Guest disk identity/type/partition/1 GiB size invariant failed".into());
    }
    println!(
        "Verified disposable target: {} => disk {}, {} bytes",
        disk.instance_id, number.disk, length
    );
    Ok(number.disk)
}
pub(crate) fn system_tool(name: &str) -> Result<PathBuf> {
    let mut path = [0_u16; 512];
    let length = unsafe { GetSystemDirectoryW(path.as_mut_ptr(), path.len() as u32) } as usize;
    if length == 0 || length >= path.len() {
        return Err("Cannot locate guest system tools".into());
    }
    Ok(PathBuf::from(OsString::from_wide(&path[..length])).join(name))
}
pub fn prepare() -> Result<()> {
    prepare_expected(0)
}
pub(crate) fn prepare_expected(expected_mode: u32) -> Result<()> {
    require_test_signing()?;
    let disk = target_number()?;
    let letters = unsafe { GetLogicalDrives() };
    if letters == 0 || letters & (1 << (b'R' - b'A')) != 0 {
        return Err("Guest R: must be unused before fixture setup".into());
    }
    let script = std::env::temp_dir().join(format!("svram-diskpart-{}.txt", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&script)
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    write!(file, "select disk {disk}\r\ndetail disk\r\nclean\r\nconvert gpt\r\ncreate partition primary\r\nformat fs=ntfs quick label=SVRAMLAB\r\nassign letter=R\r\nexit\r\n").map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    let result = std::process::Command::new(system_tool("diskpart.exe")?)
        .arg("/s")
        .arg(&script)
        .status()
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&script); // Only our create_new temporary file.
    if !result?.success() {
        return Err("Guest DiskPart failed".into());
    }
    let volume = OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(r"\\.\R:")
        .map_err(|e| e.to_string())?;
    let extents: Extents = query(&volume, 0x560000)?; // IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS
    if extents.count != 1
        || extents.disk != disk
        || extents.offset == 0
        || extents.length > 1024 * MIB
    {
        return Err("R: does not map exclusively to the verified disposable disk".into());
    }
    drop(volume);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = PathBuf::from(format!(r"R:\svram-{unique}.svram-lab"));
    let mut origin = LabOrigin::create_new(&path, MIB)?;
    for index in 0..256 {
        origin.write(index * PAGE as u64, &[index as u8; PAGE])?;
    }
    origin.flush()?;
    for index in 0..256 {
        let page = index * 73 % 256;
        let mut read = [0; PAGE];
        origin.read(page * PAGE as u64, &mut read)?;
        if read != [page as u8; PAGE] {
            return Err(format!("Bypass integrity failed on page {page}"));
        }
    }
    drop(origin);
    if path.exists() {
        return Err("Owned guest smoke file was not removed".into());
    }
    println!("SVRAM_BYPASS_STORAGE_INTEGRITY_VERIFIED: 256 direct 4 KiB writes + reads");
    let status = std::process::Command::new(system_tool("wpeutil.exe")?)
        .args(["CreatePageFile", r"/path=R:\pagefile.sys", "/size=256"])
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("Guest pagefile creation failed".into());
    }
    let interfaces = crate::device_interfaces::filters()?;
    if interfaces.len() != 1 {
        return Err("Private filter count changed during guest storage test".into());
    }
    let status = crate::driver_broker::query(&interfaces[0].path)?;
    if status.mode != expected_mode
        || status.paging_paths <= 0
        || status.writes < 256
        || status.reads < 256
    {
        return Err(format!(
            "Bypass/pagefile counters did not prove the expected operations: {status:?}"
        ));
    }
    println!("Live query after pagefile creation: {status:?}");
    println!(
        "SVRAM_PAGEFILE_PATH_VERIFIED (notification only; paging read hits not yet exercised)"
    );
    Ok(())
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetSystemDirectoryW(path: *mut u16, size: u32) -> u32;
    fn GetLogicalDrives() -> u32;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn storage_wire_sizes_match_sdk() {
        assert_eq!(std::mem::size_of::<DeviceNumber>(), 12);
        assert_eq!(std::mem::size_of::<Extents>(), 32);
        assert_eq!(std::mem::offset_of!(Extents, offset), 16);
    }
}
