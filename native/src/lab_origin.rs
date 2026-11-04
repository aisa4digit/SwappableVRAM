//! Bounded direct-I/O origin for development only; never opens an existing file
//! or physical disk. The owned test file is removed when this object is dropped.
use crate::cache::{BlockIo, PAGE};
use crate::store::{MIB, Result, validate_range};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[repr(align(4096))]
struct AlignedPage([u8; PAGE]);
pub struct LabOrigin {
    file: Option<File>,
    path: PathBuf,
    capacity: u64,
    staging: Box<AlignedPage>,
}
impl LabOrigin {
    pub fn create_new(path: &Path, capacity: u64) -> Result<Self> {
        if path.extension().and_then(|s| s.to_str()) != Some("svram-lab") {
            return Err("Lab origin must have .svram-lab extension".into());
        }
        if !(MIB..=512 * MIB).contains(&capacity) || !capacity.is_multiple_of(PAGE as u64) {
            return Err("Lab origin must be 1–512 MiB, 4 KiB aligned".into());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(0)
            .custom_flags(0xB0000000) // NO_BUFFERING | WRITE_THROUGH | RANDOM_ACCESS
            .open(path)
            .map_err(|e| format!("Create lab origin: {e}"))?;
        let origin = Self {
            file: Some(file),
            path: path.to_path_buf(),
            capacity,
            staging: Box::new(AlignedPage([0; PAGE])),
        };
        origin
            .file
            .as_ref()
            .ok_or("Missing origin handle")?
            .set_len(capacity)
            .map_err(|e| e.to_string())?;
        Ok(origin)
    }
}
impl BlockIo for LabOrigin {
    fn capacity(&self) -> u64 {
        self.capacity
    }
    fn read(&mut self, offset: u64, page: &mut [u8; PAGE]) -> Result<()> {
        validate_range(offset, PAGE as u64, self.capacity)?;
        if !offset.is_multiple_of(PAGE as u64) {
            return Err("Unaligned origin read".into());
        }
        let file = self.file.as_mut().ok_or("Origin closed")?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        file.read_exact(&mut self.staging.0)
            .map_err(|e| e.to_string())?;
        page.copy_from_slice(&self.staging.0);
        Ok(())
    }
    fn write(&mut self, offset: u64, page: &[u8; PAGE]) -> Result<()> {
        validate_range(offset, PAGE as u64, self.capacity)?;
        if !offset.is_multiple_of(PAGE as u64) {
            return Err("Unaligned origin write".into());
        }
        self.staging.0.copy_from_slice(page);
        let file = self.file.as_mut().ok_or("Origin closed")?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        file.write_all(&self.staging.0).map_err(|e| e.to_string())
    }
    fn flush(&mut self) -> Result<()> {
        self.file
            .as_ref()
            .ok_or("Origin closed")?
            .sync_data()
            .map_err(|e| e.to_string())
    }
}
impl Drop for LabOrigin {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}
