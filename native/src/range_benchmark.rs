//! Local, real-CUDA range-cache checks. Never installs/intercepts Windows I/O.
use crate::cache::{BlockIo, PAGE};
use crate::page_cache::{PageCache, PageKey};
use crate::store::{MIB, Result, VramStore};
use std::cell::Cell;
use std::io::Write;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;
struct CountedGpu {
    gpu: VramStore,
    reads: Rc<Cell<u64>>,
    writes: Rc<Cell<u64>>,
}
impl BlockIo for CountedGpu {
    fn capacity(&self) -> u64 {
        self.gpu.capacity
    }
    fn read(&mut self, offset: u64, data: &mut [u8; PAGE]) -> Result<()> {
        self.read_blocks(offset, data)
    }
    fn write(&mut self, offset: u64, data: &[u8; PAGE]) -> Result<()> {
        self.write_blocks(offset, data)
    }
    fn read_blocks(&mut self, offset: u64, data: &mut [u8]) -> Result<()> {
        self.reads.set(self.reads.get() + 1);
        self.gpu.read(offset, data)
    }
    fn write_blocks(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        self.writes.set(self.writes.get() + 1);
        self.gpu.write(offset, data)
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
fn keys(group: usize, index: usize, pages: usize) -> Vec<PageKey> {
    (0..pages)
        .map(|page| PageKey {
            volume_epoch: 7,
            page_index: (group * 1048576 + index * 16 + page) as u64,
            generation: 3 + page as u64,
        })
        .collect()
}
fn pattern(first: u64, index: usize) -> [u8; 8] {
    first
        .wrapping_mul(0x9e3779b97f4a7c15)
        .wrapping_add(index as u64 * 0x10001)
        .to_le_bytes()
}
pub fn run(output: &Path) -> Result<()> {
    const RANGES: usize = 128;
    const SAMPLES: usize = 512;
    let reads = Rc::new(Cell::new(0));
    let writes = Rc::new(Cell::new(0));
    let gpu = CountedGpu {
        gpu: VramStore::new(0, 16 * MIB)?,
        reads: reads.clone(),
        writes: writes.clone(),
    };
    let mut cache = PageCache::new(gpu)?;
    let mut rows = Vec::new();
    for (group, pages) in [1, 2, 4, 8, 16].into_iter().enumerate() {
        let length = pages * PAGE;
        let mut data = vec![0u8; length];
        let start_writes = writes.get();
        for index in 0..RANGES {
            let keys = keys(group, index, pages);
            for (index, word) in data.chunks_exact_mut(8).enumerate() {
                word.copy_from_slice(&pattern(keys[0].page_index, index));
            }
            if !cache.fill_range(&keys, &data) {
                return Err("Real GPU range fill failed".into());
            }
        }
        if writes.get() - start_writes != RANGES as u64 {
            return Err("A contiguous range was not filled with one backend transfer".into());
        }
        let mut stale = keys(group, 0, pages);
        stale[pages / 2].generation += 1;
        let before = reads.get();
        data.fill(0x55);
        if cache.lookup_range(&stale, &mut data)
            || reads.get() != before
            || data.iter().any(|&byte| byte != 0x55)
        {
            return Err("Partial/stale range started GPU I/O or modified output".into());
        }
        let mut samples = Vec::with_capacity(SAMPLES);
        let start_reads = reads.get();
        for index in 0..SAMPLES {
            let keys = keys(group, index * 73 % RANGES, pages);
            let started = Instant::now();
            if !cache.lookup_range(&keys, &mut data) {
                return Err(format!(
                    "Real GPU range hit missing: bytes={length}, index={index}, active={}, reads={}, error={:?}",
                    cache.is_active(),
                    reads.get(),
                    cache.last_error()
                ));
            }
            samples.push(started.elapsed().as_nanos() as f64 / 1000.0);
            if data
                .chunks_exact(8)
                .enumerate()
                .any(|(index, word)| word != pattern(keys[0].page_index, index))
            {
                return Err(format!("Real GPU range payload mismatch: {length} bytes"));
            }
        }
        let copies = reads.get() - start_reads;
        if copies != SAMPLES as u64 {
            return Err("Contiguous range lookup used more than one GPU transfer".into());
        }
        samples.sort_by(f64::total_cmp);
        let p50 = samples[255];
        let p95 = samples[486];
        let p99 = samples[506];
        let maximum = samples[511];
        println!(
            "Local CUDA {length} bytes: {SAMPLES} integrity-checked hits, {copies} GPU reads; p50={p50:.1}, p99={p99:.1} us"
        );
        rows.push(format!("{{\"bytes\":{length},\"samples\":{SAMPLES},\"cudaReadCalls\":{copies},\"p50Us\":{p50:.3},\"p95Us\":{p95:.3},\"p99Us\":{p99:.3},\"maxUs\":{maximum:.3}}}"));
    }
    cache.revoke();
    if cache.is_active() || cache.lookup(keys(0, 0, 1)[0], &mut [0; PAGE]) {
        return Err("Range cache did not revoke".into());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    writeln!(file,"{{\"schema\":1,\"source\":\"same-process real CUDA PageCache\",\"vramBytes\":{},\"partialMissAvoidedGpuIo\":true,\"cacheRevoked\":true,\"kernelPagingVerified\":false,\"gameBenefitProven\":false,\"results\":[{}]}}",16*MIB,rows.join(",")).map_err(|e|e.to_string())?;
    println!(
        "PASS real GPU ranges, exact transfer counts, partial/stale miss and cache revocation; not kernel paging or FPS: {}",
        output.display()
    );
    Ok(())
}
