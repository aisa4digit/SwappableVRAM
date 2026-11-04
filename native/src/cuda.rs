//! NVIDIA Driver API loaded only from System32. No CUDA Toolkit or managed memory.
use std::ffi::{CStr, c_char, c_void};
use std::sync::{Arc, OnceLock};

type Result<T> = std::result::Result<T, String>;
type CuResult = i32;
type Context = *mut c_void;
type Stream = *mut c_void;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn GetLastError() -> u32;
}

pub struct Cuda {
    pub init: unsafe extern "system" fn(u32) -> CuResult,
    pub device_count: unsafe extern "system" fn(*mut i32) -> CuResult,
    pub device_get: unsafe extern "system" fn(*mut i32, i32) -> CuResult,
    pub device_name: unsafe extern "system" fn(*mut c_char, i32, i32) -> CuResult,
    pub device_pci: unsafe extern "system" fn(*mut c_char, i32, i32) -> CuResult,
    pub device_luid: Option<unsafe extern "system" fn(*mut c_char, *mut u32, i32) -> CuResult>,
    pub device_memory: unsafe extern "system" fn(*mut usize, i32) -> CuResult,
    pub driver_version: unsafe extern "system" fn(*mut i32) -> CuResult,
    pub context_create: unsafe extern "system" fn(*mut Context, u32, i32) -> CuResult,
    pub context_destroy: unsafe extern "system" fn(Context) -> CuResult,
    pub context_set: unsafe extern "system" fn(Context) -> CuResult,
    pub memory_info: unsafe extern "system" fn(*mut usize, *mut usize) -> CuResult,
    pub memory_alloc: unsafe extern "system" fn(*mut u64, usize) -> CuResult,
    pub memory_free: unsafe extern "system" fn(u64) -> CuResult,
    pub host_alloc: unsafe extern "system" fn(*mut *mut c_void, usize) -> CuResult,
    pub host_free: unsafe extern "system" fn(*mut c_void) -> CuResult,
    pub memory_set_async: unsafe extern "system" fn(u64, u8, usize, Stream) -> CuResult,
    pub stream_create: unsafe extern "system" fn(*mut Stream, u32) -> CuResult,
    pub stream_destroy: unsafe extern "system" fn(Stream) -> CuResult,
    pub stream_sync: unsafe extern "system" fn(Stream) -> CuResult,
    pub copy_to_device: unsafe extern "system" fn(u64, *const c_void, usize, Stream) -> CuResult,
    pub copy_to_host: unsafe extern "system" fn(*mut c_void, u64, usize, Stream) -> CuResult,
    pub error_string: unsafe extern "system" fn(CuResult, *mut *const c_char) -> CuResult,
}

impl Cuda {
    pub fn load() -> Result<Arc<Self>> {
        static DRIVER: OnceLock<Result<Arc<Cuda>>> = OnceLock::new();
        DRIVER.get_or_init(Self::load_once).clone()
    }

    fn load_once() -> Result<Arc<Self>> {
        let name: Vec<u16> = "nvcuda.dll\0".encode_utf16().collect();
        // SAFETY: NUL-terminated UTF-16 and SYSTEM32-only resolution. The module
        // intentionally stays loaded for the process lifetime (function pointers).
        let module = unsafe { LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), 0x800) };
        if module.is_null() {
            return Err(format!(
                "NVIDIA nvcuda.dll not found in System32 (Win32 {})",
                unsafe { GetLastError() }
            ));
        }
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {{
                // SAFETY: NVIDIA documents these exact ABI signatures. Each
                // pointer is checked before converting it to its concrete type.
                let ptr = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) };
                if ptr.is_null() {
                    return Err(format!("CUDA symbol missing: {}", $name));
                }
                unsafe { std::mem::transmute::<*mut c_void, $ty>(ptr) }
            }};
        }
        let cuda = Self {
            init: symbol!("cuInit", unsafe extern "system" fn(u32) -> i32),
            device_count: symbol!(
                "cuDeviceGetCount",
                unsafe extern "system" fn(*mut i32) -> i32
            ),
            device_get: symbol!(
                "cuDeviceGet",
                unsafe extern "system" fn(*mut i32, i32) -> i32
            ),
            device_name: symbol!(
                "cuDeviceGetName",
                unsafe extern "system" fn(*mut c_char, i32, i32) -> i32
            ),
            device_pci: symbol!(
                "cuDeviceGetPCIBusId",
                unsafe extern "system" fn(*mut c_char, i32, i32) -> i32
            ),
            device_luid: {
                let pointer = unsafe { GetProcAddress(module, c"cuDeviceGetLuid".as_ptr().cast()) };
                if pointer.is_null() {
                    None
                } else {
                    Some(unsafe {
                        std::mem::transmute::<
                            *mut c_void,
                            unsafe extern "system" fn(*mut c_char, *mut u32, i32) -> CuResult,
                        >(pointer)
                    })
                }
            },
            device_memory: symbol!(
                "cuDeviceTotalMem_v2",
                unsafe extern "system" fn(*mut usize, i32) -> i32
            ),
            driver_version: symbol!(
                "cuDriverGetVersion",
                unsafe extern "system" fn(*mut i32) -> i32
            ),
            context_create: symbol!(
                "cuCtxCreate_v2",
                unsafe extern "system" fn(*mut Context, u32, i32) -> i32
            ),
            context_destroy: symbol!("cuCtxDestroy_v2", unsafe extern "system" fn(Context) -> i32),
            context_set: symbol!("cuCtxSetCurrent", unsafe extern "system" fn(Context) -> i32),
            memory_info: symbol!(
                "cuMemGetInfo_v2",
                unsafe extern "system" fn(*mut usize, *mut usize) -> i32
            ),
            memory_alloc: symbol!(
                "cuMemAlloc_v2",
                unsafe extern "system" fn(*mut u64, usize) -> i32
            ),
            memory_free: symbol!("cuMemFree_v2", unsafe extern "system" fn(u64) -> i32),
            host_alloc: symbol!(
                "cuMemAllocHost_v2",
                unsafe extern "system" fn(*mut *mut c_void, usize) -> i32
            ),
            host_free: symbol!(
                "cuMemFreeHost",
                unsafe extern "system" fn(*mut c_void) -> i32
            ),
            memory_set_async: symbol!(
                "cuMemsetD8Async",
                unsafe extern "system" fn(u64, u8, usize, Stream) -> i32
            ),
            stream_create: symbol!(
                "cuStreamCreate",
                unsafe extern "system" fn(*mut Stream, u32) -> i32
            ),
            stream_destroy: symbol!(
                "cuStreamDestroy_v2",
                unsafe extern "system" fn(Stream) -> i32
            ),
            stream_sync: symbol!(
                "cuStreamSynchronize",
                unsafe extern "system" fn(Stream) -> i32
            ),
            copy_to_device: symbol!(
                "cuMemcpyHtoDAsync_v2",
                unsafe extern "system" fn(u64, *const c_void, usize, Stream) -> i32
            ),
            copy_to_host: symbol!(
                "cuMemcpyDtoHAsync_v2",
                unsafe extern "system" fn(*mut c_void, u64, usize, Stream) -> i32
            ),
            error_string: symbol!(
                "cuGetErrorString",
                unsafe extern "system" fn(i32, *mut *const c_char) -> i32
            ),
        };
        cuda.check(unsafe { (cuda.init)(0) }, "cuInit")?;
        Ok(Arc::new(cuda))
    }

    pub fn check(&self, code: i32, operation: &str) -> Result<()> {
        if code == 0 {
            return Ok(());
        }
        let mut message = std::ptr::null();
        // SAFETY: driver-owned static error string; no free is required.
        unsafe {
            (self.error_string)(code, &mut message);
        }
        let detail = if message.is_null() {
            "unknown CUDA error".into()
        } else {
            unsafe { CStr::from_ptr(message) }.to_string_lossy()
        };
        Err(format!("{operation}: CUDA {code}: {detail}"))
    }
}

#[repr(C)]
pub struct DeviceInfo {
    pub ordinal: i32,
    pub driver_version: i32,
    pub total_bytes: u64,
    pub name: [u8; 256],
    pub pci_bus_id: [u8; 32],
}

pub fn device_count() -> Result<i32> {
    let cuda = Cuda::load()?;
    let mut count = 0;
    cuda.check(
        unsafe { (cuda.device_count)(&mut count) },
        "cuDeviceGetCount",
    )?;
    Ok(count)
}
pub fn device_luid(ordinal: i32) -> Result<([u8; 8], u32)> {
    let cuda = Cuda::load()?;
    let mut device = 0;
    cuda.check(
        unsafe { (cuda.device_get)(&mut device, ordinal) },
        "cuDeviceGet",
    )?;
    let function = cuda
        .device_luid
        .ok_or("CUDA does not provide a WDDM LUID; budget admission unavailable")?;
    let mut luid = [0_u8; 8];
    let mut mask = 0;
    cuda.check(
        unsafe { function(luid.as_mut_ptr().cast(), &mut mask, device) },
        "cuDeviceGetLuid",
    )?;
    if mask.count_ones() != 1 {
        return Err(
            "Single-node WDDM adapter required; CUDA node mask is absent or ambiguous".into(),
        );
    }
    Ok((luid, mask))
}
pub fn device_info(ordinal: i32) -> Result<DeviceInfo> {
    let cuda = Cuda::load()?;
    let mut device = 0;
    cuda.check(
        unsafe { (cuda.device_get)(&mut device, ordinal) },
        "cuDeviceGet",
    )?;
    let mut info = DeviceInfo {
        ordinal,
        driver_version: 0,
        total_bytes: 0,
        name: [0; 256],
        pci_bus_id: [0; 32],
    };
    let mut bytes = 0;
    cuda.check(
        unsafe { (cuda.device_memory)(&mut bytes, device) },
        "cuDeviceTotalMem",
    )?;
    info.total_bytes = bytes as u64;
    cuda.check(
        unsafe { (cuda.driver_version)(&mut info.driver_version) },
        "cuDriverGetVersion",
    )?;
    cuda.check(
        unsafe { (cuda.device_name)(info.name.as_mut_ptr().cast(), 256, device) },
        "cuDeviceGetName",
    )?;
    cuda.check(
        unsafe { (cuda.device_pci)(info.pci_bus_id.as_mut_ptr().cast(), 32, device) },
        "cuDeviceGetPCIBusId",
    )?;
    Ok(info)
}
