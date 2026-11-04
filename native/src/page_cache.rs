//! Cache-only half of the paging transport. The authoritative disk belongs to
//! the driver, not to this optional broker. Miss/error always means disk bypass.
use crate::cache::{BlockIo, PAGE};
use crate::store::Result;
use std::collections::HashMap;
pub const MAX_RANGE_PAGES: usize = 16;
const MAX_READ_RUNS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct PageKey {
    pub volume_epoch: u64,
    pub page_index: u64,
    pub generation: u64,
}
#[derive(Clone, Copy)]
struct Entry {
    slot: usize,
    checksum: u64,
}
pub struct PageCache<C: BlockIo> {
    backend: Option<C>,
    pages: HashMap<PageKey, Entry>,
    slots: Vec<Option<PageKey>>,
    next: usize,
    last_error: Option<String>,
}
impl<C: BlockIo> PageCache<C> {
    pub fn is_active(&self) -> bool {
        self.backend.is_some()
    }
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
    pub fn new(backend: C) -> Result<Self> {
        let count =
            usize::try_from(backend.capacity() / PAGE as u64).map_err(|_| "Cache too large")?;
        if count == 0 {
            return Err("Empty page cache".into());
        }
        let mut pages = HashMap::new();
        pages.try_reserve(count).map_err(|e| e.to_string())?;
        let mut slots = Vec::new();
        slots.try_reserve_exact(count).map_err(|e| e.to_string())?;
        slots.resize(count, None);
        Ok(Self {
            backend: Some(backend),
            pages,
            slots,
            next: 0,
            last_error: None,
        })
    }

    pub fn fill(&mut self, key: PageKey, data: &[u8; PAGE]) -> bool {
        self.fill_range(&[key], data)
    }
    /// Reserve consecutive VRAM slots so a whole paging range needs one GPU
    /// transfer. Publish its keys only after the entire copy has succeeded.
    pub fn fill_range(&mut self, keys: &[PageKey], data: &[u8]) -> bool {
        if !valid_range(keys, data.len()) || keys.len() > self.slots.len() {
            return false;
        }
        let Some(backend) = self.backend.as_mut() else {
            return false;
        };
        for key in keys {
            if let Some(previous) = self.pages.remove(key) {
                self.slots[previous.slot] = None;
            }
        }
        let first = if self.next + keys.len() <= self.slots.len() {
            self.next
        } else {
            0
        };
        self.next = (first + keys.len()) % self.slots.len();
        for slot in first..first + keys.len() {
            if let Some(old) = self.slots[slot].take() {
                self.pages.remove(&old);
            }
        }
        if let Err(error) = backend.write_blocks((first * PAGE) as u64, data) {
            self.last_error = Some(format!("GPU range fill: {error}"));
            self.revoke();
            return false;
        }
        for (index, key) in keys.iter().copied().enumerate() {
            let slot = first + index;
            self.slots[slot] = Some(key);
            self.pages.insert(
                key,
                Entry {
                    slot,
                    checksum: checksum(&data[index * PAGE..(index + 1) * PAGE]),
                },
            );
        }
        true
    }

    pub fn lookup(&mut self, key: PageKey, output: &mut [u8; PAGE]) -> bool {
        self.lookup_range(&[key], output)
    }
    /// A partial hit is a miss. Check every key before starting any GPU I/O;
    /// heavily fragmented ranges bypass instead of accumulating copy latency.
    pub fn lookup_range(&mut self, keys: &[PageKey], output: &mut [u8]) -> bool {
        if !valid_range(keys, output.len()) {
            return false;
        }
        let mut entries = [Entry {
            slot: 0,
            checksum: 0,
        }; MAX_RANGE_PAGES];
        let mut runs = 0;
        for (index, key) in keys.iter().enumerate() {
            let Some(entry) = self.pages.get(key).copied() else {
                return false;
            };
            entries[index] = entry;
            if index == 0 || entries[index - 1].slot + 1 != entry.slot {
                runs += 1;
            }
        }
        if runs > MAX_READ_RUNS {
            return false;
        }
        let Some(backend) = self.backend.as_mut() else {
            return false;
        };
        let mut first = 0;
        while first < keys.len() {
            let mut end = first + 1;
            while end < keys.len() && entries[end].slot == entries[end - 1].slot + 1 {
                end += 1;
            }
            if let Err(error) = backend.read_blocks(
                (entries[first].slot * PAGE) as u64,
                &mut output[first * PAGE..end * PAGE],
            ) {
                self.last_error = Some(format!("GPU range read: {error}"));
                self.revoke();
                return false;
            }
            first = end;
        }
        for (index, entry) in entries[..keys.len()].iter().enumerate() {
            if checksum(&output[index * PAGE..(index + 1) * PAGE]) != entry.checksum {
                self.last_error = Some(format!(
                    "GPU range checksum mismatch at cache slot {}",
                    entry.slot
                ));
                self.revoke();
                return false;
            }
        }
        true
    }

    pub fn revoke(&mut self) {
        // A revoked cache cannot be refilled. Release its CPU index as well as
        // its VRAM instead of retaining a large table while the game needs RAM.
        self.pages = HashMap::new();
        self.slots = Vec::new();
        self.next = 0;
        self.backend.take();
    }
}
fn valid_range(keys: &[PageKey], length: usize) -> bool {
    !keys.is_empty()
        && keys.len() <= MAX_RANGE_PAGES
        && length == keys.len() * PAGE
        && keys.iter().enumerate().all(|(index, key)| {
            key.volume_epoch == keys[0].volume_epoch
                && keys[0].page_index.checked_add(index as u64) == Some(key.page_index)
        })
}
fn checksum(page: &[u8]) -> u64 {
    page.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keys(first: u64, count: usize) -> Vec<PageKey> {
        (0..count)
            .map(|index| PageKey {
                volume_epoch: 4,
                page_index: first + index as u64,
                generation: index as u64 + 1,
            })
            .collect()
    }
    #[test]
    fn range_hit_is_all_or_nothing_and_eviction_keeps_one_slot_owner() {
        let mut cache = PageCache::new(Ram(vec![0; 18 * PAGE])).unwrap();
        let first = keys(100, 16);
        let data: Vec<u8> = (0..16 * PAGE).map(|i| (i % 251) as u8).collect();
        assert!(cache.fill_range(&first, &data));
        let mut output = vec![0; data.len()];
        assert!(cache.lookup_range(&first, &mut output));
        assert_eq!(output, data);
        let mut stale = first.clone();
        stale[7].generation += 1;
        output.fill(123);
        assert!(!cache.lookup_range(&stale, &mut output));
        assert!(output.iter().all(|&b| b == 123));
        let second = keys(200, 16);
        assert!(cache.fill_range(&second, &data));
        assert!(!cache.lookup_range(&first, &mut output));
        assert!(cache.lookup_range(&second, &mut output));
        assert_eq!(output, data);
        assert_eq!(cache.pages.len(), 16);
    }
    #[test]
    fn corrupted_middle_page_revokes_the_whole_range() {
        let mut cache = PageCache::new(Ram(vec![0; 16 * PAGE])).unwrap();
        let keys = keys(0, 16);
        assert!(cache.fill_range(&keys, &vec![7; 16 * PAGE]));
        cache.backend.as_mut().unwrap().0[8 * PAGE + 23] ^= 1;
        assert!(!cache.lookup_range(&keys, &mut vec![0; 16 * PAGE]));
        assert!(!cache.is_active());
        assert!(cache.last_error().unwrap().contains("checksum"));
    }
    #[test]
    fn fragmented_ranges_bypass_without_touching_output() {
        let mut cache = PageCache::new(Ram(vec![0; 32 * PAGE])).unwrap();
        let keys = keys(0, 8);
        for key in &keys {
            assert!(cache.fill(*key, &[1; PAGE]));
            assert!(cache.fill(
                PageKey {
                    page_index: 100 + key.page_index,
                    ..*key
                },
                &[2; PAGE]
            ));
        }
        let mut output = vec![3; 8 * PAGE];
        assert!(!cache.lookup_range(&keys, &mut output));
        assert!(output.iter().all(|&b| b == 3));
        assert!(cache.is_active());
    }
    struct Ram(Vec<u8>);
    impl BlockIo for Ram {
        fn capacity(&self) -> u64 {
            self.0.len() as u64
        }
        fn read(&mut self, offset: u64, page: &mut [u8; PAGE]) -> Result<()> {
            page.copy_from_slice(&self.0[offset as usize..offset as usize + PAGE]);
            Ok(())
        }
        fn write(&mut self, offset: u64, page: &[u8; PAGE]) -> Result<()> {
            self.0[offset as usize..offset as usize + PAGE].copy_from_slice(page);
            Ok(())
        }
        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
    }
    fn key(generation: u64) -> PageKey {
        PageKey {
            volume_epoch: 1,
            page_index: 2,
            generation,
        }
    }
    #[test]
    fn generations_and_volume_epochs_do_not_alias() {
        let mut cache = PageCache::new(Ram(vec![0; PAGE * 2])).unwrap();
        assert!(cache.fill(key(1), &[7; PAGE]));
        let mut output = [0; PAGE];
        assert!(!cache.lookup(key(2), &mut output));
        assert!(!cache.lookup(
            PageKey {
                volume_epoch: 2,
                ..key(1)
            },
            &mut output
        ));
        assert!(cache.lookup(key(1), &mut output));
        assert_eq!(output, [7; PAGE]);
    }
    #[test]
    fn repeated_fill_does_not_leave_duplicate_slot_owner() {
        let mut cache = PageCache::new(Ram(vec![0; PAGE * 2])).unwrap();
        cache.fill(key(1), &[1; PAGE]);
        cache.fill(key(1), &[2; PAGE]);
        cache.fill(key(2), &[3; PAGE]);
        let mut output = [0; PAGE];
        assert!(cache.lookup(key(1), &mut output));
        assert_eq!(output, [2; PAGE]);
    }
    #[test]
    fn revoked_cache_only_returns_misses() {
        let mut cache = PageCache::new(Ram(vec![0; PAGE])).unwrap();
        cache.fill(key(1), &[4; PAGE]);
        cache.revoke();
        assert!(!cache.lookup(key(1), &mut [0; PAGE]));
        assert!(!cache.fill(key(2), &[5; PAGE]));
    }
}
