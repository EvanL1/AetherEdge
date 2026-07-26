//! Memory-mapped file implementation for Linux.
//!
//! This module provides mmap-based shared memory for Linux systems,
//! enabling zero-copy data sharing between io and automation.

use memmap2::{MmapMut, MmapOptions};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::DEFAULT_SHM_PATH;
use crate::traits::{ShmOps, ShmOpsExt};
use aether_core::shm::{HEADER_SIZE, PointSlot, SLOT_SIZE, ShmHeader, shm_size, slot_flags};

/// Shared memory error.
#[derive(Debug)]
pub enum ShmError {
    /// I/O error.
    Io(io::Error),
    /// Invalid shared memory (magic number mismatch).
    InvalidMagic,
    /// Version mismatch.
    VersionMismatch,
    /// File too small.
    FileTooSmall,
}

impl std::fmt::Display for ShmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::InvalidMagic => write!(f, "Invalid shared memory magic number"),
            Self::VersionMismatch => write!(f, "Shared memory version mismatch"),
            Self::FileTooSmall => write!(f, "Shared memory file too small"),
        }
    }
}

impl std::error::Error for ShmError {}

impl From<io::Error> for ShmError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Shared memory configuration.
#[derive(Debug, Clone)]
pub struct ShmConfig {
    /// Path to shared memory file.
    pub path: PathBuf,
    /// Maximum number of point slots.
    pub max_slots: u32,
}

impl Default for ShmConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from(DEFAULT_SHM_PATH),
            max_slots: 8192,
        }
    }
}

impl ShmConfig {
    /// Create with custom path.
    pub fn with_path<P: AsRef<Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            ..Default::default()
        }
    }

    /// Create with custom slot count.
    pub fn with_slots(max_slots: u32) -> Self {
        Self {
            max_slots,
            ..Default::default()
        }
    }
}

/// Memory-mapped shared memory writer.
///
/// Creates and manages the shared memory file.
pub struct MmapWriter {
    mmap: MmapMut,
    max_slots: u32,
}

impl MmapWriter {
    /// Create a new shared memory file.
    pub fn create(config: &ShmConfig) -> Result<Self, ShmError> {
        // Create parent directory if needed
        if let Some(parent) = config.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let size = shm_size(config.max_slots);

        // Create or truncate file
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&config.path)?;

        // Set file size
        file.set_len(size as u64)?;

        // SAFETY: File was just created with truncate(true) and set to the exact size
        // shm_size(max_slots). We have exclusive write access.
        let mut mmap = unsafe { MmapOptions::new().len(size).map_mut(&file)? };

        // Initialize header
        let header = mmap.as_mut_ptr() as *mut ShmHeader;
        // SAFETY: mmap region is at least shm_size(max_slots) which includes HEADER_SIZE.
        // Page-aligned mmap base satisfies ShmHeader alignment requirements.
        unsafe {
            (*header).init(config.max_slots);
        }

        // Zero all slots
        for i in 0..config.max_slots {
            // SAFETY: i < max_slots, so HEADER_SIZE + i * SLOT_SIZE is within the mmap
            // region sized to shm_size(max_slots). SLOT_SIZE alignment is correct for PointSlot.
            let slot_ptr = unsafe {
                mmap.as_mut_ptr()
                    .add(HEADER_SIZE + (i as usize) * SLOT_SIZE) as *mut PointSlot
            };
            // SAFETY: slot_ptr is valid and properly aligned (computed above).
            // PointSlot::zeroed() produces a valid bit pattern for the type.
            unsafe {
                std::ptr::write(slot_ptr, PointSlot::zeroed());
            }
        }

        // Flush to ensure visibility
        mmap.flush()?;

        Ok(Self {
            mmap,
            max_slots: config.max_slots,
        })
    }

    /// Open an existing shared memory file for writing.
    pub fn open(config: &ShmConfig) -> Result<Self, ShmError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&config.path)?;

        let metadata = file.metadata()?;
        let min_size = shm_size(1); // At least header + 1 slot

        if metadata.len() < min_size as u64 {
            return Err(ShmError::FileTooSmall);
        }

        // SAFETY: File exists, is opened with read+write, and metadata.len() >= shm_size(1).
        // The file was originally created by MmapWriter::create() with a valid layout.
        let mmap = unsafe { MmapMut::map_mut(&file)? };

        // Validate header
        let header = mmap.as_ptr() as *const ShmHeader;
        // SAFETY: mmap region is at least shm_size(1) which includes HEADER_SIZE.
        // Page-aligned mmap base satisfies ShmHeader alignment.
        unsafe {
            if !(*header).is_valid() {
                return Err(ShmError::InvalidMagic);
            }
        }

        // SAFETY: Header validity was confirmed by is_valid() above; slot_count is a plain u32 read.
        let max_slots = unsafe { (*header).slot_count() };

        // slot_count comes from the file and is untrusted: a truncated or
        // corrupt file can declare more slots than the mmap actually holds,
        // and slot()/slot_mut() bound their offsets by max_slots. Reject
        // here so no slot access can run past the mmap end.
        if metadata.len() < shm_size(max_slots) as u64 {
            return Err(ShmError::FileTooSmall);
        }

        Ok(Self { mmap, max_slots })
    }

    /// Flush changes to disk.
    pub fn flush(&self) -> Result<(), ShmError> {
        self.mmap.flush()?;
        Ok(())
    }

    /// Get a mutable pointer to the header.
    #[inline]
    fn header_mut(&mut self) -> &mut ShmHeader {
        // SAFETY: mmap region starts with a valid ShmHeader (initialized in create() or
        // validated in open()). Page-aligned base satisfies ShmHeader alignment.
        unsafe { &mut *(self.mmap.as_mut_ptr() as *mut ShmHeader) }
    }

    /// Get a pointer to the header.
    #[inline]
    fn header(&self) -> &ShmHeader {
        // SAFETY: mmap region starts with a valid ShmHeader. Page-aligned base
        // satisfies ShmHeader alignment. Single-writer design prevents data races.
        unsafe { &*(self.mmap.as_ptr() as *const ShmHeader) }
    }

    /// Get a mutable reference to a slot.
    #[inline]
    fn slot_mut(&mut self, index: u32) -> Option<&mut PointSlot> {
        if index >= self.max_slots {
            return None;
        }
        let offset = HEADER_SIZE + (index as usize) * SLOT_SIZE;
        // SAFETY: index < max_slots is checked above, and create()/open() verified the
        // mmap covers shm_size(max_slots), so offset is within the mmap region.
        // PointSlot alignment is satisfied by SLOT_SIZE being a multiple of its alignment.
        unsafe { Some(&mut *(self.mmap.as_mut_ptr().add(offset) as *mut PointSlot)) }
    }

    /// Get a reference to a slot.
    #[inline]
    fn slot(&self, index: u32) -> Option<&PointSlot> {
        if index >= self.max_slots {
            return None;
        }
        let offset = HEADER_SIZE + (index as usize) * SLOT_SIZE;
        // SAFETY: index < max_slots is checked above, and create()/open() verified the
        // mmap covers shm_size(max_slots), so offset is within the mmap region.
        // PointSlot alignment is satisfied by SLOT_SIZE.
        unsafe { Some(&*(self.mmap.as_ptr().add(offset) as *const PointSlot)) }
    }
}

impl ShmOps for MmapWriter {
    fn slot_count(&self) -> u32 {
        self.header().slot_count()
    }

    fn is_slot_valid(&self, index: u32) -> bool {
        self.slot(index).map(|s| s.is_valid()).unwrap_or(false)
    }

    fn read_slot(&self, index: u32) -> Option<(f64, u64, u8)> {
        self.slot(index)?.try_read()
    }

    fn read_slot_spin(&self, index: u32) -> (f64, u64, u8) {
        self.slot(index)
            .map(|s| s.read_spin())
            .unwrap_or((0.0, 0, 0))
    }

    fn write_slot(&mut self, index: u32, value: f64, timestamp: u64, quality: u8) {
        if let Some(slot) = self.slot_mut(index) {
            slot.write(value, timestamp, quality);
        }
        self.header_mut().set_last_update(timestamp);
    }

    fn last_update(&self) -> u64 {
        self.header().last_update()
    }
}

impl ShmOpsExt for MmapWriter {
    fn slot_point_id(&self, index: u32) -> Option<u32> {
        self.slot(index).map(|s| s.point_id)
    }

    fn slot_instance_id(&self, index: u32) -> Option<u32> {
        self.slot(index).map(|s| s.instance_id)
    }

    fn slot_point_type(&self, index: u32) -> Option<u8> {
        self.slot(index).map(|s| s.point_type)
    }

    fn set_slot_metadata(&mut self, index: u32, point_id: u32, instance_id: u32, point_type: u8) {
        if let Some(slot) = self.slot_mut(index) {
            slot.point_id = point_id;
            slot.instance_id = instance_id;
            slot.point_type = point_type;
            slot.flags |= slot_flags::VALID;
        }
    }
}

/// Memory-mapped shared memory reader.
///
/// Opens an existing shared memory file for reading.
pub struct MmapReader {
    mmap: memmap2::Mmap,
    max_slots: u32,
}

impl MmapReader {
    /// Open an existing shared memory file for reading.
    pub fn open(config: &ShmConfig) -> Result<Self, ShmError> {
        let file = File::open(&config.path)?;

        let metadata = file.metadata()?;
        let min_size = shm_size(1);

        if metadata.len() < min_size as u64 {
            return Err(ShmError::FileTooSmall);
        }

        // SAFETY: File exists and metadata.len() >= shm_size(1), providing sufficient
        // space. File was created by MmapWriter::create() with a valid layout.
        let mmap = unsafe { MmapOptions::new().map(&file)? };

        // Validate header
        let header = mmap.as_ptr() as *const ShmHeader;
        // SAFETY: mmap region is at least shm_size(1) which includes HEADER_SIZE.
        // Page-aligned mmap base satisfies ShmHeader alignment.
        unsafe {
            if !(*header).is_valid() {
                return Err(ShmError::InvalidMagic);
            }
        }

        // SAFETY: Header was validated by is_valid() above; slot_count is a plain u32 read.
        let max_slots = unsafe { (*header).slot_count() };

        // slot_count comes from the file and is untrusted: a truncated or
        // corrupt file can declare more slots than the mmap actually holds,
        // and slot() bounds its offsets by max_slots. Reject here so no
        // slot access can run past the mmap end.
        if metadata.len() < shm_size(max_slots) as u64 {
            return Err(ShmError::FileTooSmall);
        }

        Ok(Self { mmap, max_slots })
    }

    /// Get a pointer to the header.
    #[inline]
    fn header(&self) -> &ShmHeader {
        // SAFETY: mmap was validated in open() — magic and version checked.
        // Page-aligned mmap base satisfies ShmHeader alignment.
        unsafe { &*(self.mmap.as_ptr() as *const ShmHeader) }
    }

    /// Get a reference to a slot.
    #[inline]
    fn slot(&self, index: u32) -> Option<&PointSlot> {
        if index >= self.max_slots {
            return None;
        }
        let offset = HEADER_SIZE + (index as usize) * SLOT_SIZE;
        // SAFETY: index < max_slots is bounds-checked above, and open() verified the
        // mmap covers shm_size(max_slots), so offset is within the mmap region.
        // PointSlot alignment is satisfied by SLOT_SIZE.
        unsafe { Some(&*(self.mmap.as_ptr().add(offset) as *const PointSlot)) }
    }
}

impl ShmOps for MmapReader {
    fn slot_count(&self) -> u32 {
        self.header().slot_count()
    }

    fn is_slot_valid(&self, index: u32) -> bool {
        self.slot(index).map(|s| s.is_valid()).unwrap_or(false)
    }

    fn read_slot(&self, index: u32) -> Option<(f64, u64, u8)> {
        self.slot(index)?.try_read()
    }

    fn read_slot_spin(&self, index: u32) -> (f64, u64, u8) {
        self.slot(index)
            .map(|s| s.read_spin())
            .unwrap_or((0.0, 0, 0))
    }

    fn write_slot(&mut self, _index: u32, _value: f64, _timestamp: u64, _quality: u8) {
        // Reader cannot write - no-op
    }

    fn last_update(&self) -> u64 {
        self.header().last_update()
    }
}

impl ShmOpsExt for MmapReader {
    fn slot_point_id(&self, index: u32) -> Option<u32> {
        self.slot(index).map(|s| s.point_id)
    }

    fn slot_instance_id(&self, index: u32) -> Option<u32> {
        self.slot(index).map(|s| s.instance_id)
    }

    fn slot_point_type(&self, index: u32) -> Option<u8> {
        self.slot(index).map(|s| s.point_type)
    }

    fn set_slot_metadata(
        &mut self,
        _index: u32,
        _point_id: u32,
        _instance_id: u32,
        _point_type: u8,
    ) {
        // Reader cannot write - no-op
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_mmap_writer_create() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.shm");

        let config = ShmConfig {
            path: path.clone(),
            max_slots: 100,
        };

        let writer = MmapWriter::create(&config).unwrap();
        assert_eq!(writer.slot_count(), 100);

        // File should exist
        assert!(path.exists());
    }

    #[test]
    fn test_mmap_writer_read_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.shm");

        let config = ShmConfig {
            path,
            max_slots: 100,
        };

        let mut writer = MmapWriter::create(&config).unwrap();

        // Write a value
        writer.write_slot(0, 42.5, 1234567890, 0);

        // Read it back
        let result = writer.read_slot(0);
        assert!(result.is_some());

        let (value, ts, quality) = result.unwrap();
        assert_eq!(value, 42.5);
        assert_eq!(ts, 1234567890);
        assert_eq!(quality, 0);
    }

    #[test]
    fn test_open_rejects_file_truncated_below_declared_slots() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("truncated.shm");

        let config = ShmConfig {
            path: path.clone(),
            max_slots: 100,
        };

        // Create a valid SHM whose header claims 100 slots, then drop it.
        drop(MmapWriter::create(&config).unwrap());

        // Truncate so the file only holds 10 slots while the header still
        // claims 100. Stays above shm_size(1), so the minimum-size check
        // alone would let this through — the slot-region check must reject.
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(shm_size(10) as u64).unwrap();
        drop(file);

        assert!(matches!(
            MmapWriter::open(&config),
            Err(ShmError::FileTooSmall)
        ));
        assert!(matches!(
            MmapReader::open(&config),
            Err(ShmError::FileTooSmall)
        ));
    }

    #[test]
    fn test_mmap_reader_writer() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.shm");

        let config = ShmConfig {
            path: path.clone(),
            max_slots: 100,
        };

        // Create writer
        let mut writer = MmapWriter::create(&config).unwrap();
        writer.set_slot_metadata(0, 100, 200, 1);
        writer.write_slot(0, 42.5, 1234567890, 0);
        writer.flush().unwrap();

        // Open reader
        let reader_config = ShmConfig {
            path,
            max_slots: 100,
        };
        let reader = MmapReader::open(&reader_config).unwrap();

        // Read values
        assert_eq!(reader.slot_count(), 100);
        assert!(reader.is_slot_valid(0));
        assert_eq!(reader.slot_point_id(0), Some(100));
        assert_eq!(reader.slot_instance_id(0), Some(200));

        let result = reader.read_slot(0);
        assert!(result.is_some());
        let (value, ts, quality) = result.unwrap();
        assert_eq!(value, 42.5);
        assert_eq!(ts, 1234567890);
        assert_eq!(quality, 0);
    }
}
