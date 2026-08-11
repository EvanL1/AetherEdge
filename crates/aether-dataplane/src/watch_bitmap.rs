//! mmap-backed subscription bitmap for cross-process event filtering.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::{MmapMut, MmapOptions};

use crate::core::authority::{AuthorityReadGuard, AuthorityWriteGuard};
use crate::{DataplaneError, DataplaneResult};

const WATCH_BITMAP_MAGIC: [u8; 8] = *b"AETHPWBM";
const WATCH_BITMAP_VERSION: u32 = 1;
const WATCH_BITMAP_HEADER_SIZE: usize = 32;

static BITMAP_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(1);

const WATCH_BITMAP_SUFFIX: &str = "-point-watch-subs";

/// Derives an isolated subscription bitmap path for one event consumer.
#[must_use]
pub fn bitmap_path_for_consumer(shm_path: &Path, consumer: &str) -> PathBuf {
    bitmap_path_with_suffix(shm_path, &format!("{WATCH_BITMAP_SUFFIX}-{consumer}"))
}

fn bitmap_path_with_suffix(shm_path: &Path, suffix: &str) -> PathBuf {
    let parent = shm_path.parent().unwrap_or_else(|| Path::new(""));
    let file_name = shm_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let new_name = match file_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => {
            format!("{stem}{suffix}.{extension}")
        },
        _ => format!("{file_name}{suffix}"),
    };
    if parent.as_os_str().is_empty() {
        PathBuf::from(new_name)
    } else {
        parent.join(new_name)
    }
}

/// Shared atomic bitset used by one event consumer to declare watched slots.
pub struct SubscriptionBitmap {
    mmap: MmapMut,
    capacity: usize,
    word_count: usize,
}

impl SubscriptionBitmap {
    /// Opens an existing bitmap or creates it without truncating a live mmap.
    ///
    /// The SHM writer uses this across process restarts so independently
    /// running consumers keep both their mapping and current subscriptions.
    /// Creation and repair are serialized through the bitmap's authority
    /// sidecar, initialized in a staging inode, and atomically published.
    pub fn open_or_create(path: &Path, capacity: usize) -> DataplaneResult<Self> {
        let layout = BitmapLayout::new(capacity)?;
        let _authority = AuthorityWriteGuard::acquire(path)?;
        match Self::open_unlocked(path, capacity) {
            Ok(bitmap) => return Ok(bitmap),
            Err(DataplaneError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {},
            Err(DataplaneError::InvalidLayout(_)) => {},
            Err(error) => return Err(error),
        }

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| {
                DataplaneError::io(format!("create watch bitmap directory {parent:?}"), source)
            })?;
        }
        let staging_path = bitmap_staging_path(path);
        let mut cleanup = BitmapStagingCleanup(Some(staging_path.clone()));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&staging_path)
            .map_err(|source| {
                DataplaneError::io(
                    format!("create staged watch bitmap {staging_path:?}"),
                    source,
                )
            })?;
        let file_size = u64::try_from(layout.file_size).map_err(|_| {
            DataplaneError::InvalidLayout(format!(
                "watch bitmap size {} exceeds the platform file-size range",
                layout.file_size
            ))
        })?;
        file.set_len(file_size)
            .map_err(|source| DataplaneError::io("size watch bitmap", source))?;
        file.write_all(&layout.encode_header())
            .map_err(|source| DataplaneError::io("write watch bitmap header", source))?;
        #[cfg(unix)]
        std::fs::set_permissions(
            &staging_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o666),
        )
        .map_err(|source| DataplaneError::io("set watch bitmap permissions", source))?;
        file.sync_all()
            .map_err(|source| DataplaneError::io("sync staged watch bitmap", source))?;
        std::fs::rename(&staging_path, path).map_err(|source| {
            DataplaneError::io(
                format!("publish staged watch bitmap {staging_path:?} at {path:?}"),
                source,
            )
        })?;
        cleanup.0 = None;
        sync_parent_directory(path)?;
        Self::open_unlocked(path, capacity)
    }

    /// Opens an existing read/write bitmap file with the required capacity.
    pub fn open(path: &Path, expected_capacity: usize) -> DataplaneResult<Self> {
        BitmapLayout::new(expected_capacity)?;
        let _authority = AuthorityReadGuard::acquire(path)?;
        Self::open_unlocked(path, expected_capacity)
    }

    fn open_unlocked(path: &Path, expected_capacity: usize) -> DataplaneResult<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| DataplaneError::io(format!("open watch bitmap {path:?}"), source))?;
        let file_len = usize::try_from(
            file.metadata()
                .map_err(|source| DataplaneError::io("stat watch bitmap", source))?
                .len(),
        )
        .map_err(|_| {
            DataplaneError::InvalidLayout(format!(
                "watch bitmap {path:?} exceeds the platform address-size range"
            ))
        })?;
        if file_len < WATCH_BITMAP_HEADER_SIZE {
            return Err(DataplaneError::InvalidLayout(format!(
                "watch bitmap {path:?} is too short: {file_len} bytes"
            )));
        }
        let mut header = [0_u8; WATCH_BITMAP_HEADER_SIZE];
        file.read_exact(&mut header)
            .map_err(|source| DataplaneError::io("read watch bitmap header", source))?;
        let layout = BitmapLayout::decode_header(&header)?;
        if layout.capacity as usize != expected_capacity {
            return Err(DataplaneError::InvalidLayout(format!(
                "watch bitmap {path:?} capacity {} does not match expected capacity {expected_capacity}",
                layout.capacity
            )));
        }
        if file_len != layout.file_size {
            return Err(DataplaneError::InvalidLayout(format!(
                "watch bitmap {path:?} has size {file_len}, expected {} for capacity {expected_capacity}",
                layout.file_size
            )));
        }

        // SAFETY: the file length was validated above and the OS provides a
        // page-aligned mmap base, satisfying `AtomicU64` alignment.
        let mmap = unsafe { MmapOptions::new().len(layout.file_size).map_mut(&file) }
            .map_err(|source| DataplaneError::io(format!("mmap watch bitmap {path:?}"), source))?;
        Ok(Self {
            mmap,
            capacity: expected_capacity,
            word_count: layout.word_count,
        })
    }

    /// Creates an anonymous bitmap for tests and in-process compositions.
    pub fn new_in_memory(capacity: usize) -> DataplaneResult<Self> {
        let layout = BitmapLayout::new(capacity)?;
        let mut mmap = MmapOptions::new()
            .len(layout.file_size)
            .map_anon()
            .map_err(|source| DataplaneError::io("create anonymous watch bitmap", source))?;
        mmap[..WATCH_BITMAP_HEADER_SIZE].copy_from_slice(&layout.encode_header());
        Ok(Self {
            mmap,
            capacity,
            word_count: layout.word_count,
        })
    }

    /// Returns the exact physical-slot capacity declared by this bitmap.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns whether one physical slot is subscribed.
    #[inline]
    #[must_use]
    pub fn is_watched(&self, slot: usize) -> bool {
        if slot >= self.capacity {
            return false;
        }
        let word_index = slot / u64::BITS as usize;
        let bit_index = slot % u64::BITS as usize;
        let Some(word) = self.words().get(word_index) else {
            return false;
        };
        word.load(Ordering::Relaxed) & (1_u64 << bit_index) != 0
    }

    /// Subscribes one physical slot.
    #[inline]
    pub fn set_watched(&self, slot: usize) -> DataplaneResult<()> {
        if slot >= self.capacity {
            return Err(self.out_of_bounds(slot));
        }
        let word_index = slot / u64::BITS as usize;
        let bit_index = slot % u64::BITS as usize;
        let word = self
            .words()
            .get(word_index)
            .ok_or_else(|| self.out_of_bounds(slot))?;
        word.fetch_or(1_u64 << bit_index, Ordering::Release);
        Ok(())
    }

    /// Unsubscribes one physical slot.
    #[inline]
    pub fn clear_watched(&self, slot: usize) -> DataplaneResult<()> {
        if slot >= self.capacity {
            return Err(self.out_of_bounds(slot));
        }
        let word_index = slot / u64::BITS as usize;
        let bit_index = slot % u64::BITS as usize;
        let word = self
            .words()
            .get(word_index)
            .ok_or_else(|| self.out_of_bounds(slot))?;
        word.fetch_and(!(1_u64 << bit_index), Ordering::Release);
        Ok(())
    }

    /// Clears every subscription for this consumer.
    pub fn clear_all(&self) {
        for word in self.words() {
            word.store(0, Ordering::Release);
        }
    }

    /// Counts watched slots for diagnostics.
    #[must_use]
    pub fn subscription_count(&self) -> usize {
        self.words()
            .iter()
            .map(|word| word.load(Ordering::Relaxed).count_ones() as usize)
            .sum()
    }

    fn words(&self) -> &[AtomicU64] {
        // SAFETY: every constructor validates the versioned header and exact
        // `header + word_count * size_of::<AtomicU64>()` mapping. The 32-byte
        // header keeps the word array aligned, mmap bases are page-aligned,
        // and the mapping outlives the returned slice borrowed from `self`.
        unsafe {
            std::slice::from_raw_parts(
                self.mmap.as_ptr().add(WATCH_BITMAP_HEADER_SIZE) as *const AtomicU64,
                self.word_count,
            )
        }
    }

    fn out_of_bounds(&self, slot: usize) -> DataplaneError {
        DataplaneError::InvalidLayout(format!(
            "watch bitmap slot {slot} exceeds capacity {}",
            self.capacity
        ))
    }
}

struct BitmapLayout {
    capacity: u32,
    word_count: usize,
    file_size: usize,
}

impl BitmapLayout {
    fn new(capacity: usize) -> DataplaneResult<Self> {
        if capacity == 0 {
            return Err(DataplaneError::InvalidLayout(
                "watch bitmap capacity must be greater than zero".to_string(),
            ));
        }
        let capacity = u32::try_from(capacity).map_err(|_| {
            DataplaneError::InvalidLayout(format!(
                "watch bitmap capacity {capacity} exceeds u32::MAX"
            ))
        })?;
        let word_count = (capacity as usize).div_ceil(u64::BITS as usize);
        let words_size = word_count
            .checked_mul(std::mem::size_of::<AtomicU64>())
            .ok_or_else(|| {
                DataplaneError::InvalidLayout(
                    "watch bitmap word-array size overflows usize".to_string(),
                )
            })?;
        let file_size = WATCH_BITMAP_HEADER_SIZE
            .checked_add(words_size)
            .ok_or_else(|| {
                DataplaneError::InvalidLayout("watch bitmap size overflows usize".to_string())
            })?;
        Ok(Self {
            capacity,
            word_count,
            file_size,
        })
    }

    fn encode_header(&self) -> [u8; WATCH_BITMAP_HEADER_SIZE] {
        let mut bytes = [0_u8; WATCH_BITMAP_HEADER_SIZE];
        bytes[0..8].copy_from_slice(&WATCH_BITMAP_MAGIC);
        bytes[8..12].copy_from_slice(&WATCH_BITMAP_VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.capacity.to_le_bytes());
        bytes[16..20].copy_from_slice(&(self.word_count as u32).to_le_bytes());
        bytes
    }

    fn decode_header(bytes: &[u8; WATCH_BITMAP_HEADER_SIZE]) -> DataplaneResult<Self> {
        if bytes[0..8] != WATCH_BITMAP_MAGIC {
            return Err(DataplaneError::InvalidLayout(
                "watch bitmap has invalid or obsolete magic".to_string(),
            ));
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().map_err(|_| {
            DataplaneError::InvalidLayout("watch bitmap version is malformed".to_string())
        })?);
        if version != WATCH_BITMAP_VERSION {
            return Err(DataplaneError::InvalidLayout(format!(
                "watch bitmap version {version} is unsupported; expected {WATCH_BITMAP_VERSION}"
            )));
        }
        if bytes[20..].iter().any(|byte| *byte != 0) {
            return Err(DataplaneError::InvalidLayout(
                "watch bitmap reserved header bytes are non-zero".to_string(),
            ));
        }
        let capacity = u32::from_le_bytes(bytes[12..16].try_into().map_err(|_| {
            DataplaneError::InvalidLayout("watch bitmap capacity is malformed".to_string())
        })?);
        let word_count = u32::from_le_bytes(bytes[16..20].try_into().map_err(|_| {
            DataplaneError::InvalidLayout("watch bitmap word count is malformed".to_string())
        })?) as usize;
        let layout = Self::new(capacity as usize)?;
        if layout.word_count != word_count {
            return Err(DataplaneError::InvalidLayout(format!(
                "watch bitmap word count {word_count} does not match capacity {capacity}"
            )));
        }
        Ok(layout)
    }
}

fn bitmap_staging_path(path: &Path) -> PathBuf {
    let sequence = BITMAP_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut staged: OsString = path.as_os_str().to_owned();
    staged.push(format!(
        ".init.{}.{timestamp}.{sequence}",
        std::process::id()
    ));
    PathBuf::from(staged)
}

fn sync_parent_directory(path: &Path) -> DataplaneResult<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| {
            DataplaneError::io(format!("sync watch bitmap directory {parent:?}"), source)
        })
}

struct BitmapStagingCleanup(Option<PathBuf>);

impl Drop for BitmapStagingCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}
