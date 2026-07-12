//! Cross-process linearization gate for canonical SHM replacement.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::{DataplaneError, DataplaneResult};

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
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| {
            DataplaneError::io(format!("open SHM authority lock {lock_path:?}"), source)
        })
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
}
