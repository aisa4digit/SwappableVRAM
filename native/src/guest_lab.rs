//! Helpers for a disposable WinPE repro guest. Certificate writes require both
//! the MiniNT registry marker and the LocalSystem token, checked at runtime.
//! The desktop application never calls these functions.
use crate::store::Result;
use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

#[derive(Debug)]
pub struct Environment {
    pub winpe: bool,
    pub local_system: bool,
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
pub fn environment() -> Result<Environment> {
    let mut key = std::ptr::null_mut();
    let hklm = (0x80000002_u32 as i32 as isize) as *mut c_void;
    let status = unsafe {
        RegOpenKeyExW(
            hklm,
            wide(r"SYSTEM\CurrentControlSet\Control\MiniNT").as_ptr(),
            0,
            0x20019,
            &mut key,
        )
    };
    let winpe = match status {
        0 => {
            unsafe { RegCloseKey(key) };
            true
        }
        2 => false,
        _ => return Err(format!("Read WinPE marker: Win32 {status}")),
    };
    let mut raw = std::ptr::null_mut();
    if unsafe { OpenProcessToken((-1_isize) as *mut c_void, 8, &mut raw) } == 0 {
        return Err(format!(
            "Open own token: {}",
            std::io::Error::last_os_error()
        ));
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut size = 0;
    unsafe { GetTokenInformation(token.as_raw_handle(), 1, std::ptr::null_mut(), 0, &mut size) };
    if unsafe { GetLastError() } != 122 || !(16..=65536).contains(&size) {
        return Err("Unexpected token information size".into());
    }
    let mut buffer = vec![0_u64; (size as usize).div_ceil(8)];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            1,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    } == 0
    {
        return Err(format!(
            "Read own token: {}",
            std::io::Error::last_os_error()
        ));
    }
    // TOKEN_USER starts with SID_AND_ATTRIBUTES: a pointer then DWORD flags.
    let sid = unsafe { buffer.as_ptr().cast::<*const c_void>().read() };
    let local_system = unsafe { IsWellKnownSid(sid, 22) } != 0; // WinLocalSystemSid
    Ok(Environment {
        winpe,
        local_system,
    })
}
pub fn require_guest() -> Result<Environment> {
    let identity = environment()?;
    if !identity.winpe || !identity.local_system {
        return Err(
            "Lab mutation is restricted to a disposable WinPE LocalSystem guest; host unchanged"
                .into(),
        );
    }
    Ok(identity)
}
pub fn enable_io_verifier() -> Result<()> {
    require_test_signing()?;
    let endpoint = crate::device_interfaces::wait_lab_filter()?;
    if crate::driver_broker::query(&endpoint.path)?.mode != 1 {
        return Err("Verifier requires the loaded idle experimental guest driver".into());
    }
    let tool = crate::guest_disk::system_tool("verifier.exe")?;
    let enabled = std::process::Command::new(&tool)
        .args(["/dif", "5", "/now", "/driver", "svramflt.sys"])
        .output()
        .map_err(|e| e.to_string())?;
    let output = String::from_utf8_lossy(&enabled.stdout);
    print!("{output}{}", String::from_utf8_lossy(&enabled.stderr));
    // This verifier build can return exit 0 while reporting a failed rule
    // deployment. Inspect its runtime result, not just the process exit code.
    let active = output
        .lines()
        .any(|line| line.trim() == "\"CurrentlyEnabled\": Yes");
    if !enabled.status.success() || !active {
        return Err(
            "Guest DIF I/O Verifier was not currently enabled; no Verifier pass is implied".into(),
        );
    }
    let query = std::process::Command::new(&tool)
        .arg("/query")
        .output()
        .map_err(|e| e.to_string())?;
    let output = String::from_utf8_lossy(&query.stdout);
    print!("{output}{}", String::from_utf8_lossy(&query.stderr));
    if !query.status.success()
        || !output.to_ascii_lowercase().contains("svramflt.sys")
        || output.contains("No drivers are currently verified")
    {
        return Err("Runtime Verifier query does not show the guest target".into());
    }
    println!("SVRAM_GUEST_IO_VERIFIER_ACTIVE (class 5 only, not full WDF/Verifier qualification)");
    Ok(())
}
pub fn trust_disposable_certificate(path: &std::path::Path) -> Result<()> {
    require_guest()?;
    let certificate = std::fs::read(path).map_err(|e| e.to_string())?;
    if !(256..=16384).contains(&certificate.len()) {
        return Err("Unexpected lab certificate size".into());
    }
    for name in ["ROOT", "TrustedPublisher"] {
        let store = unsafe {
            CertOpenStore(
                10_usize as *const u8,
                0,
                0,
                0x20000,
                wide(name).as_ptr().cast(),
            )
        };
        if store.is_null() {
            return Err(format!(
                "Open guest certificate store: {}",
                std::io::Error::last_os_error()
            ));
        }
        let added = unsafe {
            CertAddEncodedCertificateToStore(
                store,
                1,
                certificate.as_ptr(),
                certificate.len() as u32,
                3,
                std::ptr::null_mut(),
            )
        };
        let error = std::io::Error::last_os_error();
        unsafe { CertCloseStore(store, 0) };
        if added == 0 {
            return Err(format!("Import guest lab certificate: {error}"));
        }
    }
    println!("Disposable test certificate imported only into this WinPE guest");
    Ok(())
}
pub fn require_test_signing() -> Result<()> {
    require_guest()?;
    if code_integrity_options()? & 2 == 0 {
        return Err("Guest kernel does not report runtime test-signing".into());
    }
    println!("Guest kernel runtime CODEINTEGRITY_OPTION_TESTSIGN is enabled");
    Ok(())
}
/// Read-only runtime state; safe to query on the desktop, no BCD changes.
pub fn code_integrity_options() -> Result<u32> {
    #[repr(C)]
    struct CodeIntegrity {
        length: u32,
        options: u32,
    }
    let mut info = CodeIntegrity {
        length: 8,
        options: 0,
    };
    let mut returned = 0;
    // SystemCodeIntegrityInformation=103, per the Windows SDK winternl.h enum.
    let status = unsafe {
        NtQuerySystemInformation(
            103,
            (&mut info as *mut CodeIntegrity).cast(),
            8,
            &mut returned,
        )
    };
    if status < 0 {
        return Err(format!(
            "Query runtime code integrity: NTSTATUS {status:#010x}"
        ));
    }
    if info.length != 8 || (returned != 0 && returned != 8) {
        return Err("Code integrity ABI mismatch".into());
    }
    Ok(info.options)
}
pub fn check_bypass() -> Result<()> {
    require_test_signing()?;
    let filter = crate::device_interfaces::wait_lab_filter()?;
    let status = crate::driver_broker::query(&filter.path)?;
    if status.mode != 0 {
        return Err(
            "Expected bypass-only build; optional cache must not be enabled in this smoke test"
                .into(),
        );
    }
    println!("Private filter attached to {}", filter.instance_id);
    println!("Private endpoint parent: {:?}", filter.parent_instance_id);
    println!("Live query ABI v{}: {status:?}", status.version);
    println!("SVRAM_BYPASS_ATTACH_VERIFIED");
    Ok(())
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        root: *mut c_void,
        key: *const u16,
        options: u32,
        access: u32,
        result: *mut *mut c_void,
    ) -> i32;
    fn RegCloseKey(key: *mut c_void) -> i32;
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(
        token: *mut c_void,
        class: u32,
        buffer: *mut c_void,
        length: u32,
        required: *mut u32,
    ) -> i32;
    fn IsWellKnownSid(sid: *const c_void, kind: u32) -> i32;
}
#[link(name = "crypt32")]
unsafe extern "system" {
    fn CertOpenStore(
        provider: *const u8,
        encoding: u32,
        crypt_provider: usize,
        flags: u32,
        parameter: *const c_void,
    ) -> *mut c_void;
    fn CertAddEncodedCertificateToStore(
        store: *mut c_void,
        encoding: u32,
        encoded: *const u8,
        length: u32,
        disposition: u32,
        context: *mut *mut c_void,
    ) -> i32;
    fn CertCloseStore(store: *mut c_void, flags: u32) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQuerySystemInformation(
        class: u32,
        info: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}
