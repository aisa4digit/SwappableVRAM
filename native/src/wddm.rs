//! Read-only process-local WDDM budgets. CUDA and DXGI adapters are joined by
//! LUID, not enumeration order. No D3D device, reservation, or global tweak.
use crate::store::Result;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
    DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3,
    IDXGIFactory1,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{Interface, PCWSTR};

#[derive(Clone, Copy, Debug)]
pub struct MemoryFootprint {
    pub available_physical: u64,
    pub available_commit: u64,
    pub process_working_set: u64,
    pub process_private_commit: u64,
}
pub fn memory_footprint() -> Result<MemoryFootprint> {
    let mut memory = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    unsafe {
        GlobalMemoryStatusEx(&mut memory).map_err(|e| format!("Read system memory: {e}"))?;
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut process as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            process.cb,
        )
        .map_err(|e| format!("Read broker memory cost: {e}"))?;
    }
    Ok(MemoryFootprint {
        available_physical: memory.ullAvailPhys,
        available_commit: memory.ullAvailPageFile,
        process_working_set: process.WorkingSetSize as u64,
        process_private_commit: process.PrivateUsage as u64,
    })
}

#[derive(Clone, Copy, Debug)]
pub struct SegmentBudget {
    pub budget: u64,
    pub usage: u64,
    pub reservation: u64,
    /// A reservation allowance, never a global free-VRAM reading.
    pub available_for_reservation: u64,
}
impl SegmentBudget {
    pub fn allowance(self) -> u64 {
        self.budget.saturating_sub(self.usage)
    }
}
impl From<DXGI_QUERY_VIDEO_MEMORY_INFO> for SegmentBudget {
    fn from(value: DXGI_QUERY_VIDEO_MEMORY_INFO) -> Self {
        Self {
            budget: value.Budget,
            usage: value.CurrentUsage,
            reservation: value.CurrentReservation,
            available_for_reservation: value.AvailableForReservation,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct BudgetSample {
    pub local: SegmentBudget,
    pub non_local: SegmentBudget,
}
pub struct BudgetMonitor {
    adapter: IDXGIAdapter3,
    event: OwnedHandle,
    cookie: u32,
    pub luid: u64,
    pub node: u32,
}
impl BudgetMonitor {
    pub fn for_cuda(ordinal: i32) -> Result<Self> {
        let (bytes, mask) = crate::cuda::device_luid(ordinal)?;
        let wanted = u64::from_le_bytes(bytes);
        let node = mask.trailing_zeros(); // device_luid rejects non-single-bit masks.
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.map_err(|e| format!("Create DXGI factory: {e}"))?;
        for index in 0..64 {
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(format!("Enumerate DXGI adapter: {error}")),
            };
            let desc = unsafe { adapter.GetDesc1() }
                .map_err(|e| format!("Read DXGI adapter description: {e}"))?;
            let found = u64::from(desc.AdapterLuid.LowPart)
                | (u64::from(desc.AdapterLuid.HighPart as u32) << 32);
            if found != wanted {
                continue;
            }
            let adapter: IDXGIAdapter3 = adapter
                .cast()
                .map_err(|e| format!("DXGI 1.4 budget interface unavailable: {e}"))?;
            let raw = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
                .map_err(|e| format!("Create budget event: {e}"))?;
            let event = unsafe { OwnedHandle::from_raw_handle(raw.0) };
            let cookie = unsafe { adapter.RegisterVideoMemoryBudgetChangeNotificationEvent(raw) }
                .map_err(|e| format!("Register budget notification: {e}"))?;
            let monitor = Self {
                adapter,
                event,
                cookie,
                luid: wanted,
                node,
            };
            monitor.sample()?; // Construction fails closed if either segment cannot be queried.
            return Ok(monitor);
        }
        Err("CUDA LUID did not match a DXGI adapter; budget admission unavailable".into())
    }
    pub fn sample(&self) -> Result<BudgetSample> {
        let mut local = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        let mut non_local = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        unsafe {
            self.adapter
                .QueryVideoMemoryInfo(self.node, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut local)
                .map_err(|e| format!("Query local WDDM budget: {e}"))?;
            self.adapter
                .QueryVideoMemoryInfo(
                    self.node,
                    DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL,
                    &mut non_local,
                )
                .map_err(|e| format!("Query non-local WDDM budget: {e}"))?;
        }
        Ok(BudgetSample {
            local: local.into(),
            non_local: non_local.into(),
        })
    }
    pub fn wait_changed(&self, milliseconds: u32) -> Result<bool> {
        if milliseconds > 10_000 {
            return Err("Budget wait is limited to 10 seconds".into());
        }
        match unsafe { WaitForSingleObject(HANDLE(self.event.as_raw_handle()), milliseconds) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(format!(
                "Wait for budget event: {}",
                std::io::Error::last_os_error()
            )),
        }
    }
}
impl Drop for BudgetMonitor {
    fn drop(&mut self) {
        // Unregister while both the COM adapter and event are still alive.
        unsafe {
            self.adapter
                .UnregisterVideoMemoryBudgetChangeNotification(self.cookie)
        };
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn over_budget_usage_is_not_unsigned_free_memory() {
        let sample = SegmentBudget {
            budget: 64,
            usage: 128,
            reservation: 0,
            available_for_reservation: 999,
        };
        assert_eq!(sample.allowance(), 0);
        assert_eq!(
            SegmentBudget {
                usage: 32,
                ..sample
            }
            .allowance(),
            32
        );
    }
}
