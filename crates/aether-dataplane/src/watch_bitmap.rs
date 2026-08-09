//! mmap-backed subscription bitmap for cross-process event filtering.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::{MmapMut, MmapOptions};

use crate::core::authority::{AuthorityReadGuard, AuthorityWriteGuard};
use crate::{DataplaneError, DataplaneResult};

/// Number of 64-bit words in one point-watch subscription bitmap.
const WATCH_WORDS_COUNT: usize = 1_563;

/// Watch bitmap file size in bytes.
const WATCH_BITMAP_SIZE: usize = WATCH_WORDS_COUNT * std::mem::size_of::<AtomicU64>();

static BITMAP_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Maximum number of physical slots addressable by a subscription bitmap.
pub const WATCH_SLOT_CAPACITY: usize = WATCH_WORDS_COUNT * u64::BITS as usize;

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
}

impl SubscriptionBitmap {
    /// Opens an existing bitmap or creates it without truncating a live mmap.
    ///
    /// The SHM writer uses this across process restarts so independently
    /// running consumers keep both their mapping and current subscriptions.
    /// Creation and repair are serialized through the bitmap's authority
    /// sidecar, initialized in a staging inode, and atomically published.
    pub fn open_or_create(path: &Path) -> DataplaneResult<Self> {
        let _authority = AuthorityWriteGuard::acquire(path)?;
        match Self::open_unlocked(path) {
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
        let file = OpenOptions::new()
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
        file.set_len(WATCH_BITMAP_SIZE as u64)
            .map_err(|source| DataplaneError::io("size watch bitmap", source))?;
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
        Self::open_unlocked(path)
    }

    /// Opens an existing read/write bitmap file.
    pub fn open(path: &Path) -> DataplaneResult<Self> {
        let _authority = AuthorityReadGuard::acquire(path)?;
        Self::open_unlocked(path)
    }

    fn open_unlocked(path: &Path) -> DataplaneResult<Self> {
        let file = OpenOptions::new()
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
        Ok(Self { mmap })
    }

    /// Creates an anonymous bitmap for tests and in-process compositions.
    pub fn new_in_memory() -> DataplaneResult<Self> {
        let mmap = MmapOptions::new()
            .len(WATCH_BITMAP_SIZE)
            .map_anon()
            .map_err(|source| DataplaneError::io("create anonymous watch bitmap", source))?;
        Ok(Self { mmap })
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
    pub fn set_watched(&self, slot: usize) -> DataplaneResult<()> {
        let word_index = slot / u64::BITS as usize;
        let bit_index = slot % u64::BITS as usize;
        let word = self.words().get(word_index).ok_or_else(|| {
            DataplaneError::InvalidLayout(format!(
                "watch bitmap slot {slot} exceeds capacity {WATCH_SLOT_CAPACITY}"
            ))
        })?;
        word.fetch_or(1_u64 << bit_index, Ordering::Release);
        Ok(())
    }

    /// Unsubscribes one physical slot.
    #[inline]
    pub fn clear_watched(&self, slot: usize) -> DataplaneResult<()> {
        let word_index = slot / u64::BITS as usize;
        let bit_index = slot % u64::BITS as usize;
        let word = self.words().get(word_index).ok_or_else(|| {
            DataplaneError::InvalidLayout(format!(
                "watch bitmap slot {slot} exceeds capacity {WATCH_SLOT_CAPACITY}"
            ))
        })?;
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
        // SAFETY: every constructor guarantees an exact
        // `WATCH_WORDS_COUNT * size_of::<AtomicU64>()` mapping. mmap bases are
        // page-aligned and therefore correctly aligned for `AtomicU64`; the
        // mapping outlives the returned slice borrowed from `self`.
        unsafe {
            std::slice::from_raw_parts(self.mmap.as_ptr() as *const AtomicU64, WATCH_WORDS_COUNT)
        }
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
