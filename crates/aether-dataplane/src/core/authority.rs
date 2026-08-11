//! Cross-process linearization gate for canonical SHM replacement.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;

use crate::{DataplaneError, DataplaneResult};

static AUTHORITY_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Derives the stable sidecar used to serialize transactions with canonical
/// SHM replacement.
///
/// The lock cannot live on the SHM inode itself: `rename(2)` replaces that
/// inode. A sidecar whose name survives every generation gives all processes
/// one common lock object before and after the swap.
#[must_use]
pub fn authority_lock_path(canonical_path: &Path) -> PathBuf {
    let mut path = canonical_path.as_os_str().to_os_string();
    path.push(".authority.lock");
    PathBuf::from(path)
}

fn open_lock(canonical_path: &Path) -> DataplaneResult<File> {
    let lock_path = authority_lock_path(canonical_path);
    if let Some(parent) = lock_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| {
            DataplaneError::io(
                format!("create SHM authority-lock directory {parent:?}"),
                source,
            )
        })?;
    }
    match open_existing_lock(&lock_path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => publish_lock(&lock_path),
        Err(source) => Err(DataplaneError::io(
            format!("open SHM authority lock {lock_path:?}"),
            source,
        )),
    }
}

fn open_existing_lock(lock_path: &Path) -> io::Result<File> {
    // Advisory flock does not require a writable descriptor. Opening the
    // stable sidecar read-only lets non-owner runtime services participate in
    // the same shared/exclusive gate without making the lock contents mutable.
    OpenOptions::new().read(true).open(lock_path)
}

fn publish_lock(lock_path: &Path) -> DataplaneResult<File> {
    let staging_path = authority_staging_path(lock_path);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&staging_path)
        .map_err(|source| {
            DataplaneError::io(
                format!("create staged SHM authority lock {staging_path:?}"),
                source,
            )
        })?;
    let mut cleanup = AuthorityStagingCleanup(Some(staging_path.clone()));
    #[cfg(unix)]
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o644))
        .map_err(|source| DataplaneError::io("set SHM authority-lock permissions", source))?;
    file.sync_all()
        .map_err(|source| DataplaneError::io("sync staged SHM authority lock", source))?;

    match std::fs::hard_link(&staging_path, lock_path) {
        Ok(()) => {
            std::fs::remove_file(&staging_path).map_err(|source| {
                DataplaneError::io(
                    format!("remove staged SHM authority name {staging_path:?}"),
                    source,
                )
            })?;
            cleanup.0 = None;
            Ok(file)
        },
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            drop(file);
            drop(cleanup);
            open_existing_lock(lock_path).map_err(|source| {
                DataplaneError::io(
                    format!("open raced SHM authority lock {lock_path:?}"),
                    source,
                )
            })
        },
        Err(source) => Err(DataplaneError::io(
            format!("publish SHM authority lock {lock_path:?}"),
            source,
        )),
    }
}

fn authority_staging_path(lock_path: &Path) -> PathBuf {
    let sequence = AUTHORITY_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut staged: OsString = lock_path.as_os_str().to_owned();
    staged.push(format!(
        ".init.{}.{timestamp}.{sequence}",
        std::process::id()
    ));
    PathBuf::from(staged)
}

struct AuthorityStagingCleanup(Option<PathBuf>);

impl Drop for AuthorityStagingCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Shared transaction lease held while a mapped SHM generation is used.
///
/// Dropping the guard releases the OS advisory lock, including during unwind.
/// The kernel also releases it if the process exits unexpectedly.
#[must_use = "dropping the guard releases the SHM authority lease"]
pub struct AuthorityReadGuard {
    file: File,
}

impl AuthorityReadGuard {
    /// Blocks until no canonical replacement is in progress.
    pub fn acquire(canonical_path: &Path) -> DataplaneResult<Self> {
        let file = open_lock(canonical_path)?;
        FileExt::lock_shared(&file).map_err(|source| {
            DataplaneError::io(
                format!(
                    "acquire shared SHM authority lock {:?}",
                    authority_lock_path(canonical_path)
                ),
                source,
            )
        })?;
        Ok(Self { file })
    }

    /// Attempts to acquire a shared lease without blocking the caller.
    pub fn try_acquire(canonical_path: &Path) -> DataplaneResult<Option<Self>> {
        let file = open_lock(canonical_path)?;
        match FileExt::try_lock_shared(&file) {
            Ok(()) => Ok(Some(Self { file })),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(source) => Err(DataplaneError::io(
                format!(
                    "try shared SHM authority lock {:?}",
                    authority_lock_path(canonical_path)
                ),
                source,
            )),
        }
    }
}

impl Drop for AuthorityReadGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Exclusive lease held across staging, canonical rename, reopen, and local
/// generation publication.
#[must_use = "dropping the guard allows SHM transactions to resume"]
pub struct AuthorityWriteGuard {
    file: File,
    canonical_path: PathBuf,
}

impl AuthorityWriteGuard {
    /// Blocks until every acquisition and command transaction on the
    /// canonical path has completed, then excludes new transactions.
    pub fn acquire(canonical_path: &Path) -> DataplaneResult<Self> {
        let file = open_lock(canonical_path)?;
        FileExt::lock_exclusive(&file).map_err(|source| {
            DataplaneError::io(
                format!(
                    "acquire exclusive SHM authority lock {:?}",
                    authority_lock_path(canonical_path)
                ),
                source,
            )
        })?;
        Ok(Self {
            file,
            canonical_path: canonical_path.to_path_buf(),
        })
    }

    /// Attempts to acquire the exclusive replacement lease without blocking.
    pub fn try_acquire(canonical_path: &Path) -> DataplaneResult<Option<Self>> {
        let file = open_lock(canonical_path)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(Self {
                file,
                canonical_path: canonical_path.to_path_buf(),
            })),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(source) => Err(DataplaneError::io(
                format!(
                    "try exclusive SHM authority lock {:?}",
                    authority_lock_path(canonical_path)
                ),
                source,
            )),
        }
    }

    pub(crate) fn guards(&self, canonical_path: &Path) -> bool {
        self.canonical_path == canonical_path
    }
}

impl Drop for AuthorityWriteGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_transaction_and_exclusive_replacement_are_mutually_exclusive() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let canonical = directory.path().join("authority.shm");

        let read = AuthorityReadGuard::acquire(&canonical).expect("shared transaction lease");
        assert!(
            AuthorityWriteGuard::try_acquire(&canonical)
                .expect("try exclusive replacement lease")
                .is_none(),
            "replacement must not overlap a live transaction"
        );
        drop(read);

        let write = AuthorityWriteGuard::try_acquire(&canonical)
            .expect("try exclusive replacement lease")
            .expect("replacement lease after transaction completes");
        assert!(
            AuthorityReadGuard::try_acquire(&canonical)
                .expect("try shared transaction lease")
                .is_none(),
            "new transactions must not enter during replacement"
        );
        drop(write);

        assert!(
            AuthorityReadGuard::try_acquire(&canonical)
                .expect("try shared lease after replacement")
                .is_some(),
            "transactions must resume after publication"
        );
    }

    #[cfg(unix)]
    #[test]
    fn authority_sidecar_is_atomically_published_for_cross_uid_readers() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let canonical = directory.path().join("shared.shm");
        let first = AuthorityReadGuard::acquire(&canonical).expect("create authority sidecar");
        drop(first);

        let lock_path = authority_lock_path(&canonical);
        assert_eq!(
            std::fs::metadata(&lock_path)
                .expect("stat authority sidecar")
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        assert!(
            std::fs::read_dir(directory.path())
                .expect("list authority directory")
                .all(|entry| !entry
                    .expect("authority directory entry")
                    .file_name()
                    .to_string_lossy()
                    .contains(".authority.lock.init.")),
            "staging names must not survive publication"
        );

        // Existing participants only need read access to the sidecar itself;
        // both shared and exclusive flock modes still coordinate on that fd.
        std::fs::set_permissions(&lock_path, PermissionsExt::from_mode(0o444))
            .expect("make the existing sidecar read-only");
        let write = AuthorityWriteGuard::acquire(&canonical)
            .expect("exclusive lock must not require writable sidecar contents");
        drop(write);
        let _read = AuthorityReadGuard::acquire(&canonical)
            .expect("shared lock must not require writable sidecar contents");
    }
}
