//! SSD-authoritative, revocable 4 KiB page cache for the future block transport.
//! This module does NOT intercept Windows paging I/O. Its correctness contract
//! is useful independently of the eventual, separately qualified kernel path.
use crate::store::{Result, VramStore};
use std::collections::HashMap;

pub const PAGE: usize = 4096;
pub trait BlockIo {
    fn capacity(&self) -> u64;
    fn read(&mut self, offset: u64, page: &mut [u8; PAGE]) -> Result<()>;
    fn write(&mut self, offset: u64, page: &[u8; PAGE]) -> Result<()>;
    fn read_blocks(&mut self, offset: u64, data: &mut [u8]) -> Result<()> {
        validate_blocks(offset, data.len(), self.capacity())?;
        for (index, page) in data.chunks_exact_mut(PAGE).enumerate() {
            self.read(
                offset + (index * PAGE) as u64,
                page.try_into().map_err(|_| "Page size")?,
            )?;
        }
        Ok(())
    }
    fn write_blocks(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        validate_blocks(offset, data.len(), self.capacity())?;
        for (index, page) in data.chunks_exact(PAGE).enumerate() {
            self.write(
                offset + (index * PAGE) as u64,
                page.try_into().map_err(|_| "Page size")?,
            )?;
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<()>;
}
fn validate_blocks(offset: u64, length: usize, capacity: u64) -> Result<()> {
    if !offset.is_multiple_of(PAGE as u64) || length == 0 || !length.is_multiple_of(PAGE) {
        return Err("Block transfer must contain aligned whole pages".into());
    }
    crate::store::validate_range(offset, length as u64, capacity)
}
impl BlockIo for VramStore {
    fn capacity(&self) -> u64 {
        self.capacity
    }
    fn read(&mut self, offset: u64, page: &mut [u8; PAGE]) -> Result<()> {
        VramStore::read(self, offset, page)
    }
    fn write(&mut self, offset: u64, page: &[u8; PAGE]) -> Result<()> {
        VramStore::write(self, offset, page)
    }
    fn read_blocks(&mut self, offset: u64, data: &mut [u8]) -> Result<()> {
        validate_blocks(offset, data.len(), self.capacity)?;
        VramStore::read(self, offset, data)
    }
    fn write_blocks(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        validate_blocks(offset, data.len(), self.capacity)?;
        VramStore::write(self, offset, data)
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    } // Writes synchronize the CUDA stream, not durable storage.
}
#[derive(Clone, Copy)]
struct Entry {
    slot: usize,
    checksum: u64,
}
#[derive(Default, Debug)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub origin_writes: u64,
    pub gpu_errors: u64,
    pub integrity_fallbacks: u64,
}
pub struct CachedOrigin<O: BlockIo, C: BlockIo> {
    origin: O,
    cache: Option<C>,
    pages: HashMap<u64, Entry>,
    slots: Vec<Option<u64>>,
    next: usize,
    pub stats: CacheStats,
}
impl<O: BlockIo, C: BlockIo> CachedOrigin<O, C> {
    pub fn new(origin: O, cache: C) -> Result<Self> {
        let count =
            usize::try_from(cache.capacity() / PAGE as u64).map_err(|_| "Cache too large")?;
        if count == 0 || origin.capacity() < PAGE as u64 {
            return Err("Empty origin/cache".into());
        }
        let mut pages = HashMap::new();
        pages.try_reserve(count).map_err(|e| e.to_string())?;
        let mut slots = Vec::new();
        slots.try_reserve_exact(count).map_err(|e| e.to_string())?;
        slots.resize(count, None);
        Ok(Self {
            origin,
            cache: Some(cache),
            pages,
            slots,
            next: 0,
            stats: CacheStats::default(),
        })
    }
    pub fn read_page(&mut self, page_index: u64, data: &mut [u8; PAGE]) -> Result<()> {
        let offset = self.offset(page_index)?;
        if let Some(entry) = self.pages.get(&page_index).copied() {
            if let Some(cache) = self.cache.as_mut() {
                match cache.read((entry.slot * PAGE) as u64, data) {
                    Ok(()) if checksum(data) == entry.checksum => {
                        self.stats.hits += 1;
                        return Ok(());
                    }
                    Ok(()) => {
                        self.stats.integrity_fallbacks += 1;
                    }
                    Err(_) => {
                        self.stats.gpu_errors += 1;
                    }
                }
            }
            self.release_cache();
        }
        self.stats.misses += 1;
        self.origin.read(offset, data)?;
        self.fill(page_index, data);
        Ok(())
    }
    pub fn write_page(&mut self, page_index: u64, data: &[u8; PAGE]) -> Result<()> {
        let offset = self.offset(page_index)?;
        // An origin write may partially succeed before returning an error. Never
        // retain a now-stale cache entry if origin write/flush fails.
        self.invalidate(page_index);
        self.origin.write(offset, data)?;
        self.origin.flush()?;
        self.stats.origin_writes += 1;
        self.fill(page_index, data);
        Ok(())
    }
    pub fn release_cache(&mut self) {
        self.pages.clear();
        self.slots.fill(None);
        self.cache.take(); // All acknowledged writes already belong to origin.
    }
    pub fn cached_pages(&self) -> usize {
        self.pages.len()
    }
    pub fn cache_enabled(&self) -> bool {
        self.cache.is_some()
    }
    fn offset(&self, index: u64) -> Result<u64> {
        let offset = index
            .checked_mul(PAGE as u64)
            .ok_or("Page index overflow")?;
        crate::store::validate_range(offset, PAGE as u64, self.origin.capacity())?;
        Ok(offset)
    }
    fn invalidate(&mut self, index: u64) {
        if let Some(entry) = self.pages.remove(&index) {
            self.slots[entry.slot] = None;
        }
    }
    fn fill(&mut self, index: u64, data: &[u8; PAGE]) {
        let Some(cache) = self.cache.as_mut() else {
            return;
        };
        let slot = self.next;
        self.next = (self.next + 1) % self.slots.len();
        if let Some(old) = self.slots[slot].take() {
            self.pages.remove(&old);
        }
        if cache.write((slot * PAGE) as u64, data).is_err() {
            self.stats.gpu_errors += 1;
            self.release_cache();
            return;
        }
        self.slots[slot] = Some(index);
        self.pages.insert(
            index,
            Entry {
                slot,
                checksum: checksum(data),
            },
        );
    }
}
// Detection of accidental corruption only; not an adversarial integrity scheme.
fn checksum(page: &[u8; PAGE]) -> u64 {
    page.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Memory {
        bytes: Vec<u8>,
        fail_read: bool,
        fail_write: bool,
        fail_flush: bool,
        corrupt: bool,
    }
    impl Memory {
        fn new(pages: usize) -> Self {
            Self {
                bytes: vec![0; pages * PAGE],
                fail_read: false,
                fail_write: false,
                fail_flush: false,
                corrupt: false,
            }
        }
    }
    impl BlockIo for Memory {
        fn capacity(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn read(&mut self, offset: u64, page: &mut [u8; PAGE]) -> Result<()> {
            if self.fail_read {
                return Err("injected read failure".into());
            }
            page.copy_from_slice(&self.bytes[offset as usize..offset as usize + PAGE]);
            if self.corrupt {
                page[42] ^= 1;
            }
            Ok(())
        }
        fn write(&mut self, offset: u64, page: &[u8; PAGE]) -> Result<()> {
            if self.fail_write {
                return Err("injected write failure".into());
            }
            self.bytes[offset as usize..offset as usize + PAGE].copy_from_slice(page);
            Ok(())
        }
        fn flush(&mut self) -> Result<()> {
            if self.fail_flush {
                Err("injected flush failure".into())
            } else {
                Ok(())
            }
        }
    }
    fn fixture() -> CachedOrigin<Memory, Memory> {
        CachedOrigin::new(Memory::new(8), Memory::new(2)).unwrap()
    }
    #[test]
    fn write_through_survives_revocation() {
        let mut tier = fixture();
        tier.write_page(0, &[17; PAGE]).unwrap();
        tier.release_cache();
        let mut read = [0; PAGE];
        tier.read_page(0, &mut read).unwrap();
        assert_eq!(read, [17; PAGE]);
    }
    #[test]
    fn eviction_does_not_lose_pages() {
        let mut tier = fixture();
        for p in 0..8 {
            tier.write_page(p, &[p as u8; PAGE]).unwrap();
        }
        assert_eq!(tier.cached_pages(), 2);
        for p in 0..8 {
            let mut read = [0; PAGE];
            tier.read_page(p, &mut read).unwrap();
            assert_eq!(read, [p as u8; PAGE]);
        }
    }
    #[test]
    fn gpu_read_error_falls_back_to_origin() {
        let mut tier = fixture();
        tier.write_page(0, &[91; PAGE]).unwrap();
        tier.cache.as_mut().unwrap().fail_read = true;
        let mut read = [0; PAGE];
        tier.read_page(0, &mut read).unwrap();
        assert_eq!(read, [91; PAGE]);
        assert!(!tier.cache_enabled());
        assert_eq!(tier.stats.gpu_errors, 1);
    }
    #[test]
    fn gpu_write_error_does_not_fail_committed_origin_write() {
        let mut tier = fixture();
        tier.cache.as_mut().unwrap().fail_write = true;
        tier.write_page(0, &[7; PAGE]).unwrap();
        assert!(!tier.cache_enabled());
        assert_eq!(tier.stats.origin_writes, 1);
    }
    #[test]
    fn silent_gpu_corruption_falls_back_to_origin() {
        let mut tier = fixture();
        tier.write_page(0, &[19; PAGE]).unwrap();
        tier.cache.as_mut().unwrap().corrupt = true;
        let mut read = [0; PAGE];
        tier.read_page(0, &mut read).unwrap();
        assert_eq!(read, [19; PAGE]);
        assert!(!tier.cache_enabled());
        assert_eq!(tier.stats.integrity_fallbacks, 1);
    }
    #[test]
    fn failed_origin_flush_invalidates_old_cache() {
        let mut tier = fixture();
        tier.write_page(0, &[1; PAGE]).unwrap();
        tier.origin.fail_flush = true;
        assert!(tier.write_page(0, &[2; PAGE]).is_err());
        assert_eq!(tier.cached_pages(), 0);
    }
    #[test]
    fn origin_errors_are_never_acknowledged_as_success() {
        let mut tier = fixture();
        tier.origin.fail_write = true;
        assert!(tier.write_page(0, &[1; PAGE]).is_err());
        assert_eq!(tier.stats.origin_writes, 0);
        assert_eq!(tier.cached_pages(), 0);
    }
    #[test]
    fn invalid_page_does_not_wrap() {
        assert!(fixture().read_page(u64::MAX, &mut [0; PAGE]).is_err());
    }
}
