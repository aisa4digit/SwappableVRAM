//! Experimental KMDF broker client. Opens only the filter's private interface,
//! never a PhysicalDrive path. This command does not install a driver or create
//! a pagefile. It requires the separately built/loaded isolated-lab filter.
use crate::cache::{BlockIo, PAGE};
use crate::page_cache::PageCache;
use crate::paging_protocol::{self, Frame, HIT, MAX_BYTES, MISS, OP_FILL, OP_READ};
#[cfg(feature = "wddm")]
use crate::store::VramStore;
use crate::store::{MIB, Result};
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const INTERFACE: &str = "#{79320e2b-6b29-4df1-a827-869105412c47}";
const QUERY: u32 = (0x22 << 16) | (1 << 14) | (0x900 << 2);
const ENABLE: u32 = (0x22 << 16) | (3 << 14) | (0x901 << 2);
const GET: u32 = (0x22 << 16) | (3 << 14) | (0x902 << 2);
const REPLY: u32 = (0x22 << 16) | (3 << 14) | (0x903 << 2);
const OWNER_INFO: u32 = (0x22 << 16) | (1 << 14) | (0x904 << 2);
#[repr(C)]
#[derive(Default, Debug)]
pub struct OwnerInfo {
    pub size: u32,
    pub version: u32,
    pub wdf_file_present: u32,
    pub stack_file_present: u32,
    pub sideband_file_present: u32,
    pub requestor_pid: u32,
    pub control_id: u64,
}

#[repr(C)]
#[derive(Default, Debug)]
pub struct DriverStatus {
    pub size: u32,
    pub version: u32,
    pub mode: u32,
    pub paging_paths: i32,
    pub reads: u64,
    pub writes: u64,
    pub paging_reads: u64,
    pub candidates: u64,
    pub cache_hits: u64,
    pub issued_reads: u64,
    pub deadline_fallbacks: u64,
    pub reply_fallbacks: u64,
    pub cancelled_reads: u64,
    pub late_replies: u64,
    pub offered_fills: u64,
    pub cache_leases: i32,
    pub cache_d0_available: u32,
    pub quiesce_calls: u64,
    pub protocol_version: u32,
    pub maximum_transfer_bytes: u32,
}
#[repr(C)]
struct Enable {
    size: u32,
    version: u32,
    enabled: u32,
    reserved: u32,
}
#[repr(C)]
#[derive(Default)]
pub(crate) struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    pub(crate) event: *mut c_void,
}
impl Overlapped {
    pub(crate) fn with_event(event: *mut c_void) -> Self {
        Self {
            event,
            ..Self::default()
        }
    }
}
struct Connection {
    file: File,
    event: OwnedHandle,
    endpoint: String,
    control_gate: Mutex<()>,
}
impl Connection {
    fn open(path: &str) -> Result<Self> {
        if !valid_interface(path) {
            return Err("Only the SwappableVRAM private device interface is accepted".into());
        }
        let discovery = Self::open_endpoint(path)?;
        let identity = discovery.owner_info()?;
        if identity.control_id == 0 {
            return Err("Filter control endpoint is not ready".into());
        }
        let endpoint = format!(r"\\.\Global\SwappableVRAM-{:016X}", identity.control_id);
        let connection = Self::open_endpoint(&endpoint)?;
        if connection.owner_info()?.control_id != identity.control_id {
            return Err("Control endpoint target identity changed".into());
        }
        Ok(connection)
    }
    // Called only for a verified PnP discovery path or a name constructed from
    // its kernel-returned boot-local target ID, never a caller-supplied disk.
    fn open_endpoint(path: &str) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(3)
            .custom_flags(0x40000000)
            .open(path)
            .map_err(|e| format!("Open isolated-lab filter: {e}"))?;
        Self::from_file(file, path)
    }
    fn from_file(file: File, path: &str) -> Result<Self> {
        let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if raw.is_null() {
            return Err(format!("Create broker event: Win32 {}", unsafe {
                GetLastError()
            }));
        }
        Ok(Self {
            file,
            event: unsafe { OwnedHandle::from_raw_handle(raw) },
            endpoint: path.into(),
            control_gate: Mutex::new(()),
        })
    }
    // GET slots have independent events; synchronous-in-intent control calls
    // serialize their shared event even if a future caller shares Connection.
    fn control(
        &self,
        code: u32,
        input: &[u8],
        output: &mut [u8],
        timeout_ms: u32,
    ) -> std::result::Result<Option<u32>, u32> {
        let _guard = self.control_gate.lock().map_err(|_| 31u32)?;
        let event = self.event.as_raw_handle();
        if unsafe { ResetEvent(event) } == 0 {
            return Err(unsafe { GetLastError() });
        }
        let overlap = UnsafeCell::new(Overlapped::with_event(event));
        let file = self.file.as_raw_handle();
        let success = unsafe {
            DeviceIoControl(
                file,
                code,
                if input.is_empty() {
                    std::ptr::null()
                } else {
                    input.as_ptr().cast()
                },
                input.len() as u32,
                if output.is_empty() {
                    std::ptr::null_mut()
                } else {
                    output.as_mut_ptr().cast()
                },
                output.len() as u32,
                std::ptr::null_mut(),
                overlap.get(),
            )
        };
        if success != 0 {
            return unsafe { crate::overlapped_io::finish(&self.file, overlap.get()) }.map(Some);
        }
        let error = unsafe { GetLastError() };
        if error != 997 {
            return Err(error);
        }
        match unsafe { WaitForSingleObject(event, timeout_ms) } {
            0 => unsafe { crate::overlapped_io::finish(&self.file, overlap.get()) }.map(Some),
            258 => {
                unsafe {
                    CancelIoEx(file, overlap.get());
                }
                match unsafe { crate::overlapped_io::finish(&self.file, overlap.get()) } {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(995) => Ok(None),
                    Err(code) => Err(code),
                }
            }
            _ => {
                let error = unsafe { GetLastError() };
                unsafe {
                    CancelIoEx(file, overlap.get());
                }
                let _ = unsafe { crate::overlapped_io::finish(&self.file, overlap.get()) };
                Err(error)
            }
        }
    }
    fn query(&self) -> Result<DriverStatus> {
        let mut status = DriverStatus::default();
        let bytes = self
            .control(QUERY, &[], bytes_mut(&mut status), 1000)
            .map_err(|c| format!("Query filter: Win32 {c}"))?;
        if bytes != Some(std::mem::size_of::<DriverStatus>() as u32)
            || status.size != std::mem::size_of::<DriverStatus>() as u32
            || status.version != 4
            || status.protocol_version != paging_protocol::VERSION
            || status.maximum_transfer_bytes != MAX_BYTES as u32
        {
            return Err("KMDF query ABI mismatch".into());
        }
        Ok(status)
    }
    fn enable(&self, enabled: bool) -> Result<()> {
        let options = Enable {
            size: 16,
            version: paging_protocol::VERSION,
            enabled: u32::from(enabled),
            reserved: 0,
        };
        self.control(ENABLE, bytes(&options), &mut [], 1000)
            .map_err(|c| format!("Lab enable/disable: Win32 {c}"))?
            .ok_or("Lab enable/disable timed out")?;
        Ok(())
    }
    fn owner_info(&self) -> Result<OwnerInfo> {
        let mut info = OwnerInfo::default();
        let count = self
            .control(OWNER_INFO, &[], bytes_mut(&mut info), 1000)
            .map_err(|e| format!("Query broker binding: Win32 {e}"))?;
        if count != Some(32) || info.size != 32 || info.version != 3 {
            return Err("Broker binding diagnostics ABI mismatch".into());
        }
        Ok(info)
    }
    fn verify_session_isolation(&self) -> Result<()> {
        // Same process, same target, a distinct CreateFile/session. PID alone
        // must never authorize this handle to steal, stop, or serve the owner.
        let other = Self::open_endpoint(&self.endpoint)?;
        for enabled in [1, 0] {
            let options = Enable {
                size: 16,
                version: paging_protocol::VERSION,
                enabled,
                reserved: 0,
            };
            if other.control(ENABLE, bytes(&options), &mut [], 1000) != Err(5) {
                return Err("Foreign session could enable/disable the owner".into());
            }
        }
        let mut frame = [0_u8; std::mem::size_of::<Frame>()];
        if other.control(GET, &[], &mut frame, 1000) != Err(5)
            || other.control(REPLY, &frame, &mut [], 1000) != Err(5)
            || self.query()?.mode != 2
        {
            return Err("Foreign session was not rejected or changed owner state".into());
        }
        println!("Same-PID foreign handle rejected for ENABLE, DISABLE, GET and REPLY");
        Ok(())
    }
}
fn valid_interface(path: &str) -> bool {
    path.starts_with("\\\\?\\") && path.to_ascii_lowercase().contains(INTERFACE)
}

// Only POD repr(C) wire structs are passed to these private helpers. Their
// fields/padding are fully initialized; kernel/public wire ABI is fixed.
fn bytes<T>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((value as *const T).cast(), std::mem::size_of::<T>()) }
}
fn bytes_mut<T>(value: &mut T) -> &mut [u8] {
    unsafe { std::slice::from_raw_parts_mut((value as *mut T).cast(), std::mem::size_of::<T>()) }
}
pub fn query(path: &str) -> Result<DriverStatus> {
    Connection::open(path)?.query()
}
pub(crate) fn query_target(path: &str) -> Result<(DriverStatus, u64)> {
    let connection = Connection::open(path)?;
    Ok((connection.query()?, connection.owner_info()?.control_id))
}
/// Real local-file DeviceIoControl test for completion byte counts and the
/// serialized control event. It never opens a disk or enables our driver.
pub fn local_control_smoke() -> Result<()> {
    struct OwnedFile(std::path::PathBuf);
    impl Drop for OwnedFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "svram-control-{}-{unique}.svram-lab",
        std::process::id()
    ));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .custom_flags(0x40000000)
        .open(&path)
        .map_err(|e| e.to_string())?;
    // File remains empty: the control is FSCTL_GET_COMPRESSION (read-only).
    let owned = OwnedFile(path);
    let connection = Connection::from_file(file, "owned-local-control-smoke")?;
    let mut expected = [0u8; 2];
    if connection
        .control(0x9003c, &[], &mut expected, 1000)
        .map_err(|e| format!("Local filesystem control: Win32 {e}"))?
        != Some(2)
    {
        return Err("Local filesystem control returned an invalid byte count".into());
    }
    std::thread::scope(|scope| -> Result<()> {
        let mut workers = Vec::new();
        for _ in 0..4 {
            let worker = scope.spawn(|| -> Result<()> {
                for _ in 0..64 {
                    let mut output = [0xa5u8; 2];
                    let size = connection
                        .control(0x9003c, &[], &mut output, 1000)
                        .map_err(|e| format!("Concurrent local control: Win32 {e}"))?;
                    if size != Some(2) || output != expected {
                        return Err("Concurrent control status/output mismatch".into());
                    }
                }
                Ok(())
            });
            workers.push(worker);
        }
        for worker in workers {
            worker
                .join()
                .map_err(|_| "Local control worker panicked")??;
        }
        Ok(())
    })?;
    drop(connection);
    drop(owned);
    println!(
        "PASS 256 concurrent local-file control calls: serialized event, completed byte count and payload; no KMDF/pagefile qualification"
    );
    Ok(())
}
pub fn run(path: &str, seconds: u64) -> Result<()> {
    run_controlled(path, seconds, None, None, None)
}
pub(crate) fn run_managed(
    path: &str,
    expected_target: u64,
    seconds: u64,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
) -> Result<()> {
    run_controlled(
        path,
        seconds,
        Some(expected_target),
        Some(stop),
        Some(active),
    )
}
fn run_controlled(
    path: &str,
    seconds: u64,
    expected_target: Option<u64>,
    stop: Option<Arc<AtomicBool>>,
    active: Option<Arc<AtomicBool>>,
) -> Result<()> {
    #[cfg(not(feature = "wddm"))]
    {
        let _ = (path, seconds, expected_target, stop, active);
        Err("CUDA broker requires a build with --features wddm; unknown budget cannot enable the cache".into())
    }
    #[cfg(feature = "wddm")]
    {
        let connection = idle_lab_connection(path, seconds)?;
        if let Some(expected) = expected_target
            && (expected == 0 || connection.owner_info()?.control_id != expected)
        {
            return Err("GUI-selected target changed before broker activation".into());
        }
        let budget = crate::wddm::BudgetMonitor::for_cuda(0)?;
        if crate::wddm::memory_footprint()?.available_commit < (64 + 512) * MIB {
            return Err("Insufficient commit headroom for WDDM cache backing".into());
        }
        if budget.sample()?.local.allowance() < (64 + 256) * MIB {
            return Err("Insufficient WDDM allowance for experimental cache".into());
        }
        let cache = PageCache::new(VramStore::new(0, 64 * MIB)?)?;
        let non_local_baseline = budget.sample()?.non_local.usage;
        let working_set_baseline = crate::wddm::memory_footprint()?.process_working_set;
        serve(
            connection,
            cache,
            seconds,
            FixtureFault::None,
            BrokerControl {
                stop,
                active,
                budget: Some(budget),
                last_budget: Instant::now() - Duration::from_secs(1),
                non_local_baseline,
                working_set_baseline,
            },
        )
    }
}
struct BrokerControl {
    stop: Option<Arc<AtomicBool>>,
    active: Option<Arc<AtomicBool>>,
    #[cfg(feature = "wddm")]
    budget: Option<crate::wddm::BudgetMonitor>,
    #[cfg(feature = "wddm")]
    last_budget: Instant,
    #[cfg(feature = "wddm")]
    non_local_baseline: u64,
    #[cfg(feature = "wddm")]
    working_set_baseline: u64,
}
struct Activity(Option<Arc<AtomicBool>>);
impl Activity {
    fn started(&self) {
        if let Some(active) = &self.0 {
            active.store(true, Ordering::Release);
        }
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        if let Some(active) = &self.0 {
            active.store(false, Ordering::Release);
        }
    }
}
impl BrokerControl {
    fn running(&mut self) -> Result<bool> {
        if self
            .stop
            .as_ref()
            .is_some_and(|s| s.load(Ordering::Acquire))
        {
            return Ok(false);
        }
        #[cfg(feature = "wddm")]
        if let Some(budget) = &self.budget
            && (self.last_budget.elapsed() >= Duration::from_millis(250)
                || budget.wait_changed(0)?)
        {
            let sample = budget.sample()?;
            if sample.local.allowance() < 128 * MIB {
                return Err("WDDM budget pressure: experimental cache disabled".into());
            }
            if sample.non_local.usage > self.non_local_baseline.saturating_add(32 * MIB) {
                return Err("Non-local GPU memory grew beyond the cache baseline; releasing the experimental cache".into());
            }
            let memory = crate::wddm::memory_footprint()?;
            if memory.available_commit < 256 * MIB
                || memory.process_working_set > self.working_set_baseline.saturating_add(64 * MIB)
            {
                return Err("RAM/commit cost exceeded the experimental cache budget".into());
            }
            self.last_budget = Instant::now();
        }
        Ok(true)
    }
}

#[derive(Clone, Copy)]
enum FixtureFault {
    None,
    Miss,
    Late,
    Exit,
}

/// GPU-free fixture for an isolated Windows guest's *kernel transport* tests.
/// Its RAM allocation is deliberately not exposed as a VRAM or RAM-saving mode.
pub fn run_fixture(path: &str, seconds: u64, behavior: &str, stop_on_stdin: bool) -> Result<()> {
    let fault = match behavior {
        "cache" => FixtureFault::None,
        "miss" => FixtureFault::Miss,
        "late" => FixtureFault::Late,
        "exit" => FixtureFault::Exit,
        _ => return Err("Fixture behavior must be cache | miss | late | exit".into()),
    };
    let connection = idle_lab_connection(path, seconds)?;
    let mut data = Vec::new();
    data.try_reserve_exact(8 * MIB as usize)
        .map_err(|e| e.to_string())?;
    data.resize(8 * MIB as usize, 0);
    let cache = PageCache::new(RamFixture(data))?;
    let stop = if stop_on_stdin {
        let flag = Arc::new(AtomicBool::new(false));
        let thread_flag = flag.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let _ = std::io::stdin().read(&mut [0_u8; 1]);
            thread_flag.store(true, Ordering::Release);
        });
        Some(flag)
    } else {
        None
    };
    println!(
        "ISOLATED KERNEL TEST FIXTURE: 8 MiB RAM, no GPU, no RAM-saving claim; behavior={behavior}"
    );
    serve(
        connection,
        cache,
        seconds,
        fault,
        BrokerControl {
            stop,
            active: None,
            #[cfg(feature = "wddm")]
            budget: None,
            #[cfg(feature = "wddm")]
            last_budget: Instant::now(),
            #[cfg(feature = "wddm")]
            non_local_baseline: 0,
            #[cfg(feature = "wddm")]
            working_set_baseline: 0,
        },
    )
}
struct RamFixture(Vec<u8>);
impl BlockIo for RamFixture {
    fn capacity(&self) -> u64 {
        self.0.len() as u64
    }
    fn read(&mut self, offset: u64, page: &mut [u8; PAGE]) -> Result<()> {
        let start = usize::try_from(offset).map_err(|_| "Fixture offset overflow")?;
        let end = start.checked_add(PAGE).ok_or("Fixture range overflow")?;
        page.copy_from_slice(self.0.get(start..end).ok_or("Fixture read out of bounds")?);
        Ok(())
    }
    fn write(&mut self, offset: u64, page: &[u8; PAGE]) -> Result<()> {
        let start = usize::try_from(offset).map_err(|_| "Fixture offset overflow")?;
        let end = start.checked_add(PAGE).ok_or("Fixture range overflow")?;
        self.0
            .get_mut(start..end)
            .ok_or("Fixture write out of bounds")?
            .copy_from_slice(page);
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
fn idle_lab_connection(path: &str, seconds: u64) -> Result<Connection> {
    if !(1..=3600).contains(&seconds) {
        return Err("Broker duration must be 1–3600 seconds".into());
    }
    let connection = Connection::open(path)?;
    let before = connection.query()?;
    if before.mode != 1 {
        return Err("Requires an idle explicitly experimental KMDF build; no production qualification is implied".into());
    }
    Ok(connection)
}
fn serve<C: BlockIo>(
    connection: Connection,
    mut cache: PageCache<C>,
    seconds: u64,
    fault: FixtureFault,
    mut control: BrokerControl,
) -> Result<()> {
    println!("Broker binding: {:?}", connection.owner_info()?);
    let activity = Activity(control.active.clone());
    let mut pool = crate::broker_queue::GetPool::ioctl(&connection.file, GET)?;
    connection.enable(true)?;
    let result = (|| {
        connection.verify_session_isolation()?;
        pool.post_all()?;
        let _ = pool.next_ready()?;
        activity.started();
        let deadline = Instant::now() + Duration::from_secs(seconds);
        let mut reads = 0u64;
        let mut fills = 0u64;
        let mut skipped = 0u64;
        let mut expired_hits = 0u64;
        let mut work = crate::overlapped_io::Buffer::new();
        while Instant::now() < deadline && control.running()? {
            let Some(index) = pool.next_ready()? else {
                pool.wait(100)?;
                continue;
            };
            let size = pool.take_and_repost(index, &mut work)?;
            let frame = work.frame_mut();
            frame.validate_command(size as usize)?;
            let (keys, count) = frame.range.keys(frame.length as usize)?;
            let length = frame.length as usize;
            match frame.operation {
                OP_FILL => {
                    if !cache.fill_range(&keys[..count], &frame.data[..length]) {
                        return Err(format!(
                            "Cache revoked after backend fill failure: {}",
                            cache.last_error().unwrap_or("unknown backend error")
                        ));
                    }
                    fills += 1;
                }
                OP_READ => {
                    match fault {
                        // Test-only faults occur after the GET has reached this
                        // process. The independent kernel timer must take over.
                        FixtureFault::Late => std::thread::sleep(Duration::from_millis(20)),
                        FixtureFault::Exit => std::process::exit(87),
                        _ => (),
                    }
                    let may_lookup =
                        crate::read_budget::can_lookup(frame.deadline, crate::read_budget::now());
                    if !may_lookup {
                        skipped += 1;
                    }
                    frame.status = if may_lookup
                        && !matches!(fault, FixtureFault::Miss)
                        && cache.lookup_range(&keys[..count], &mut frame.data[..length])
                    {
                        HIT
                    } else {
                        MISS
                    };
                    if !cache.is_active() {
                        return Err(format!(
                            "Cache revoked after backend read failure: {}",
                            cache.last_error().unwrap_or("unknown backend error")
                        ));
                    }
                    if frame.status == HIT
                        && !crate::read_budget::can_reply_hit(
                            frame.deadline,
                            crate::read_budget::now(),
                        )
                    {
                        frame.status = MISS;
                        expired_hits += 1;
                    }
                    let reply_size = frame.reply_size();
                    match connection.control(REPLY, work.bytes(reply_size), &mut [], 1000) {
                        Ok(Some(_)) | Err(1168) => (), // Late kernel reply: disk already owns the read.
                        Ok(None) => return Err("Broker REPLY timed out".into()),
                        Err(code) => return Err(format!("Broker REPLY: Win32 {code}")),
                    }
                    reads += 1;
                }
                _ => return Err("Unknown KMDF broker operation".into()),
            }
        }
        Ok((reads, fills, skipped, expired_hits))
    })();
    let disable = connection.enable(false);
    pool.cancel_and_drain();
    drop(pool);
    cache.revoke();
    drop(activity);
    let (reads, fills, skipped, expired_hits) = result?;
    disable?;
    println!("Experimental broker stopped: {reads} read requests, {fills} optional fills");
    println!(
        "Latency guard: {skipped} GPU lookups skipped, {expired_hits} expired hits downgraded to miss"
    );
    println!("Filter counters: {:?}", connection.query()?);
    Ok(())
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
    pub(crate) fn DeviceIoControl(
        file: *mut c_void,
        code: u32,
        input: *const c_void,
        input_length: u32,
        output: *mut c_void,
        output_length: u32,
        bytes: *mut u32,
        overlap: *mut Overlapped,
    ) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, timeout: u32) -> u32;
    fn CancelIoEx(file: *mut c_void, overlap: *mut Overlapped) -> i32;
    fn GetLastError() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_overlapped_control_calls_keep_their_own_completion() {
        local_control_smoke().unwrap();
    }
    #[test]
    fn generic_disk_paths_cannot_be_used_by_broker() {
        assert!(!valid_interface(r"\\.\PhysicalDrive0"));
        assert!(!valid_interface(r"C:\pagefile.sys"));
        assert!(valid_interface(
            r"\\?\scsi#disk#lab#{79320e2b-6b29-4df1-a827-869105412c47}"
        ));
    }
    #[test]
    fn wire_sizes_match_driver_headers() {
        assert_eq!(std::mem::size_of::<DriverStatus>(), 128);
        assert_eq!(std::mem::size_of::<OwnerInfo>(), 32);
        assert_eq!(std::mem::offset_of!(OwnerInfo, control_id), 24);
        assert_eq!(std::mem::size_of::<Enable>(), 16);
        assert_eq!(std::mem::size_of::<Overlapped>(), 32);
        assert_eq!(
            std::mem::size_of::<Frame>(),
            paging_protocol::HEADER_BYTES + MAX_BYTES
        );
    }
}
