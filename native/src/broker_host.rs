//! Interactive, authenticated GUI control channel. The default profile never
//! enables an unqualified backend. An explicit lab profile is separate.
use crate::control_protocol::*;
use crate::overlapped_io::{Buffer, Request};
use crate::process_identity::ProcessIdentity;
use crate::store::Result;
use std::ffi::c_void;
use std::fs::OpenOptions;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
struct Actor {
    target: u64,
    path: String,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<Result<()>>,
}
impl Actor {
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}
fn inspect(identity: &Packet, allow_experimental: bool) -> Result<(Packet, Option<String>)> {
    let mut packet = identity.clone();
    packet.kind = REPORT;
    packet.sequence = 0;
    packet.ci = crate::guest_lab::code_integrity_options()?;
    let devices = crate::device_interfaces::filters()?;
    packet.drivers = devices.len() as u32;
    packet.state = if devices.is_empty() {
        NO_DRIVER
    } else {
        UNQUALIFIED
    };
    if allow_experimental && devices.len() == 1 && devices[0].is_lab_target() {
        if packet.flags & ELEVATED == 0 {
            packet.state = PERMISSION_REQUIRED;
        } else if packet.flags & WDDM == 0 {
            packet.state = UNSUPPORTED_BUILD;
        } else if packet.ci & 2 != 0 {
            let (status, target_id) = crate::driver_broker::query_target(&devices[0].path)?;
            if status.mode == 1 && status.cache_d0_available == 1 {
                packet.target = target_id;
                packet.state = READY_EXPERIMENTAL;
                return Ok((packet, Some(devices[0].path.clone())));
            }
        }
    }
    Ok((packet, None))
}
fn write(writer: &mut Request<'_>, packet: &Packet, parent: &ProcessIdentity) -> Result<()> {
    writer.start_pipe_write(&packet.encode())?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        writer.poll();
        if let Some(result) = writer.completion() {
            return if result == Ok(BYTES as u32) {
                Ok(())
            } else {
                Err(format!("GUI control write failed: {result:?}"))
            };
        }
        if !parent.alive() || Instant::now() >= deadline {
            writer.cancel();
            writer.drain();
            return Err("GUI peer disappeared or stopped reading".into());
        }
        Request::wait_any(std::slice::from_ref(writer), 50)?;
    }
}
pub fn run(pipe: &str, parent_pid: u32, nonce_text: &str, allow_experimental: bool) -> Result<()> {
    if !pipe.starts_with("SwappableVRAM-ui-")
        || pipe.len() > 128
        || !pipe.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err("Invalid GUI pipe name".into());
    }
    let parent = ProcessIdentity::read(parent_pid)?;
    let own = ProcessIdentity::read(std::process::id())?;
    if own.pid == parent.pid
        || own.session == 0
        || own.session != parent.session
        || own.sid != parent.sid
        || !parent.alive()
    {
        return Err("Broker requires its live GUI in the same user/session".into());
    }
    let path = format!(r"\\.\pipe\{pipe}");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(0x40110000)
        .open(path)
        .map_err(|e| format!("Connect GUI pipe: {e}"))?; // OVERLAPPED, SQOS, IDENTIFICATION
    let mut server_pid = 0;
    if unsafe { GetNamedPipeServerProcessId(file.as_raw_handle(), &mut server_pid) } == 0
        || server_pid != parent.pid
    {
        return Err("GUI pipe server PID mismatch".into());
    }
    let mode = 2u32;
    if unsafe {
        SetNamedPipeHandleState(
            file.as_raw_handle(),
            &mode,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err("GUI pipe must use message framing".into());
    }
    let identity = Packet {
        kind: HELLO,
        parent: parent.pid,
        broker: own.pid,
        session: own.session,
        flags: (u32::from(allow_experimental) * EXPERIMENTAL)
            | (u32::from(cfg!(feature = "wddm")) * WDDM)
            | (u32::from(own.elevated) * ELEVATED),
        nonce: crate::control_protocol::nonce(nonce_text)?,
        ..Packet::default()
    };
    let mut reader = Request::new(&file)?;
    let mut writer = Request::new(&file)?;
    let mut work = Buffer::new();
    write(&mut writer, &identity, &parent)?;
    reader.start_pipe_read_length(BYTES)?;
    let mut actor: Option<Actor> = None;
    let mut last = 0;
    let mut next_report = Instant::now();
    let mut stopping = None;
    let mut fault = 0;
    let result = (|| -> Result<()> {
        loop {
            if !parent.alive() {
                break;
            }
            reader.poll();
            let mut request_sequence = 0;
            if let Some(completion) = reader.completion() {
                let bytes = completion.map_err(|e| format!("GUI control read: Win32 {e}"))?;
                if bytes as usize != BYTES {
                    return Err("Partial GUI command rejected".into());
                }
                reader.take_buffer(&mut work)?;
                let command = Packet::decode(work.bytes(BYTES))?;
                command.validate_command(&identity, last)?;
                last = command.sequence;
                request_sequence = command.sequence;
                match command.kind {
                    STOP => {
                        if let Some(actor) = &actor {
                            actor.stop();
                        }
                        stopping = Some((command.sequence, Instant::now()));
                    }
                    START => {
                        let (status, target) = inspect(&identity, allow_experimental)?;
                        if status.state == READY_EXPERIMENTAL
                            && command.target == status.target
                            && actor.is_none()
                        {
                            let stop = Arc::new(AtomicBool::new(false));
                            let active = Arc::new(AtomicBool::new(false));
                            let thread_stop = stop.clone();
                            let thread_active = active.clone();
                            let target = target.ok_or("Experimental target missing")?;
                            let path = target.clone();
                            let expected_target = status.target;
                            let thread = std::thread::spawn(move || {
                                crate::driver_broker::run_managed(
                                    &target,
                                    expected_target,
                                    3600,
                                    thread_stop,
                                    thread_active,
                                )
                            });
                            actor = Some(Actor {
                                target: status.target,
                                path,
                                stop,
                                active,
                                thread,
                            });
                            fault = 0;
                        }
                    }
                    QUERY => (),
                    _ => unreachable!(),
                }
                if stopping.is_none() {
                    reader.start_pipe_read_length(BYTES)?;
                }
            }
            let finished = actor.as_ref().is_some_and(|a| a.thread.is_finished());
            if finished {
                let finished = actor.take().unwrap();
                if !matches!(finished.thread.join(), Ok(Ok(()))) {
                    fault = 1;
                }
            }
            if request_sequence != 0 || Instant::now() >= next_report || stopping.is_some() {
                let (mut status, _) = inspect(&identity, allow_experimental)?;
                status.sequence = request_sequence;
                if let Some(actor) = &actor {
                    status.target = actor.target;
                    status.state = if actor.active.load(Ordering::Acquire) {
                        ACTIVE_EXPERIMENTAL
                    } else {
                        STARTING
                    };
                    if status.state == ACTIVE_EXPERIMENTAL {
                        match crate::driver_broker::query_target(&actor.path) {
                            Ok((driver, target)) if driver.mode == 2 && target == actor.target => {
                                status.cache_bytes = 64 * crate::store::MIB;
                                status.hits = driver.cache_hits;
                                status.fallbacks =
                                    driver.reply_fallbacks + driver.deadline_fallbacks;
                            }
                            _ => {
                                actor.stop();
                                status.state = FAULTED;
                                status.error = 2;
                            }
                        }
                    }
                }
                if fault != 0 {
                    status.state = FAULTED;
                    status.error = fault;
                }
                if let Some((sequence, _)) = stopping
                    && actor.is_none()
                {
                    status.state = STOPPED;
                    status.sequence = sequence;
                    write(&mut writer, &status, &parent)?;
                    break;
                }
                if stopping.is_none() {
                    write(&mut writer, &status, &parent)?;
                }
                next_report = Instant::now() + Duration::from_secs(1);
            }
            if let Some((_, started)) = stopping {
                if started.elapsed() > Duration::from_secs(5) {
                    std::process::exit(2);
                }
                std::thread::sleep(Duration::from_millis(20));
            } else {
                Request::wait_any(std::slice::from_ref(&reader), 100)?;
            }
        }
        Ok(())
    })();
    if let Some(actor) = actor {
        actor.stop();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !actor.thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if !actor.thread.is_finished() {
            std::process::exit(2);
        } // only this helper; kernel has independent SSD fallback.
        let _ = actor.thread.join();
    }
    reader.cancel();
    reader.drain();
    writer.cancel();
    writer.drain();
    result
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetNamedPipeServerProcessId(pipe: *mut c_void, pid: *mut u32) -> i32;
    fn SetNamedPipeHandleState(
        pipe: *mut c_void,
        mode: *const u32,
        maximum: *const u32,
        timeout: *const u32,
    ) -> i32;
}
