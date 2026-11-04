//! Small preposted GET window. Ready reads outrank optional fills; every slot
//! keeps its own buffer/event/OVERLAPPED until Windows completes the operation.
use crate::overlapped_io::{Buffer, Request};
use crate::paging_protocol::OP_READ;
use crate::store::Result;
use std::fs::File;
pub(crate) const WINDOW: usize = 16;
enum Source {
    Ioctl(u32),
    PipeRead,
}
pub(crate) struct GetPool<'a> {
    slots: Vec<Request<'a>>,
    source: Source,
}
#[derive(Default, Debug)]
pub(crate) struct DrainReport {
    pub completed: u32,
    pub cancelled: u32,
    pub failed: u32,
}
impl<'a> GetPool<'a> {
    fn new(file: &'a File, source: Source) -> Result<Self> {
        let mut slots = Vec::with_capacity(WINDOW);
        for _ in 0..WINDOW {
            slots.push(Request::new(file)?);
        }
        Ok(Self { slots, source })
    }
    pub(crate) fn ioctl(file: &'a File, code: u32) -> Result<Self> {
        Self::new(file, Source::Ioctl(code))
    }
    pub(crate) fn pipe(file: &'a File) -> Result<Self> {
        Self::new(file, Source::PipeRead)
    }
    fn post(&mut self, index: usize) -> Result<()> {
        match self.source {
            Source::Ioctl(code) => self.slots[index].start_ioctl(code),
            Source::PipeRead => self.slots[index].start_pipe_read(),
        }
    }
    pub(crate) fn post_all(&mut self) -> Result<()> {
        for index in 0..WINDOW {
            self.post(index)?;
        }
        Ok(())
    }
    pub(crate) fn next_ready(&mut self) -> Result<Option<usize>> {
        for index in 0..WINDOW {
            self.slots[index].poll();
            if self.slots[index].completion() == Some(Err(1237)) {
                self.post(index)?;
            }
            if let Some(Err(code)) = self.slots[index].completion()
                && code != 1237
            {
                return Err(format!("Asynchronous broker GET: Win32 {code}"));
            }
        }
        let mut selected: Option<(u32, u64, usize)> = None;
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(Ok(bytes)) = slot.completion() {
                let frame = slot
                    .ready_frame()
                    .ok_or("Completed GET has no owned buffer")?;
                frame.validate_command(bytes as usize)?;
                let rank = (u32::from(frame.operation != OP_READ), frame.sequence, index);
                if selected.is_none_or(|old| rank < old) {
                    selected = Some(rank);
                }
            }
        }
        Ok(selected.map(|(_, _, index)| index))
    }
    pub(crate) fn take_and_repost(&mut self, index: usize, work: &mut Buffer) -> Result<u32> {
        let bytes = self.slots[index].take_buffer(work)?;
        self.post(index)?;
        Ok(bytes)
    }
    pub(crate) fn wait(&self, milliseconds: u32) -> Result<()> {
        Request::wait_any(&self.slots, milliseconds)
    }
    pub(crate) fn completed_count(&mut self) -> usize {
        for slot in &mut self.slots {
            slot.poll();
        }
        self.slots
            .iter()
            .filter(|slot| slot.completion().is_some())
            .count()
    }
    pub(crate) fn cancel_and_drain(&mut self) -> DrainReport {
        // Cancel the whole window before waiting on any one slot.
        for slot in &mut self.slots {
            slot.cancel();
        }
        for slot in &mut self.slots {
            slot.drain();
        }
        let mut report = DrainReport::default();
        for slot in &self.slots {
            match slot.completion() {
                Some(Ok(_)) => report.completed += 1,
                Some(Err(995)) => report.cancelled += 1,
                Some(Err(_)) => report.failed += 1,
                None => (),
            }
        }
        report
    }
}
impl Drop for GetPool<'_> {
    fn drop(&mut self) {
        let _ = self.cancel_and_drain();
    }
}
