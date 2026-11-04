//! Real local Win32 I/O ownership tests. No KMDF device, GPU, pagefile, driver
//! installation or host memory-policy changes are involved.
use crate::broker_queue::{GetPool, WINDOW};
use crate::cache::PAGE;
use crate::overlapped_io::{Buffer, CAPACITY, Request};
use crate::paging_protocol::{Frame, HEADER_BYTES, OP_FILL, OP_READ};
use crate::store::Result;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::windows::io::FromRawHandle;
use std::sync::Barrier;
use std::time::{Duration, Instant};
fn packet(operation: u32, sequence: u64) -> Vec<u8> {
    let pages = [1, 2, 4, 8, 16][sequence as usize % 5];
    let bytes = pages * PAGE;
    let mut frame = Frame::empty();
    frame.operation = operation;
    frame.sequence = sequence;
    frame.deadline = if operation == OP_READ {
        crate::read_budget::now() + 10_000_000
    } else {
        0
    };
    frame.length = bytes as u32;
    frame.range.volume_epoch = 7;
    frame.range.page_index = sequence * 16;
    frame.range.pages = pages as u32;
    frame.range.generations[..pages].fill(3);
    frame.data[..bytes].fill(sequence as u8);
    let length = HEADER_BYTES + if operation == OP_FILL { bytes } else { 0 };
    unsafe { std::slice::from_raw_parts(frame.as_ref() as *const Frame as *const u8, length) }
        .to_vec()
}
fn await_request(request: &mut Request<'_>) -> Result<std::result::Result<u32, u32>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        request.poll();
        if let Some(result) = request.completion() {
            return Ok(result);
        }
        if Instant::now() >= deadline {
            return Err("Local asynchronous I/O deadline".into());
        }
        Request::wait_any(std::slice::from_ref(request), 50)?;
    }
}
fn verify_buffer(buffer: &Buffer, size: u32, sequence: u64) -> Result<()> {
    let frame = buffer.frame();
    frame.validate_command(size as usize)?;
    if frame.sequence != sequence
        || frame.operation != OP_FILL
        || frame.data[..frame.length as usize]
            .iter()
            .any(|&b| b != sequence as u8)
    {
        return Err("Local asynchronous payload/sequence mismatch".into());
    }
    Ok(())
}
pub fn run() -> Result<()> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let name = format!(
        r"\\.\pipe\SwappableVRAM-ownership-{}-{unique}",
        std::process::id()
    );
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let raw = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            0x40080001,
            0x0e,
            1,
            (CAPACITY * WINDOW) as u32,
            (CAPACITY * WINDOW) as u32,
            1000,
            std::ptr::null(),
        )
    };
    if raw == (-1_isize) as *mut c_void {
        return Err(format!(
            "Create owned local pipe: {}",
            std::io::Error::last_os_error()
        ));
    }
    let server = unsafe { File::from_raw_handle(raw) };
    let mut connect = Request::new(&server)?;
    connect.start_pipe_connect()?;
    let client = OpenOptions::new()
        .write(true)
        .open(&name)
        .map_err(|e| e.to_string())?;
    await_request(&mut connect)?.map_err(|e| format!("Connect local pipe: Win32 {e}"))?;
    drop(connect);
    let mut pool = GetPool::pipe(&server)?;
    pool.post_all()?;
    let mut writer = &client;
    // Deliberately complete fills before reads. Once both kinds are ready, the
    // same pool used by the CDO broker must choose every read first.
    for index in 0..WINDOW {
        writer
            .write_all(&packet(
                if index < 8 { OP_FILL } else { OP_READ },
                index as u64 + 1,
            ))
            .map_err(|e| e.to_string())?;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while pool.completed_count() != WINDOW {
        if Instant::now() >= deadline {
            return Err("Local 16-slot completion deadline".into());
        }
        pool.wait(50)?;
    }
    let mut work = Buffer::new();
    for ordinal in 0..WINDOW {
        let index = pool
            .next_ready()?
            .ok_or("Completed pool lost its ready request")?;
        let bytes = pool.take_and_repost(index, &mut work)?;
        let frame = work.frame();
        frame.validate_command(bytes as usize)?;
        let expected = if ordinal < 8 {
            ordinal as u64 + 9
        } else {
            ordinal as u64 - 7
        };
        if frame.sequence != expected || (ordinal < 8) != (frame.operation == OP_READ) {
            return Err("Ready read was delayed behind optional fills".into());
        }
        if frame.operation == OP_FILL {
            verify_buffer(&work, bytes, expected)?;
        }
    }
    let drained = pool.cancel_and_drain();
    if drained.cancelled != WINDOW as u32 || drained.failed != 0 {
        return Err(format!(
            "Pending GET window did not cancel/drain: {drained:?}"
        ));
    }
    drop(pool);
    {
        let mut abandoned = GetPool::pipe(&server)?;
        abandoned.post_all()?;
        // No explicit shutdown call: RAII must cancel/drain all sixteen reads
        // before these buffers can be freed or reused by the allocator.
    }
    let mut after_drop = Request::new(&server)?;
    after_drop.start_pipe_read()?;
    writer
        .write_all(&packet(OP_FILL, 42))
        .map_err(|e| e.to_string())?;
    let size =
        await_request(&mut after_drop)?.map_err(|e| format!("Read after pool drop: Win32 {e}"))?;
    after_drop.take_buffer(&mut work)?;
    verify_buffer(&work, size, 42)?;
    drop(after_drop);
    // Exercise completion-vs-cancellation with real OS-owned buffers. A message
    // that loses the race remains in the pipe and is explicitly collected.
    let mut cancelled = 0;
    let mut completed = 0;
    for sequence in 1000..1032 {
        let mut request = Request::new(&server)?;
        request.start_pipe_read()?;
        let message = packet(OP_FILL, sequence);
        let barrier = Barrier::new(2);
        std::thread::scope(|scope| -> Result<()> {
            let handle = scope.spawn(|| -> Result<()> {
                barrier.wait();
                let mut writer = &client;
                writer.write_all(&message).map_err(|e| e.to_string())
            });
            barrier.wait();
            request.cancel();
            request.drain();
            handle.join().map_err(|_| "Local pipe writer panicked")??;
            Ok(())
        })?;
        match request.completion() {
            Some(Ok(size)) => {
                request.take_buffer(&mut work)?;
                verify_buffer(&work, size, sequence)?;
                completed += 1;
            }
            Some(Err(995)) => {
                cancelled += 1;
                let mut remaining = Request::new(&server)?;
                remaining.start_pipe_read()?;
                let size = await_request(&mut remaining)?
                    .map_err(|e| format!("Collect cancelled-race message: Win32 {e}"))?;
                remaining.take_buffer(&mut work)?;
                verify_buffer(&work, size, sequence)?;
            }
            other => return Err(format!("Unexpected cancellation race result: {other:?}")),
        }
    }
    // An oversized message must report MORE_DATA, never be accepted as a
    // successful protocol packet merely because its prefix looks plausible.
    let mut partial = GetPool::pipe(&server)?;
    partial.post_all()?;
    let mut oversized = packet(OP_FILL, 5000);
    oversized.resize(CAPACITY + 32, 0x55);
    writer.write_all(&oversized).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match partial.next_ready() {
            Err(error) if error.contains("Win32 234") => break,
            Err(error) => return Err(error),
            Ok(Some(_)) => return Err("Oversized asynchronous packet was accepted".into()),
            Ok(None) => {
                if Instant::now() >= deadline {
                    return Err("Oversized packet test deadline".into());
                }
                partial.wait(50)?;
            }
        }
    }
    drop(partial);
    println!(
        "PASS local Win32 ownership: 4-64 KiB packets, 16-slot read priority; 16 pending cancellations drained; 32 races ({completed} completed, {cancelled} cancelled); oversized packet rejected"
    );
    println!(
        "No KMDF/pagefile/GPU/FPS qualification; named pipe and all handles are local and temporary"
    );
    Ok(())
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        instances: u32,
        output_size: u32,
        input_size: u32,
        timeout: u32,
        security: *const c_void,
    ) -> *mut c_void;
}
#[cfg(test)]
mod tests {
    #[test]
    fn windows_owns_pending_buffers_until_cancelled_or_completed() {
        super::run().unwrap();
    }
}
