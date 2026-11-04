//! GPU-free fault injection for real kernel paging-I/O transport in the private
//! WinPE guest. A passing result is never a VRAM/FPS qualification.
use crate::store::Result;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
struct Peer(Child);
impl Drop for Peer {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
    }
}
pub fn run(boot: &Path, behavior: &str) -> Result<()> {
    crate::guest_lab::require_test_signing()?;
    if !matches!(behavior, "miss" | "late" | "exit" | "anon") {
        return Err("Kernel scenario must be miss | late | exit | anon".into());
    }
    if !boot.join("SVRAM.ID").is_file() {
        return Err("Owned disposable boot marker missing".into());
    }
    let endpoint = crate::device_interfaces::wait_lab_filter()?;
    println!(
        "Control endpoint: {} / parent {:?}",
        endpoint.instance_id, endpoint.parent_instance_id
    );
    let interface = &endpoint.path;
    let before = crate::driver_broker::query(interface)?;
    if before.mode != 1 || before.cache_leases != 0 || before.cache_d0_available != 1 {
        return Err(format!("Experimental driver not idle/ready: {before:?}"));
    }
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let log = boot.join(format!("fixture-{behavior}-{unique}.log"));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&log)
        .map_err(|e| e.to_string())?;
    let error_file = file.try_clone().map_err(|e| e.to_string())?;
    let child = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .args([
            "driver-fixture",
            interface,
            "120",
            if behavior == "anon" {
                "cache"
            } else {
                behavior
            },
            "--isolated-test",
            "--stop-on-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(error_file))
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut peer = Peer(child);
    let ready_deadline = Instant::now() + Duration::from_secs(4);
    while crate::driver_broker::query(interface)?.mode != 2 {
        if let Some(status) = peer.0.try_wait().map_err(|e| e.to_string())? {
            return Err(format!(
                "Fixture exited before activation: {status}; log={}",
                log.display()
            ));
        }
        if Instant::now() >= ready_deadline {
            return Err("Fixture never enabled the experimental cache".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    println!("GPU-free kernel scenario active: {behavior}");
    // This formats ONLY the guarded private 1 GiB target, performs real file
    // integrity checks and creates the guest pagefile while the broker is live.
    crate::guest_disk::prepare_expected(if behavior == "exit" { 1 } else { 2 })?;
    if behavior == "anon" {
        crate::guest_anonymous::run(interface)?;
    }
    if behavior != "exit" {
        use std::io::Write;
        let mut stop = peer
            .0
            .stdin
            .take()
            .ok_or("Fixture stdin stop pipe missing")?;
        stop.write_all(b"q").map_err(|e| e.to_string())?;
        stop.flush().map_err(|e| e.to_string())?;
    }
    let stop_deadline = Instant::now() + Duration::from_secs(5);
    let exit = loop {
        if let Some(exit) = peer.0.try_wait().map_err(|e| e.to_string())? {
            break exit;
        }
        if Instant::now() >= stop_deadline {
            return Err("Fixture did not stop within the guest deadline".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if (behavior == "exit" && exit.code() != Some(87)) || (behavior != "exit" && !exit.success()) {
        return Err(format!("Unexpected fixture exit: {exit}"));
    }
    let after = crate::driver_broker::query(interface)?;
    if after.mode != 1 || after.cache_leases != 0 || after.issued_reads <= before.issued_reads {
        return Err(format!(
            "Real kernel read/owner cleanup evidence missing: {after:?}"
        ));
    }
    if behavior == "late" && after.deadline_fallbacks <= before.deadline_fallbacks {
        return Err(format!(
            "Delayed broker did not exercise the kernel timer: {after:?}"
        ));
    }
    if behavior == "miss"
        && (after.reply_fallbacks + after.deadline_fallbacks
            <= before.reply_fallbacks + before.deadline_fallbacks
            || after.cache_hits != before.cache_hits)
    {
        return Err(format!("Forced-miss evidence invalid: {after:?}"));
    }
    println!("Kernel scenario result: {after:?}");
    println!("SVRAM_KERNEL_{behavior}_VERIFIED (RAM fixture, no GPU or game qualification)");
    println!("Fixture log: {}", log.display());
    Ok(())
}
