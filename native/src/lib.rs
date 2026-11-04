//! Windows VRAM engine. All timing-sensitive operations stay in Rust.
//! C ABI is restricted to trusted in-process callers; invalid foreign pointers
//! cannot be recovered by catch_unwind. The .NET bridge owns SafeHandles.
pub mod broker_host;
mod broker_queue;
pub mod cache;
pub mod control_protocol;
pub mod cuda;
pub mod device_interfaces;
pub mod driver_broker;
pub mod guest_anonymous;
pub mod guest_cache;
pub mod guest_disk;
pub mod guest_lab;
pub mod ipc_benchmark;
pub mod lab_origin;
mod overlapped_io;
pub mod overlapped_smoke;
pub mod page_cache;
pub mod paging_protocol;
pub mod policy;
mod process_identity;
pub mod range_benchmark;
mod read_budget;
pub mod store;
pub mod transport_lab;
#[cfg(feature = "wddm")]
pub mod wddm;

use cuda::DeviceInfo;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use store::VramStore;

thread_local! { static ERROR: RefCell<String> = const { RefCell::new(String::new()) }; }
fn boundary(f: impl FnOnce() -> Result<(), String>) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => 0,
        Ok(Err(message)) => {
            ERROR.with(|e| *e.borrow_mut() = message);
            -1
        }
        Err(_) => {
            ERROR.with(|e| *e.borrow_mut() = "Rust engine panic contained at ABI boundary".into());
            -2
        }
    }
}
fn valid<T>(pointer: *const T) -> Result<(), String> {
    if pointer.is_null() {
        Err("Null FFI pointer".into())
    } else {
        Ok(())
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn sv_api_version() -> u32 {
    1
}
/// Read-only host prerequisites. No boot, driver, or process-memory changes.
/// # Safety
/// Both outputs must be aligned writable u32 storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_host_status(options: *mut u32, interfaces: *mut u32) -> i32 {
    boundary(|| {
        valid(options)?;
        valid(interfaces)?;
        unsafe {
            *options = 0;
            *interfaces = 0;
        }
        let runtime = guest_lab::code_integrity_options()?;
        let count =
            u32::try_from(device_interfaces::filters()?.len()).map_err(|e| e.to_string())?;
        unsafe {
            *options = runtime;
            *interfaces = count;
        }
        Ok(())
    })
}

/// # Safety
/// `buffer` must point to at least `length` writable bytes, or be null for size query.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_last_error(buffer: *mut u8, length: usize) -> usize {
    ERROR.with(|e| {
        let message = e.borrow();
        if !buffer.is_null() && length > 0 {
            let count = message.len().min(length - 1);
            unsafe {
                std::ptr::copy_nonoverlapping(message.as_ptr(), buffer, count);
                *buffer.add(count) = 0;
            }
        }
        message.len() + 1
    })
}
/// # Safety
/// `count` must be a writable i32.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_device_count(count: *mut i32) -> i32 {
    boundary(|| {
        valid(count)?;
        unsafe {
            *count = cuda::device_count()?;
        }
        Ok(())
    })
}
/// # Safety
/// `info` must be aligned writable storage for a DeviceInfo.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_device_info(ordinal: i32, info: *mut DeviceInfo) -> i32 {
    boundary(|| {
        valid(info)?;
        unsafe {
            *info = cuda::device_info(ordinal)?;
        }
        Ok(())
    })
}
/// # Safety
/// `output` must point to writable pointer storage. Caller owns the resulting
/// handle and must destroy it exactly once, after all concurrent calls finish.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_store_create(
    ordinal: i32,
    capacity: u64,
    output: *mut *mut VramStore,
) -> i32 {
    boundary(|| {
        valid(output)?;
        unsafe {
            *output = std::ptr::null_mut();
        }
        let store = Box::new(VramStore::new(ordinal, capacity)?);
        unsafe {
            *output = Box::into_raw(store);
        }
        Ok(())
    })
}
/// # Safety
/// `handle` must be a live engine handle; output pointers must be writable u64s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_store_info(
    handle: *const VramStore,
    free: *mut u64,
    total: *mut u64,
) -> i32 {
    boundary(|| {
        valid(handle)?;
        valid(free)?;
        valid(total)?;
        unsafe {
            *free = (*handle).free_before;
            *total = (*handle).device_total;
        }
        Ok(())
    })
}
/// # Safety
/// `handle` must be the unique live pointer returned by create; no calls may
/// race with destroy. A null handle is a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_store_destroy(handle: *mut VramStore) -> i32 {
    boundary(|| {
        if !handle.is_null() {
            drop(unsafe { Box::from_raw(handle) });
        }
        Ok(())
    })
}
/// # Safety
/// Handle must remain alive; buffer must hold `length` writable bytes and may
/// not alias the engine's internal allocations or any simultaneous write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_store_read(
    handle: *const VramStore,
    offset: u64,
    buffer: *mut u8,
    length: usize,
) -> i32 {
    boundary(|| {
        valid(handle)?;
        let store = unsafe { &*handle };
        store::validate_range(offset, length as u64, store.capacity)?;
        if length == 0 {
            return Ok(());
        }
        valid(buffer)?;
        store.read(offset, unsafe {
            std::slice::from_raw_parts_mut(buffer, length)
        })
    })
}
/// # Safety
/// Handle must remain alive; buffer must hold `length` readable bytes for the
/// duration of this call and must not alias engine-owned memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_store_write(
    handle: *const VramStore,
    offset: u64,
    buffer: *const u8,
    length: usize,
) -> i32 {
    boundary(|| {
        valid(handle)?;
        let store = unsafe { &*handle };
        store::validate_range(offset, length as u64, store.capacity)?;
        if length == 0 {
            return Ok(());
        }
        valid(buffer)?;
        store.write(offset, unsafe {
            std::slice::from_raw_parts(buffer, length)
        })
    })
}
/// # Safety
/// Handle must remain alive; samples must point to `count` exclusive writable
/// f64 elements. Only one benchmark may run per handle at a time.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_measure(
    handle: *const VramStore,
    write: i32,
    gap_ms: u32,
    samples: *mut f64,
    count: usize,
) -> i32 {
    boundary(|| {
        valid(handle)?;
        valid(samples)?;
        if !(100..=100_000).contains(&count) || !matches!(write, 0 | 1) {
            return Err("Invalid measurement request".into());
        }
        unsafe { &*handle }.measure(write == 1, gap_ms, unsafe {
            std::slice::from_raw_parts_mut(samples, count)
        })
    })
}
/// # Safety
/// Handle must remain alive for this call; may run concurrently with measure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_cancel(handle: *const VramStore) -> i32 {
    boundary(|| {
        valid(handle)?;
        unsafe { &*handle }.cancel();
        Ok(())
    })
}
/// # Safety
/// Handle must remain alive; caller must serialize reset with benchmark starts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_reset_cancel(handle: *const VramStore) -> i32 {
    boundary(|| {
        valid(handle)?;
        unsafe { &*handle }.reset_cancel();
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn null_ffi_is_contained() {
        assert_eq!(unsafe { sv_device_count(std::ptr::null_mut()) }, -1);
        let mut message = [0u8; 128];
        let size = unsafe { sv_last_error(message.as_mut_ptr(), message.len()) };
        assert!(size > 1);
        assert!(String::from_utf8_lossy(&message).contains("Null FFI pointer"));
    }
    #[test]
    fn panic_does_not_cross_abi() {
        assert_eq!(boundary(|| panic!("test")), -2);
    }
}
