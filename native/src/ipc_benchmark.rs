//! Executes the broker in a separate private child process. Reports actual
//! mailbox + scheduling + GPU read + verified reply latency, not page faults.
use crate::page_cache::PageKey;
use crate::store::Result;
use crate::transport_lab::OP_STALL_THEN_EXIT;
use crate::transport_lab::{Frame, HIT, MISS, Mailbox, OK, OP_FILL, OP_READ, OP_REVOKE, OP_STOP};
use std::os::windows::{io::AsRawHandle, process::CommandExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct PrivateChild(Child);
impl PrivateChild {
    fn finish(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Some(status) = self.0.try_wait().map_err(|e| e.to_string())? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("Broker exited with {status}"))
                };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err("Private broker did not stop in two seconds".into())
    }
}
impl Drop for PrivateChild {
    fn drop(&mut self) {
        // Only this private diagnostic child, never a user's application.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
    }
}

pub fn run(output: &Path) -> Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let session = format!("{}-{stamp}", std::process::id());
    let mut mailbox = Mailbox::create(&session)?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = PrivateChild(
        Command::new(executable)
            .args(["lab-broker", &session])
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?,
    );
    let handle = child.0.as_raw_handle();
    // SAFETY: child owns this process handle until the last request completes.
    unsafe {
        mailbox.wait_ready(handle)?;
    }
    fn key(page: u64) -> PageKey {
        PageKey {
            volume_epoch: 14041,
            page_index: page,
            generation: 1,
        }
    }
    const PAGES: u64 = 256;
    for page in 0..PAGES {
        let mut request = Frame::request(OP_FILL, key(page));
        request.data.fill((page % 251) as u8);
        let response = unsafe { mailbox.call(request, handle, 1000)? };
        if response.status != OK {
            return Err("Broker refused cache fill".into());
        }
    }
    for page in 0..32 {
        unsafe {
            mailbox.call(Frame::request(OP_READ, key(page)), handle, 1000)?;
        }
    }
    let mut raw = Vec::with_capacity(2000);
    let mut seed = 14041u64;
    for _ in 0..2000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let page = (seed >> 32) % PAGES;
        let start = Instant::now();
        let response = unsafe { mailbox.call(Frame::request(OP_READ, key(page)), handle, 1000)? };
        raw.push(start.elapsed().as_secs_f64() * 1_000_000.0);
        if response.status != HIT || response.data.iter().any(|b| *b != (page % 251) as u8) {
            return Err(format!("Broker payload failed integrity at page {page}"));
        }
    }
    // New write generation must not return old data from the cache.
    let stale = PageKey {
        generation: 2,
        ..key(0)
    };
    if unsafe { mailbox.call(Frame::request(OP_READ, stale), handle, 1000)? }.status != MISS {
        return Err("Stale cache generation returned as a hit".into());
    }
    if unsafe { mailbox.call(Frame::request(OP_REVOKE, key(0)), handle, 1000)? }.status != OK {
        return Err("Revoke failed".into());
    }
    if unsafe { mailbox.call(Frame::request(OP_READ, key(0)), handle, 1000)? }.status != MISS {
        return Err("Revoked cache returned a hit".into());
    }
    unsafe {
        mailbox.call(Frame::request(OP_STOP, key(0)), handle, 1000)?;
    }
    child.finish()?;
    let start = Instant::now();
    let exited = unsafe { mailbox.call(Frame::request(OP_READ, key(0)), handle, 1000) };
    let exited_us = start.elapsed().as_secs_f64() * 1_000_000.0;
    if exited.is_ok() {
        return Err("Exited broker returned a successful response".into());
    }
    // Timeout/exit poisons the mailbox: never reuse a slot a late writer could own.
    if unsafe { mailbox.call(Frame::request(OP_READ, key(0)), handle, 1000) }.is_ok() {
        return Err("Poisoned mailbox was reusable".into());
    }
    let mut sorted = raw.clone();
    // Exercise an actual deadline/late reply separately from process exit.
    let timeout_session = format!("{session}-timeout");
    let mut timeout_mailbox = Mailbox::create(&timeout_session)?;
    let mut slow_child = PrivateChild(
        Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
            .args(["lab-broker", &timeout_session])
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?,
    );
    let slow_handle = slow_child.0.as_raw_handle();
    unsafe {
        timeout_mailbox.wait_ready(slow_handle)?;
    }
    let timeout_result = unsafe {
        timeout_mailbox.call(Frame::request(OP_STALL_THEN_EXIT, key(0)), slow_handle, 10)
    };
    if !matches!(timeout_result, Err(ref message) if message.contains("timed out")) {
        return Err("Real broker timeout was not exercised".into());
    }
    if unsafe { timeout_mailbox.call(Frame::request(OP_READ, key(0)), slow_handle, 1000) }.is_ok() {
        return Err("Timed-out mailbox was reusable before late response".into());
    }
    slow_child.finish()?;
    if unsafe { timeout_mailbox.call(Frame::request(OP_READ, key(0)), slow_handle, 1000) }.is_ok() {
        return Err("Late response reactivated a poisoned request slot".into());
    }
    sorted.sort_by(f64::total_cmp);
    let (p50, p95, p99, maximum) = (sorted[999], sorted[1899], sorted[1979], sorted[1999]);
    let report = format!(
        concat!(
            "{{\n  \"kind\": \"separate-process-CUDA-mailbox-lab\",\n",
            "  \"windowsPagingDriverActive\": false,\n  \"blockBytes\": 4096,\n  \"queueDepth\": 1,\n",
            "  \"samples\": 2000,\n  \"corpusBytes\": 1048576,\n  \"vramPoolMiB\": 16,\n",
            "  \"integrityPassed\": true,\n  \"staleGenerationMissPassed\": true,\n  \"revocationMissPassed\": true,\n",
            "  \"brokerExitDetected\": true,\n  \"realTimeoutExercised\": true,\n  \"timeoutSlotReuseRefused\": true,\n  \"lateReplyReuseRefused\": true,\n",
            "  \"brokerExitDetectionUs\": {},\n  \"p50Us\": {},\n  \"p95Us\": {},\n  \"p99Us\": {},\n  \"maxUs\": {},\n",
            "  \"rawMicroseconds\": {:?}\n}}\n"
        ),
        exited_us, p50, p95, p99, maximum, raw
    );
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(output, report).map_err(|e| e.to_string())?;
    println!(
        "PASS separate-process CUDA: 4KiB QD1 p50={p50:.1}, p95={p95:.1}, p99={p99:.1}, max={maximum:.1} us"
    );
    println!(
        "PASS payload integrity, stale generation, revocation, broker exit detection, actual timeout and late-reply slot ownership"
    );
    println!("{}", output.display());
    Ok(())
}
