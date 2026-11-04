//! RAII-owned VRAM and a bounded 4 MiB page-locked host staging area.
use crate::cuda::Cuda;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const MIB: u64 = 1024 * 1024;
pub const TRANSFER_BYTES: usize = 4 * 1024 * 1024;
pub type Result<T> = std::result::Result<T, String>;

pub fn validate_range(offset: u64, length: u64, capacity: u64) -> Result<()> {
    if offset > capacity || length > capacity - offset {
        Err("VRAM request out of bounds".into())
    } else {
        Ok(())
    }
}

pub struct VramStore {
    pub capacity: u64,
    pub free_before: u64,
    pub device_total: u64,
    cancelled: AtomicBool,
    inner: Mutex<Allocation>,
}
struct Allocation {
    cuda: Arc<Cuda>,
    context: *mut c_void,
    stream: *mut c_void,
    staging: *mut c_void,
    pointer: u64,
}
// SAFETY: all use (including drop) is exclusive via the store mutex. Every
// operation rebinds the CUDA context on its calling thread before accessing it.
unsafe impl Send for Allocation {}

impl VramStore {
    pub fn new(ordinal: i32, capacity: u64) -> Result<Self> {
        if !(16 * MIB..=8192 * MIB).contains(&capacity) || !capacity.is_multiple_of(4096) {
            return Err("Capacity must be 16–8192 MiB, aligned to 4096 bytes".into());
        }
        let cuda = Cuda::load()?;
        let mut device = 0;
        cuda.check(
            unsafe { (cuda.device_get)(&mut device, ordinal) },
            "cuDeviceGet",
        )?;
        let mut allocation = Allocation {
            cuda: cuda.clone(),
            context: std::ptr::null_mut(),
            stream: std::ptr::null_mut(),
            staging: std::ptr::null_mut(),
            pointer: 0,
        };
        // Blocking sync avoids consuming a CPU core waiting for the GPU.
        cuda.check(
            unsafe { (cuda.context_create)(&mut allocation.context, 4, device) },
            "cuCtxCreate",
        )?;
        let (mut free, mut total) = (0, 0);
        cuda.check(
            unsafe { (cuda.memory_info)(&mut free, &mut total) },
            "cuMemGetInfo",
        )?;
        let reserve = (2048 * MIB).max(total as u64 / 5);
        if capacity > (free as u64).saturating_sub(reserve) {
            return Err(format!(
                "Insufficient GPU headroom; reserve {} MiB, free {} MiB",
                reserve / MIB,
                free as u64 / MIB
            ));
        }
        cuda.check(
            unsafe { (cuda.memory_alloc)(&mut allocation.pointer, capacity as usize) },
            "cuMemAlloc",
        )?;
        cuda.check(
            unsafe { (cuda.host_alloc)(&mut allocation.staging, TRANSFER_BYTES) },
            "cuMemAllocHost",
        )?;
        cuda.check(
            unsafe { (cuda.stream_create)(&mut allocation.stream, 1) },
            "cuStreamCreate",
        )?;
        cuda.check(
            unsafe {
                (cuda.memory_set_async)(allocation.pointer, 0, capacity as usize, allocation.stream)
            },
            "cuMemsetD8Async",
        )?;
        // Zeroing and transfers share one stream. A default-stream memset
        // could race writes on our NON_BLOCKING stream. Finish initialization
        // before publishing the allocation to any caller.
        cuda.check(
            unsafe { (cuda.stream_sync)(allocation.stream) },
            "cuStreamSynchronize/initialization",
        )?;
        Ok(Self {
            capacity,
            free_before: free as u64,
            device_total: total as u64,
            cancelled: AtomicBool::new(false),
            inner: Mutex::new(allocation),
        })
    }

    pub fn read(&self, offset: u64, destination: &mut [u8]) -> Result<()> {
        validate_range(offset, destination.len() as u64, self.capacity)?;
        let allocation = self.inner.lock().map_err(|_| "VRAM lock poisoned")?;
        let a = &*allocation;
        a.cuda.check(
            unsafe { (a.cuda.context_set)(a.context) },
            "cuCtxSetCurrent",
        )?;
        let mut position = offset;
        for chunk in destination.chunks_mut(TRANSFER_BYTES) {
            a.cuda.check(
                unsafe {
                    (a.cuda.copy_to_host)(a.staging, a.pointer + position, chunk.len(), a.stream)
                },
                "cuMemcpyDtoHAsync",
            )?;
            a.cuda.check(
                unsafe { (a.cuda.stream_sync)(a.stream) },
                "cuStreamSynchronize/read",
            )?;
            // SAFETY: staging has TRANSFER_BYTES capacity; GPU copy is complete.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    a.staging.cast::<u8>(),
                    chunk.as_mut_ptr(),
                    chunk.len(),
                );
            }
            position += chunk.len() as u64;
        }
        Ok(())
    }
    pub fn write(&self, offset: u64, source: &[u8]) -> Result<()> {
        validate_range(offset, source.len() as u64, self.capacity)?;
        let allocation = self.inner.lock().map_err(|_| "VRAM lock poisoned")?;
        let a = &*allocation;
        a.cuda.check(
            unsafe { (a.cuda.context_set)(a.context) },
            "cuCtxSetCurrent",
        )?;
        let mut position = offset;
        for chunk in source.chunks(TRANSFER_BYTES) {
            // SAFETY: source and driver-owned staging are disjoint, bounded.
            unsafe {
                std::ptr::copy_nonoverlapping(chunk.as_ptr(), a.staging.cast::<u8>(), chunk.len());
            }
            a.cuda.check(
                unsafe {
                    (a.cuda.copy_to_device)(a.pointer + position, a.staging, chunk.len(), a.stream)
                },
                "cuMemcpyHtoDAsync",
            )?;
            a.cuda.check(
                unsafe { (a.cuda.stream_sync)(a.stream) },
                "cuStreamSynchronize/write",
            )?;
            position += chunk.len() as u64;
        }
        Ok(())
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn reset_cancel(&self) {
        self.cancelled.store(false, Ordering::Release);
    }
    pub fn measure(&self, write: bool, idle_gap_ms: u32, samples: &mut [f64]) -> Result<()> {
        if !(100..=100_000).contains(&samples.len()) || idle_gap_ms > 100 {
            return Err("Invalid benchmark options".into());
        }
        let mut page = [0xA5; 4096];
        let mut random = 14041u64;
        for sample in samples {
            if self.cancelled.load(Ordering::Acquire) {
                return Err("CANCELLED".into());
            }
            if idle_gap_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(idle_gap_ms.into()));
            }
            if self.cancelled.load(Ordering::Acquire) {
                return Err("CANCELLED".into());
            }
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let offset = (random % (self.capacity / 4096)) * 4096;
            let start = std::time::Instant::now();
            if write {
                self.write(offset, &page)?;
            } else {
                self.read(offset, &mut page)?;
            }
            *sample = start.elapsed().as_secs_f64() * 1_000_000.0;
            // Paired benchmark: the preceding write run uses identical seeded
            // offsets and 0xA5 payload. Verify outside the timed read interval.
            if !write && page.iter().any(|byte| *byte != 0xA5) {
                return Err(format!("Measured VRAM read failed integrity at {offset}"));
            }
        }
        Ok(())
    }
}
impl Drop for Allocation {
    fn drop(&mut self) {
        // Best-effort per-resource cleanup followed by context destruction also
        // covers partial initialization and GPU reset paths.
        if self.context.is_null() {
            return;
        }
        unsafe {
            (self.cuda.context_set)(self.context);
            if !self.stream.is_null() {
                (self.cuda.stream_sync)(self.stream);
                (self.cuda.stream_destroy)(self.stream);
            }
            if !self.staging.is_null() {
                (self.cuda.host_free)(self.staging);
            }
            if self.pointer != 0 {
                (self.cuda.memory_free)(self.pointer);
            }
            (self.cuda.context_destroy)(self.context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_do_not_wrap() {
        assert!(validate_range(u64::MAX - 1, 10, u64::MAX).is_err());
        assert!(validate_range(9, 2, 10).is_err());
        assert!(validate_range(10, 0, 10).is_ok());
        assert!(validate_range(0, 10, 10).is_ok());
    }
    #[test]
    fn capacity_rejects_invalid_before_loading_driver() {
        assert!(VramStore::new(0, 0).is_err());
        assert!(VramStore::new(0, 16 * MIB + 1).is_err());
    }
}
