//! mmap-backed subscription bitmap for cross-process event filtering.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::{MmapMut, MmapOptions};

use crate::{DataplaneError, DataplaneResult};

/// Number of 64-bit words in the compatibility watch bitmap.
pub const WATCH_WORDS_COUNT: usize = 1_563;

/// Watch bitmap file size in bytes.
pub const WATCH_BITMAP_SIZE: usize = WATCH_WORDS_COUNT * std::mem::size_of::<AtomicU64>();

/// Common suffix for per-consumer PointWatch subscription bitmaps.
pub const WATCH_BITMAP_SUFFIX: &str = "-point-watch-subs";

/// Derives the aether-automation bitmap path from the main SHM path.
#[must_use]
pub fn automation_bitmap_path_from_shm(shm_path: &Path) -> PathBuf {
    bitmap_path_for_consumer(shm_path, "automation")
}

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
    path: PathBuf,
}

impl SubscriptionBitmap {
    /// Creates a fresh, zero-filled bitmap file.
    pub fn create(path: &Path) -> DataplaneResult<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| {
                DataplaneError::io(format!("create watch bitmap directory {parent:?}"), source)
            })?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|source| {
                DataplaneError::io(format!("create watch bitmap {path:?}"), source)
            })?;
        file.set_len(WATCH_BITMAP_SIZE as u64)
            .map_err(|source| DataplaneError::io("size watch bitmap", source))?;
        #[cfg(unix)]
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o666))
            .map_err(|source| DataplaneError::io("set watch bitmap permissions", source))?;

        // SAFETY: the writable file was just sized to the exact mapping length
        // and remains alive while the mapping is created. It is exclusively
        // owned and zero-filled at this initialization point.
        let mmap = unsafe { MmapOptions::new().len(WATCH_BITMAP_SIZE).map_mut(&file) }
            .map_err(|source| DataplaneError::io(format!("mmap watch bitmap {path:?}"), source))?;
        Ok(Self {
            mmap,
            path: path.to_owned(),
        })
    }

    /// Opens an existing bitmap or creates it without truncating a live mmap.
    ///
    /// The SHM writer uses this across process restarts so independently
    /// running consumers keep both their mapping and current subscriptions.
    /// A `create_new` race is resolved by opening the winner's file.
    pub fn open_or_create(path: &Path) -> DataplaneResult<Self> {
        match Self::open(path) {
            Ok(bitmap) => {
                #[cfg(unix)]
                std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o666))
                    .map_err(|source| {
                        DataplaneError::io("set existing watch bitmap permissions", source)
                    })?;
                return Ok(bitmap);
            },
            Err(DataplaneError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| {
                DataplaneError::io(format!("create watch bitmap directory {parent:?}"), source)
            })?;
        }
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                return Self::open(path);
            },
            Err(source) => {
                return Err(DataplaneError::io(
                    format!("create watch bitmap {path:?}"),
                    source,
                ));
            },
        };
        file.set_len(WATCH_BITMAP_SIZE as u64)
            .map_err(|source| DataplaneError::io("size watch bitmap", source))?;
        #[cfg(unix)]
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o666))
            .map_err(|source| DataplaneError::io("set watch bitmap permissions", source))?;

        // SAFETY: this process won create_new, sized the new file exactly,
        // and has not exposed a mapping before this initialization.
        let mmap = unsafe { MmapOptions::new().len(WATCH_BITMAP_SIZE).map_mut(&file) }
            .map_err(|source| DataplaneError::io(format!("mmap watch bitmap {path:?}"), source))?;
        Ok(Self {
            mmap,
            path: path.to_owned(),
        })
    }

    /// Opens an existing read/write bitmap file.
    pub fn open(path: &Path) -> DataplaneResult<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| DataplaneError::io(format!("open watch bitmap {path:?}"), source))?;
        let file_len = file
            .metadata()
            .map_err(|source| DataplaneError::io("stat watch bitmap", source))?
            .len() as usize;
        if file_len != WATCH_BITMAP_SIZE {
            return Err(DataplaneError::InvalidLayout(format!(
                "watch bitmap {path:?} has size {file_len}, expected {WATCH_BITMAP_SIZE}"
            )));
        }

        // SAFETY: the file length was validated above and the OS provides a
        // page-aligned mmap base, satisfying `AtomicU64` alignment.
        let mmap = unsafe { MmapOptions::new().len(WATCH_BITMAP_SIZE).map_mut(&file) }
            .map_err(|source| DataplaneError::io(format!("mmap watch bitmap {path:?}"), source))?;
        Ok(Self {
            mmap,
            path: path.to_owned(),
        })
    }

    /// Creates an anonymous bitmap for tests and in-process compositions.
    pub fn new_in_memory() -> DataplaneResult<Self> {
        let mmap = MmapOptions::new()
            .len(WATCH_BITMAP_SIZE)
            .map_anon()
            .map_err(|source| DataplaneError::io("create anonymous watch bitmap", source))?;
        Ok(Self {
            mmap,
            path: PathBuf::from("<memory>"),
        })
    }

    /// Returns whether one physical slot is subscribed.
    #[inline]
    #[must_use]
    pub fn is_watched(&self, slot: usize) -> bool {
        let word_index = slot / u64::BITS as usize;
        let bit_index = slot % u64::BITS as usize;
        let Some(word) = self.words().get(word_index) else {
            return false;
        };
        word.load(Ordering::Relaxed) & (1_u64 << bit_index) != 0
    }

    /// Subscribes one physical slot.
    #[inline]
    pub fn set_watched(&self, slot: usize) {
        let word_index = slot / u64::BITS as usize;
        let bit_index = slot % u64::BITS as usize;
        if let Some(word) = self.words().get(word_index) {
            word.fetch_or(1_u64 << bit_index, Ordering::Release);
        }
    }

    /// Unsubscribes one physical slot.
    #[inline]
    pub fn clear_watched(&self, slot: usize) {
        let word_index = slot / u64::BITS as usize;
        let bit_index = slot % u64::BITS as usize;
        if let Some(word) = self.words().get(word_index) {
            word.fetch_and(!(1_u64 << bit_index), Ordering::Release);
        }
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

    /// Returns the backing file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Compatibility accessor; bitmap mappings are intentionally read/write.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        false
    }

    fn words(&self) -> &[AtomicU64] {
        // SAFETY: every constructor guarantees an exact
        // `WATCH_WORDS_COUNT * size_of::<AtomicU64>()` mapping. mmap bases are
        // page-aligned and therefore correctly aligned for `AtomicU64`; the
        // mapping outlives the returned slice borrowed from `self`.
        unsafe {
            std::slice::from_raw_parts(self.mmap.as_ptr() as *const AtomicU64, WATCH_WORDS_COUNT)
        }
    }
}
