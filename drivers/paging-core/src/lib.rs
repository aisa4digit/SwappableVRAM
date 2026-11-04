//! Allocation-free paging state shared with the KMDF bridge.
//!
//! Every call for a device must hold that device's nonpaged spin lock. There are
//! no waits, heap allocations, floating point, or OS calls on the fast path.
#![cfg_attr(not(test), no_std)]

pub const BUCKETS: usize = 4096;
const PAGE: u64 = 4096;
pub const RANGE_PAGES: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Bucket {
    generation: u64,
    writers: u32,
    reserved: u32,
}
#[repr(C)]
pub struct GenerationBook {
    volume_epoch: u64,
    next_generation: u64,
    floor: u64,
    global_writers: u32,
    disabled: u32,
    buckets: [Bucket; BUCKETS],
}
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub volume_epoch: u64,
    pub page_index: u64,
    pub generation: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct RangeStamp {
    pub volume_epoch: u64,
    pub page_index: u64,
    pub pages: u32,
    pub reserved: u32,
    pub generations: [u64; RANGE_PAGES],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WriteTicket {
    generation: u64,
    floor: u64,
}

impl GenerationBook {
    pub fn invalidate_all(&mut self) {
        if let Some(generation) = self.advance() {
            self.floor = generation;
        }
    }
    pub fn write_ticket(&self, offset: u64, length: u64) -> Option<WriteTicket> {
        if self.disabled != 0
            || self.global_writers != 0
            || length == 0
            || !length.is_multiple_of(PAGE)
            || length / PAGE >= BUCKETS as u64
            || !offset.is_multiple_of(PAGE)
            || offset.checked_add(length).is_none()
        {
            return None;
        }
        let bucket = &self.buckets[((offset / PAGE) % BUCKETS as u64) as usize];
        if bucket.writers != 1
            || (0..length / PAGE).any(|index| {
                let current = &self.buckets[((offset / PAGE + index) % BUCKETS as u64) as usize];
                current.writers != 1 || current.generation != bucket.generation
            })
        {
            return None;
        }
        Some(WriteTicket {
            generation: bucket.generation,
            floor: self.floor,
        })
    }
    pub fn ticket_matches(&self, offset: u64, length: u64, ticket: WriteTicket) -> bool {
        self.write_ticket(offset, length).is_some_and(|current| {
            current.generation == ticket.generation && current.floor == ticket.floor
        })
    }
    fn advance(&mut self) -> Option<u64> {
        match self.next_generation.checked_add(1) {
            Some(next) => {
                self.next_generation = next;
                Some(next)
            }
            None => {
                self.disabled = 1;
                None
            }
        }
    }
    fn range(&mut self, offset: u64, length: u64) -> Option<(u64, u64)> {
        if length == 0 {
            return None;
        }
        let Some(last) = offset.checked_add(length - 1) else {
            self.disabled = 1;
            return None;
        };
        Some((offset / PAGE, last / PAGE))
    }
    pub fn begin_write(&mut self, offset: u64, length: u64) {
        if self.disabled != 0 {
            return;
        }
        let Some((first, last)) = self.range(offset, length) else {
            return;
        };
        let Some(generation) = self.advance() else {
            return;
        };
        if last - first + 1 >= BUCKETS as u64 {
            self.floor = generation;
            match self.global_writers.checked_add(1) {
                Some(n) => self.global_writers = n,
                None => self.disabled = 1,
            }
            return;
        }
        for page in first..=last {
            let bucket = &mut self.buckets[(page % BUCKETS as u64) as usize];
            bucket.generation = generation;
            match bucket.writers.checked_add(1) {
                Some(n) => bucket.writers = n,
                None => {
                    self.disabled = 1;
                    return;
                }
            }
        }
    }
    pub fn end_write(&mut self, offset: u64, length: u64) {
        if self.disabled != 0 {
            return;
        }
        let Some((first, last)) = self.range(offset, length) else {
            return;
        };
        let Some(generation) = self.advance() else {
            return;
        };
        if last - first + 1 >= BUCKETS as u64 {
            self.floor = generation;
            match self.global_writers.checked_sub(1) {
                Some(n) => self.global_writers = n,
                None => self.disabled = 1,
            }
            return;
        }
        for page in first..=last {
            let bucket = &mut self.buckets[(page % BUCKETS as u64) as usize];
            bucket.generation = generation;
            match bucket.writers.checked_sub(1) {
                Some(n) => bucket.writers = n,
                None => {
                    self.disabled = 1;
                    return;
                }
            }
        }
    }
    pub fn stamp(&self, offset: u64) -> Option<Stamp> {
        if self.disabled != 0 || self.global_writers != 0 || !offset.is_multiple_of(PAGE) {
            return None;
        }
        let page_index = offset / PAGE;
        let bucket = &self.buckets[(page_index % BUCKETS as u64) as usize];
        if bucket.writers != 0 {
            return None;
        }
        Some(Stamp {
            volume_epoch: self.volume_epoch,
            page_index,
            generation: bucket.generation.max(self.floor),
        })
    }
    pub fn range_stamp(&self, offset: u64, length: u64) -> Option<RangeStamp> {
        if length == 0
            || !length.is_multiple_of(PAGE)
            || length / PAGE > RANGE_PAGES as u64
            || offset.checked_add(length).is_none()
        {
            return None;
        }
        let first = self.stamp(offset)?;
        let mut range = RangeStamp {
            volume_epoch: first.volume_epoch,
            page_index: first.page_index,
            pages: (length / PAGE) as u32,
            ..RangeStamp::default()
        };
        for (index, generation) in range.generations[..range.pages as usize]
            .iter_mut()
            .enumerate()
        {
            *generation = self.stamp(offset + index as u64 * PAGE)?.generation;
        }
        Some(range)
    }
}

pub const WAITING: u32 = 1;
pub const CACHE: u32 = 2;
pub const DISK: u32 = 3;
pub const CANCEL: u32 = 4;
pub const COMPLETE: u32 = 5;
#[repr(C)]
#[derive(Default)]
pub struct RequestState {
    sequence: u64,
    generation: u64,
    deadline: u64,
    state: u32,
    reserved: u32,
}
impl RequestState {
    pub fn prepare(&mut self, sequence: u64, generation: u64, deadline: u64) {
        *self = Self {
            sequence,
            generation,
            deadline,
            state: WAITING,
            reserved: 0,
        };
    }
    pub fn claim_hit(&mut self, sequence: u64, generation: u64, now: u64) -> bool {
        if self.state != WAITING
            || self.sequence != sequence
            || self.generation != generation
            || now >= self.deadline
        {
            return false;
        }
        self.state = CACHE;
        true
    }
    pub fn claim_disk(&mut self) -> bool {
        if self.state != WAITING {
            return false;
        }
        self.state = DISK;
        true
    }
    pub fn cancel(&mut self) -> bool {
        if self.state != WAITING {
            return false;
        }
        self.state = CANCEL;
        true
    }
    pub fn complete(&mut self) -> bool {
        if !matches!(self.state, CACHE | DISK | CANCEL) {
            return false;
        }
        self.state = COMPLETE;
        true
    }
}

// Trusted kernel C ABI; all pointers are driver-owned nonpaged memory and calls
// are serialized by its spin lock. ABI sizes are checked in protocol.h.
/// # Safety
/// `book` is writable, aligned nonpaged memory for GenerationBook; no concurrent access.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_book_init(book: *mut GenerationBook, volume_epoch: u64) {
    if book.is_null() {
        return;
    }
    // Avoid a 64 KiB temporary on the limited kernel stack.
    unsafe {
        core::ptr::write_bytes(book, 0, 1);
        (*book).volume_epoch = volume_epoch;
        (*book).next_generation = 1;
        (*book).floor = 1;
    }
}
/// # Safety
/// `book` is initialized nonpaged memory exclusively held by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_book_begin_write(book: *mut GenerationBook, offset: u64, length: u64) {
    if let Some(book) = unsafe { book.as_mut() } {
        book.begin_write(offset, length);
    }
}
/// # Safety
/// Same as begin_write; must be paired once for each forwarded write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_book_end_write(book: *mut GenerationBook, offset: u64, length: u64) {
    if let Some(book) = unsafe { book.as_mut() } {
        book.end_write(offset, length);
    }
}
/// # Safety
/// Book is initialized/exclusively locked nonpaged driver memory. Invalidation
/// preserves writer counts: outstanding lower operations must still finish.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_book_invalidate_all(book: *mut GenerationBook) {
    if let Some(book) = unsafe { book.as_mut() } {
        book.invalidate_all();
    }
}
/// # Safety
/// `book` is initialized and locked; stamp is writable nonpaged memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_book_stamp(
    book: *const GenerationBook,
    offset: u64,
    stamp: *mut Stamp,
) -> u32 {
    if book.is_null() || stamp.is_null() {
        return 0;
    }
    if let Some(value) = unsafe { &*book }.stamp(offset) {
        unsafe {
            *stamp = value;
        }
        1
    } else {
        0
    }
}
/// # Safety
/// Book is locked, initialized nonpaged storage; output is writable/aligned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_book_range_stamp(
    book: *const GenerationBook,
    offset: u64,
    length: u64,
    output: *mut RangeStamp,
) -> u32 {
    if book.is_null() || output.is_null() {
        return 0;
    }
    if let Some(value) = unsafe { &*book }.range_stamp(offset, length) {
        unsafe {
            *output = value;
        }
        1
    } else {
        0
    }
}

/// # Safety
/// The book and ticket are driver-owned, initialized/aligned nonpaged memory;
/// caller holds the device spin lock. Ticket is captured immediately after begin.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_book_write_ticket(
    book: *const GenerationBook,
    offset: u64,
    length: u64,
    ticket: *mut WriteTicket,
) -> u32 {
    if book.is_null() || ticket.is_null() {
        return 0;
    }
    if let Some(value) = unsafe { &*book }.write_ticket(offset, length) {
        unsafe {
            *ticket = value;
        }
        1
    } else {
        0
    }
}
/// # Safety
/// Same as write_ticket. Returns true only if no overlapping/colliding write
/// or global barrier intervened since the ticket was captured.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_book_ticket_matches(
    book: *const GenerationBook,
    offset: u64,
    length: u64,
    ticket: *const WriteTicket,
) -> u32 {
    if book.is_null() || ticket.is_null() {
        return 0;
    }
    u32::from(unsafe { &*book }.ticket_matches(offset, length, unsafe { *ticket }))
}
/// # Safety
/// State is exclusively held initialized/nonpaged driver memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_request_prepare(
    state: *mut RequestState,
    sequence: u64,
    generation: u64,
    deadline: u64,
) {
    if let Some(state) = unsafe { state.as_mut() } {
        state.prepare(sequence, generation, deadline);
    }
}
/// # Safety
/// State is exclusively held initialized/nonpaged driver memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_request_claim_hit(
    state: *mut RequestState,
    sequence: u64,
    generation: u64,
    now: u64,
) -> u32 {
    unsafe { state.as_mut() }.map_or(0, |state| {
        u32::from(state.claim_hit(sequence, generation, now))
    })
}
/// # Safety
/// State is exclusively held initialized/nonpaged driver memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_request_claim_disk(state: *mut RequestState) -> u32 {
    unsafe { state.as_mut() }.map_or(0, |state| u32::from(state.claim_disk()))
}
/// # Safety
/// State is exclusively held initialized/nonpaged driver memory. The caller
/// must also own cancellation according to the KMDF request contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sv_request_claim_cancel(state: *mut RequestState) -> u32 {
    unsafe { state.as_mut() }.map_or(0, |state| u32::from(state.cancel()))
}

#[cfg(not(test))]
unsafe extern "C" {
    fn SvDriverPanic() -> !;
}
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    unsafe { SvDriverPanic() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn range_checks_every_generation_and_wraps_bucket_index_safely() {
        let mut book = book();
        let offset = (BUCKETS as u64 - 8) * PAGE;
        let before = book.range_stamp(offset, 16 * PAGE).unwrap();
        book.begin_write(offset + 7 * PAGE, 2 * PAGE);
        assert!(book.range_stamp(offset, 16 * PAGE).is_none());
        book.end_write(offset + 7 * PAGE, 2 * PAGE);
        let after = book.range_stamp(offset, 16 * PAGE).unwrap();
        assert_eq!(after.generations[0], before.generations[0]);
        assert_ne!(after.generations[7], before.generations[7]);
        assert_ne!(after.generations[8], before.generations[8]);
        assert_eq!(after.generations[15], before.generations[15]);
        assert_eq!(core::mem::size_of::<RangeStamp>(), 152);
    }
    #[test]
    fn clustered_write_ticket_rejects_any_overlapping_completion_or_barrier() {
        let mut book = book();
        book.begin_write(0, 256 * PAGE);
        let ticket = book.write_ticket(0, 256 * PAGE).unwrap();
        assert!(book.ticket_matches(0, 256 * PAGE, ticket));
        book.begin_write(255 * PAGE, PAGE);
        assert!(!book.ticket_matches(0, 256 * PAGE, ticket));
        book.end_write(255 * PAGE, PAGE);
        assert!(!book.ticket_matches(0, 256 * PAGE, ticket));
        book.end_write(0, 256 * PAGE);
        let first = book.range_stamp(0, 16 * PAGE).unwrap();
        let last = book.range_stamp(240 * PAGE, 16 * PAGE).unwrap();
        assert_eq!(first.generations, last.generations);
        book.begin_write(0, 256 * PAGE);
        let ticket = book.write_ticket(0, 256 * PAGE).unwrap();
        book.invalidate_all();
        assert!(!book.ticket_matches(0, 256 * PAGE, ticket));
    }
    #[test]
    fn invalid_ranges_are_never_partially_stamped() {
        let book = book();
        for (offset, length) in [
            (0, 0),
            (1, PAGE),
            (0, PAGE + 1),
            (0, 17 * PAGE),
            (u64::MAX - 4095, PAGE),
        ] {
            assert!(book.range_stamp(offset, length).is_none());
        }
    }
    fn book() -> Box<GenerationBook> {
        let mut allocation = Box::<GenerationBook>::new_uninit();
        unsafe {
            sv_book_init(allocation.as_mut_ptr(), 42);
            allocation.assume_init()
        }
    }
    #[test]
    fn writes_block_only_affected_buckets_and_change_generation() {
        let mut book = book();
        let before = book.stamp(0).unwrap();
        book.begin_write(0, PAGE);
        assert!(book.stamp(0).is_none());
        assert!(book.stamp(PAGE).is_some());
        book.end_write(0, PAGE);
        assert_ne!(book.stamp(0).unwrap(), before);
    }
    #[test]
    fn overlapping_writes_must_all_finish() {
        let mut book = book();
        book.begin_write(0, PAGE);
        book.begin_write(0, PAGE);
        book.end_write(0, PAGE);
        assert!(book.stamp(0).is_none());
        book.end_write(0, PAGE);
        assert!(book.stamp(0).is_some());
    }
    #[test]
    fn unaligned_write_invalidates_both_touched_pages() {
        let mut book = book();
        book.begin_write(PAGE - 1, 2);
        assert!(book.stamp(0).is_none());
        assert!(book.stamp(PAGE).is_none());
        book.end_write(PAGE - 1, 2);
        assert!(book.stamp(0).is_some());
        assert!(book.stamp(PAGE).is_some());
    }
    #[test]
    fn large_write_blocks_all_reads_and_invalidates_all_old_stamps() {
        let mut book = book();
        let before = book.stamp(80 * PAGE).unwrap();
        book.begin_write(0, BUCKETS as u64 * PAGE);
        assert!(book.stamp(80 * PAGE).is_none());
        book.end_write(0, BUCKETS as u64 * PAGE);
        assert_ne!(book.stamp(80 * PAGE).unwrap(), before);
    }
    #[test]
    fn generation_wrap_or_unpaired_completion_disables_cache() {
        let mut a = book();
        a.next_generation = u64::MAX;
        a.begin_write(0, PAGE);
        assert!(a.stamp(PAGE).is_none());
        let mut b = book();
        b.end_write(0, PAGE);
        assert!(b.stamp(PAGE).is_none());
        let mut c = book();
        c.begin_write(u64::MAX, PAGE);
        assert!(c.stamp(PAGE).is_none());
    }
    #[test]
    fn collisions_invalidate_conservatively_instead_of_returning_stale_data() {
        let mut book = book();
        let before = book.stamp(0).unwrap();
        book.begin_write(BUCKETS as u64 * PAGE, PAGE);
        assert!(book.stamp(0).is_none());
        book.end_write(BUCKETS as u64 * PAGE, PAGE);
        assert_ne!(book.stamp(0).unwrap(), before);
    }
    #[test]
    fn hit_timeout_cancel_permutations_have_one_owner() {
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let mut request = RequestState::default();
            request.prepare(7, 8, 100);
            let mut winners = 0;
            for event in order {
                winners += u32::from(match event {
                    0 => request.claim_hit(7, 8, 99),
                    1 => request.claim_disk(),
                    _ => request.cancel(),
                });
            }
            assert_eq!(winners, 1);
            assert!(request.complete());
            assert!(!request.complete());
        }
    }
    #[test]
    fn late_stale_or_wrong_generation_reply_never_claims_request() {
        let mut request = RequestState::default();
        request.prepare(7, 8, 100);
        assert!(!request.claim_hit(6, 8, 50));
        assert!(!request.claim_hit(7, 9, 50));
        assert!(!request.claim_hit(7, 8, 100));
        assert!(request.claim_disk());
        assert!(!request.claim_hit(7, 8, 50));
    }
    #[test]
    fn abi_sizes_match_c() {
        assert_eq!(core::mem::size_of::<Bucket>(), 16);
        assert_eq!(core::mem::size_of::<GenerationBook>(), 65568);
        assert_eq!(core::mem::size_of::<Stamp>(), 24);
        assert_eq!(core::mem::size_of::<RequestState>(), 32);
    }
    #[test]
    fn completed_overlapping_write_cannot_fill_old_payload_as_current() {
        let mut book = book();
        book.begin_write(0, PAGE);
        let ticket = book.write_ticket(0, PAGE).unwrap();
        book.begin_write(0, PAGE);
        book.end_write(0, PAGE);
        assert!(!book.ticket_matches(0, PAGE, ticket));
        book.end_write(0, PAGE);
    }
    #[test]
    fn completed_global_barrier_invalidates_inflight_write_ticket() {
        let mut book = book();
        book.begin_write(0, PAGE);
        let ticket = book.write_ticket(0, PAGE).unwrap();
        book.begin_write(0, BUCKETS as u64 * PAGE);
        book.end_write(0, BUCKETS as u64 * PAGE);
        assert!(!book.ticket_matches(0, PAGE, ticket));
        book.end_write(0, PAGE);
    }
    #[test]
    fn exclusive_write_ticket_is_valid_until_its_completion() {
        let mut book = book();
        book.begin_write(0, PAGE);
        let ticket = book.write_ticket(0, PAGE).unwrap();
        assert!(book.ticket_matches(0, PAGE, ticket));
        book.end_write(0, PAGE);
        assert!(!book.ticket_matches(0, PAGE, ticket));
    }
    #[test]
    fn power_invalidation_preserves_inflight_writer_barrier() {
        let mut book = book();
        let old = book.stamp(PAGE).unwrap();
        book.begin_write(0, PAGE);
        book.invalidate_all();
        assert!(book.stamp(0).is_none());
        assert_ne!(book.stamp(PAGE).unwrap(), old);
        book.end_write(0, PAGE);
        assert!(book.stamp(0).is_some());
    }
}
