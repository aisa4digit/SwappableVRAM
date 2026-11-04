//! Bounded anonymous-memory integrity workload for the private WinPE guest.
//! Only this process's working-set limits change. This is not an FPS test.
use crate::store::{MIB, Result};
use std::ffi::c_void;
use std::time::{Duration, Instant};
const PROCESS: *mut c_void = (-1_isize) as *mut c_void;
#[repr(C)]
#[derive(Default, Debug)]
struct Counters {
    size: u32,
    faults: u32,
    values: [usize; 9],
}
fn counters() -> Result<Counters> {
    let mut result = Counters {
        size: std::mem::size_of::<Counters>() as u32,
        ..Counters::default()
    };
    if unsafe { K32GetProcessMemoryInfo(PROCESS, &mut result, result.size) } == 0 {
        return Err(format!(
            "Read own memory counters: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(result)
}
struct Workload {
    pointer: *mut u64,
    pressure: *mut u64,
    old_min: usize,
    old_max: usize,
    old_flags: u32,
}
impl Drop for Workload {
    fn drop(&mut self) {
        unsafe {
            if !self.pointer.is_null() {
                VirtualFree(self.pointer.cast(), 0, 0x8000);
            }
            if !self.pressure.is_null() {
                VirtualFree(self.pressure.cast(), 0, 0x8000);
            }
            SetProcessWorkingSetSizeEx(PROCESS, self.old_min, self.old_max, self.old_flags);
        }
    }
}
fn word(page: usize, offset: usize) -> u64 {
    (page as u64).wrapping_mul(0x9e3779b97f4a7c15)
        ^ (offset as u64).wrapping_mul(0xd6e8feb86659fd93)
}
pub fn run(interface: &str) -> Result<()> {
    crate::guest_lab::require_test_signing()?;
    let endpoint = crate::device_interfaces::wait_lab_filter()?;
    if !endpoint.path.eq_ignore_ascii_case(interface) {
        return Err("Anonymous workload target changed".into());
    }
    let before = crate::driver_broker::query(interface)?;
    if before.mode != 2 || before.paging_paths <= 0 {
        return Err("Anonymous test requires an active isolated cache and pagefile path".into());
    }
    let mut workload = Workload {
        pointer: std::ptr::null_mut(),
        pressure: std::ptr::null_mut(),
        old_min: 0,
        old_max: 0,
        old_flags: 0,
    };
    if unsafe {
        GetProcessWorkingSetSizeEx(
            PROCESS,
            &mut workload.old_min,
            &mut workload.old_max,
            &mut workload.old_flags,
        )
    } == 0
    {
        // No quota was changed, so avoid restoring an unread value.
        std::mem::forget(workload);
        return Err(format!(
            "Read own working-set limits: {}",
            std::io::Error::last_os_error()
        ));
    }
    if unsafe { SetProcessWorkingSetSizeEx(PROCESS, 16 * MIB as usize, 32 * MIB as usize, 4) } == 0
    {
        return Err(format!(
            "Set own 32 MiB hard working-set maximum: {}",
            std::io::Error::last_os_error()
        ));
    }
    let length = 192 * MIB as usize;
    workload.pointer = unsafe { VirtualAlloc(std::ptr::null_mut(), length, 0x3000, 4) }.cast();
    if workload.pointer.is_null() {
        return Err(format!(
            "Commit own anonymous pages: {}",
            std::io::Error::last_os_error()
        ));
    }
    let started = Instant::now();
    let initial = counters()?;
    let pages = length / 4096;
    for page in 0..pages {
        if page % 256 == 0 && started.elapsed() > Duration::from_secs(45) {
            return Err("Anonymous write workload deadline".into());
        }
        for offset in 0..512 {
            unsafe {
                workload
                    .pointer
                    .add(page * 512 + offset)
                    .write_volatile(word(page, offset));
            }
        }
    }
    // A hard WS cap alone leaves modified pages physically resident. Allocate
    // and dirty an additional bounded buffer in this same disposable process
    // to require reuse of those frames; never purge another process or host.
    let mut memory = MemoryStatus {
        length: 64,
        ..MemoryStatus::default()
    };
    if unsafe { GlobalMemoryStatusEx(&mut memory) } == 0 {
        return Err(format!(
            "Read guest available memory: {}",
            std::io::Error::last_os_error()
        ));
    }
    let pressure_bytes = (memory.available_physical + 64 * MIB).div_ceil(4096) * 4096;
    if pressure_bytes > 1024 * MIB || memory.available_commit < pressure_bytes + 64 * MIB {
        return Err(format!(
            "Guest pressure cannot fit the 1 GiB / 64 MiB commit-headroom bounds: {memory:?}"
        ));
    }
    workload.pressure =
        unsafe { VirtualAlloc(std::ptr::null_mut(), pressure_bytes as usize, 0x3000, 4) }.cast();
    if workload.pressure.is_null() {
        return Err(format!(
            "Guest pressure allocation: {}",
            std::io::Error::last_os_error()
        ));
    }
    println!(
        "Guest-only additional anonymous pressure: {} MiB; available physical {} MiB, available commit {} MiB",
        pressure_bytes / MIB,
        memory.available_physical / MIB,
        memory.available_commit / MIB
    );
    for page in 0..pressure_bytes as usize / 4096 {
        if page % 256 == 0 && started.elapsed() > Duration::from_secs(45) {
            return Err("Guest pressure workload deadline".into());
        }
        unsafe {
            workload
                .pressure
                .add(page * 512)
                .write_volatile(word(page, 23));
        }
    }
    let written = counters()?;
    let after_write = crate::driver_broker::query(interface)?;
    for index in 0..pages {
        if index % 256 == 0 && started.elapsed() > Duration::from_secs(45) {
            return Err("Anonymous read workload deadline".into());
        }
        let page = index * 257 % pages;
        for offset in 0..512 {
            if unsafe { workload.pointer.add(page * 512 + offset).read_volatile() }
                != word(page, offset)
            {
                return Err(format!("Anonymous page corruption at {page}:{offset}"));
            }
        }
    }
    let read = counters()?;
    let after_read = crate::driver_broker::query(interface)?;
    println!(
        "Anonymous 192 MiB / own hard WS max 32 MiB: write faults={}, read faults={}, read WS={} bytes, elapsed={} ms",
        written.faults.wrapping_sub(initial.faults),
        read.faults.wrapping_sub(written.faults),
        read.values[1],
        started.elapsed().as_millis()
    );
    println!(
        "Anonymous workload driver snapshots: before={before:?}; written={after_write:?}; read={after_read:?}"
    );
    if after_read.paging_reads <= after_write.paging_reads {
        return Err("Anonymous integrity passed but no additional lower paging reads were observed; offload not proven".into());
    }
    println!(
        "SVRAM_ANONYMOUS_INTEGRITY_VERIFIED (controlled private-memory workload + correlated paging I/O; RAM fixture, no GPU/FPS claim)"
    );
    Ok(())
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
    fn GetProcessWorkingSetSizeEx(
        process: *mut c_void,
        min: *mut usize,
        max: *mut usize,
        flags: *mut u32,
    ) -> i32;
    fn SetProcessWorkingSetSizeEx(process: *mut c_void, min: usize, max: usize, flags: u32) -> i32;
    fn VirtualAlloc(address: *mut c_void, size: usize, kind: u32, protection: u32) -> *mut c_void;
    fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> i32;
    fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut Counters, size: u32) -> i32;
}
#[repr(C)]
#[derive(Default, Debug)]
struct MemoryStatus {
    length: u32,
    load: u32,
    total_physical: u64,
    available_physical: u64,
    total_commit: u64,
    available_commit: u64,
    total_virtual: u64,
    available_virtual: u64,
    extended_virtual: u64,
}
