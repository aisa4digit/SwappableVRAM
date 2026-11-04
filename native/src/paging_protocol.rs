//! Bounded KMDF range transport. Separate from the legacy 4 KiB mailbox lab.
use crate::cache::PAGE;
use crate::page_cache::{MAX_RANGE_PAGES, PageKey};
use crate::store::Result;
pub const VERSION: u32 = 3;
pub const MAX_BYTES: usize = MAX_RANGE_PAGES * PAGE;
pub const MAGIC: u64 = 0x535652414d4c4142;
pub const OP_READ: u32 = 1;
pub const OP_FILL: u32 = 2;
pub const HIT: u32 = 2;
pub const MISS: u32 = 3;
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct RangeStamp {
    pub volume_epoch: u64,
    pub page_index: u64,
    pub pages: u32,
    pub reserved: u32,
    pub generations: [u64; MAX_RANGE_PAGES],
}
impl RangeStamp {
    pub fn keys(&self, length: usize) -> Result<([PageKey; MAX_RANGE_PAGES], usize)> {
        let count = self.pages as usize;
        if count == 0
            || count > MAX_RANGE_PAGES
            || length != count * PAGE
            || self.reserved != 0
            || self.generations[..count].contains(&0)
            || self.generations[count..].iter().any(|&g| g != 0)
            || self
                .page_index
                .checked_mul(PAGE as u64)
                .and_then(|offset| offset.checked_add(length as u64))
                .is_none()
        {
            return Err("Malformed paging range".into());
        }
        let mut keys = [PageKey {
            volume_epoch: 0,
            page_index: 0,
            generation: 0,
        }; MAX_RANGE_PAGES];
        for (index, key) in keys[..count].iter_mut().enumerate() {
            *key = PageKey {
                volume_epoch: self.volume_epoch,
                page_index: self.page_index + index as u64,
                generation: self.generations[index],
            };
        }
        Ok((keys, count))
    }
}
#[repr(C)]
pub struct Frame {
    pub magic: u64,
    pub version: u32,
    pub operation: u32,
    pub sequence: u64,
    pub deadline: u64,
    pub status: u32,
    pub length: u32,
    pub range: RangeStamp,
    pub data: [u8; MAX_BYTES],
}
pub const HEADER_BYTES: usize = std::mem::offset_of!(Frame, data);
impl Frame {
    pub fn empty() -> Box<Self> {
        Box::new(Self {
            magic: MAGIC,
            version: VERSION,
            operation: 0,
            sequence: 0,
            deadline: 0,
            status: 0,
            length: 0,
            range: RangeStamp::default(),
            data: [0; MAX_BYTES],
        })
    }
    pub fn validate_command(&self, received: usize) -> Result<()> {
        if self.magic != MAGIC || self.version != VERSION || self.sequence == 0 || self.status != 0
        {
            return Err("KMDF range transport ABI mismatch".into());
        }
        self.range.keys(self.length as usize)?;
        let expected = match self.operation {
            OP_READ => {
                if self.deadline == 0 {
                    return Err("Read command has no kernel deadline".into());
                }
                HEADER_BYTES
            }
            OP_FILL => {
                if self.deadline != 0 {
                    return Err("Fill command unexpectedly carries a read deadline".into());
                }
                HEADER_BYTES + self.length as usize
            }
            _ => return Err("Unknown range command".into()),
        };
        if received != expected {
            return Err("Truncated/oversized KMDF command".into());
        }
        Ok(())
    }
    pub fn reply_size(&self) -> usize {
        HEADER_BYTES
            + if self.status == HIT {
                self.length as usize
            } else {
                0
            }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn header_only_reads_and_misses_never_need_payload_copies() {
        assert_eq!(std::mem::size_of::<RangeStamp>(), 152);
        assert_eq!(HEADER_BYTES, 192);
        assert_eq!(std::mem::size_of::<Frame>(), 65728);
        let mut frame = Frame::empty();
        frame.operation = OP_READ;
        frame.sequence = 1;
        frame.deadline = 10000;
        frame.length = MAX_BYTES as u32;
        frame.range.pages = 16;
        frame.range.generations.fill(3);
        assert!(frame.validate_command(HEADER_BYTES).is_ok());
        assert!(frame.validate_command(HEADER_BYTES + 1).is_err());
        frame.status = MISS;
        assert_eq!(frame.reply_size(), HEADER_BYTES);
        frame.status = HIT;
        assert_eq!(frame.reply_size(), HEADER_BYTES + MAX_BYTES);
    }
    #[test]
    fn malformed_counts_wrap_and_noncanonical_tail_are_rejected() {
        let mut range = RangeStamp {
            pages: 1,
            generations: [1; 16],
            ..RangeStamp::default()
        };
        assert!(range.keys(PAGE).is_err());
        range.generations[1..].fill(0);
        assert!(range.keys(PAGE).is_ok());
        assert!(range.keys(PAGE + 1).is_err());
        range.page_index = u64::MAX / PAGE as u64;
        assert!(range.keys(PAGE).is_err());
        range.pages = 17;
        assert!(range.keys(17 * PAGE).is_err());
    }
    #[test]
    fn fill_requires_exact_payload_and_rejects_legacy_protocol() {
        let mut frame = Frame::empty();
        frame.sequence = 8;
        frame.operation = OP_FILL;
        frame.length = PAGE as u32;
        frame.range.pages = 1;
        frame.range.generations[0] = 9;
        assert!(frame.validate_command(HEADER_BYTES + PAGE).is_ok());
        assert!(frame.validate_command(HEADER_BYTES).is_err());
        assert!(frame.validate_command(HEADER_BYTES + PAGE - 1).is_err());
        frame.version = 1;
        assert!(frame.validate_command(HEADER_BYTES + PAGE).is_err());
    }
}
