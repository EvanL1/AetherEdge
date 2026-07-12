//! Pure-infra constants, path resolution, and time helpers.
//!
//! These items have no dependency on `aether-model` (`PointType`) or any
//! other business concept — they speak only in terms of files, paths, and
//! wall-clock time.

use std::path::{Path, PathBuf};

use crate::core::authority::AuthorityWriteGuard;
use crate::{DataplaneError, DataplaneResult};

/// Magic number for validation: "AETHER_" in ASCII.
pub const SHARED_MAGIC: u64 = 0x564F4C544147455F;

/// Default snapshot interval in seconds (5 minutes).
pub const DEFAULT_SNAPSHOT_INTERVAL_SECS: u64 = 300;

/// Default shared memory file path (Docker tmpfs mount point).
///
/// Kept for backward compatibility — prefer [`default_shm_path`] for
/// intelligent path selection.
pub const DEFAULT_SHM_PATH: &str = "/shm/rtdb/aether-rtdb.shm";

/// Get the default shared memory path with intelligent fallback.
///
/// Priority:
/// 1. `AETHER_SHM_PATH` environment variable (if set)
/// 2. Docker tmpfs mount point `/shm/rtdb/aether-rtdb.shm` (if exists)
/// 3. Linux RAM-backed tmpfs `/dev/shm/aether-rtdb.shm`
/// 4. Fallback to `/tmp/aether-rtdb.shm` (macOS or other platforms)
pub fn default_shm_path() -> PathBuf {
    if let Ok(path) = std::env::var("AETHER_SHM_PATH") {
        return PathBuf::from(path);
    }

    let docker_path = Path::new("/shm/rtdb");
    if docker_path.exists() {
        return PathBuf::from(DEFAULT_SHM_PATH);
    }

    #[cfg(target_os = "linux")]
    {
        let dev_shm = Path::new("/dev/shm");
        if dev_shm.exists() {
            return dev_shm.join("aether-rtdb.shm");
        }
    }

    PathBuf::from("/tmp/aether-rtdb.shm")
}

/// Check whether the parent directory of a SHM file path exists.
///
/// This is the only generic "is the SHM mountpoint available" check that
/// makes sense without knowing anything about the business config. The
/// business-config-aware version lives in `shared_config::is_shm_available`.
pub fn parent_dir_exists(path: &Path) -> bool {
    path.parent().map(|p| p.exists()).unwrap_or(false)
}

/// Get current timestamp in milliseconds since UNIX epoch.
#[inline]
pub fn timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Compute the path for a per-generation SHM file given the canonical
/// "current" path and a generation number.
///
/// Convention: a base path of `/dev/shm/aether-rtdb.shm` with generation
/// 42 yields `/dev/shm/aether-rtdb-42.shm`. The base path itself
/// continues to refer to the "current" generation — atomic swaps land a
/// freshly-created per-generation file at the base path via
/// `rename(2)`, preserving the open file inode for any reader that
/// already mmap'd the previous generation.
///
/// If the base path has no extension, the generation suffix is appended
/// directly (no `.shm`). Multiple-extension paths (`foo.shm.bak`) get
/// the generation injected before the final extension.
pub fn generation_file_path(base: &Path, generation: u64) -> PathBuf {
    let parent = base.parent();
    let file_name = base.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let new_name = match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{generation}.{ext}"),
        _ => format!("{file_name}-{generation}"),
    };
    match parent {
        Some(p) if !p.as_os_str().is_empty() => p.join(new_name),
        _ => PathBuf::from(new_name),
    }
}

/// Best-effort cleanup of orphaned per-generation SHM staging files.
///
/// `rebuild_via_swap` creates a fresh file at `generation_file_path(canonical, N)`,
/// flushes, then `rename(2)`s it over `canonical`. The rename consumes
/// the staging filename atomically, so under steady-state operation no
/// `*-{N}.{ext}` files ever persist. They can only appear if a process
/// crashed between the create and the rename — typically a io
/// SIGKILL during reload.
///
/// This helper is meant to be called at io startup, before any
/// new SHM file is created. It scans `canonical.parent()` for entries
/// whose name matches `{stem}-{digits}.{ext}` (the exact pattern
/// produced by `generation_file_path`) and unlinks them. Files that
/// were rename'd over `canonical` are unreachable through this scan
/// (the staging name no longer exists), so this is safe regardless of
/// concurrent readers in other processes.
///
/// Returns the number of orphan files removed. Individual unlink
/// failures are logged via `tracing::warn` but do not abort the scan,
/// because a single permission error should not block startup.
pub fn cleanup_orphan_generation_files(canonical: &Path) -> DataplaneResult<usize> {
    let parent = canonical
        .parent()
        .ok_or_else(|| DataplaneError::InvalidPath(canonical.to_path_buf()))?;
    if parent.as_os_str().is_empty() {
        return Ok(0);
    }
    let file_name = canonical
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| DataplaneError::InvalidPath(canonical.to_path_buf()))?;

    let (stem, ext_suffix): (String, String) = match file_name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (file_name.to_string(), String::new()),
    };
    let prefix = format!("{stem}-");

    let read_dir = match std::fs::read_dir(parent) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(DataplaneError::io(
                format!("read SHM generation directory {parent:?}"),
                source,
            ));
        },
    };

    let mut removed = 0;
    for entry in read_dir.flatten() {
        let entry_name_os = entry.file_name();
        let Some(name) = entry_name_os.to_str() else {
            continue;
        };
        if !name.starts_with(&prefix) || !name.ends_with(&ext_suffix) {
            continue;
        }
        if name.len() <= prefix.len() + ext_suffix.len() {
            continue;
        }
        let middle = &name[prefix.len()..name.len() - ext_suffix.len()];
        if middle.is_empty() || !middle.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        // Matched pattern {stem}-{digits}{.ext}. Unlink.
        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(_) => {
                removed += 1;
                tracing::info!("removed orphan generation SHM file: {path:?}");
            },
            Err(e) => {
                tracing::warn!("failed to remove orphan generation SHM file {path:?}: {e}");
            },
        }
    }
    Ok(removed)
}

/// Atomically rename a freshly-created per-generation SHM file into the
/// canonical "current" path.
///
/// POSIX `rename(2)` is atomic on the same filesystem: any process opening
/// `canonical_path` after the call sees either the old or the new file,
/// never a partial state. Existing readers that already mmap'd the
/// previous file at `canonical_path` continue to operate on the previous
/// inode (kept alive by the mmap reference), so they never see torn data
/// either. New readers that re-open `canonical_path` pick up the new
/// generation.
///
/// The two paths must be on the same filesystem; otherwise `rename(2)`
/// degrades to copy+unlink and loses atomicity. Both per-generation files
/// and the canonical "current" file should live in the same SHM mount
/// (typically `/dev/shm` or `/shm/rtdb`).
pub fn commit_generation_swap(staging_path: &Path, canonical_path: &Path) -> DataplaneResult<()> {
    let authority = AuthorityWriteGuard::acquire(canonical_path)?;
    commit_generation_swap_locked(staging_path, canonical_path, &authority)
}

/// Commits a generation while the caller retains an exclusive authority lease
/// acquired before it began staging the replacement.
///
/// This is the linearizable publication path. The guard is tied to the exact
/// canonical path so a lease for another segment cannot authorize the rename.
pub fn commit_generation_swap_locked(
    staging_path: &Path,
    canonical_path: &Path,
    authority: &AuthorityWriteGuard,
) -> DataplaneResult<()> {
    if !authority.guards(canonical_path) {
        return Err(DataplaneError::InvalidPath(canonical_path.to_path_buf()));
    }
    std::fs::rename(staging_path, canonical_path).map_err(|source| {
        DataplaneError::io(
            format!("rename SHM generation {staging_path:?} to {canonical_path:?}"),
            source,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_path_appends_before_extension() {
        let p = generation_file_path(Path::new("/dev/shm/aether-rtdb.shm"), 42);
        assert_eq!(p, PathBuf::from("/dev/shm/aether-rtdb-42.shm"));
    }

    #[test]
    fn generation_path_handles_no_extension() {
        let p = generation_file_path(Path::new("/tmp/aether"), 7);
        assert_eq!(p, PathBuf::from("/tmp/aether-7"));
    }

    #[test]
    fn generation_path_handles_multi_extension() {
        // foo.shm.bak → foo.shm-N.bak (suffix injected before final ext)
        let p = generation_file_path(Path::new("/tmp/aether.shm.bak"), 3);
        assert_eq!(p, PathBuf::from("/tmp/aether.shm-3.bak"));
    }

    #[test]
    fn generation_path_handles_relative_path() {
        let p = generation_file_path(Path::new("aether.shm"), 1);
        assert_eq!(p, PathBuf::from("aether-1.shm"));
    }

    #[test]
    fn generation_path_handles_hidden_file() {
        // Leading-dot file: ".aether" has empty stem before the dot — we
        // treat the whole name as the stem and append the generation.
        let p = generation_file_path(Path::new("/tmp/.aether"), 5);
        assert_eq!(p, PathBuf::from("/tmp/.aether-5"));
    }

    #[test]
    fn commit_swap_renames_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("aether-rtdb.shm");
        let staging = generation_file_path(&canonical, 1);

        // Pre-populate canonical to simulate a "current" file already in
        // place — the swap should overwrite it.
        std::fs::write(&canonical, b"OLD").unwrap();
        std::fs::write(&staging, b"NEW").unwrap();

        commit_generation_swap(&staging, &canonical).unwrap();

        assert_eq!(std::fs::read(&canonical).unwrap(), b"NEW");
        assert!(
            !staging.exists(),
            "staging file should be gone after rename"
        );
    }

    #[test]
    fn cleanup_orphan_removes_matching_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("aether-rtdb.shm");

        // Plant a mix:
        //   - canonical itself (must be preserved)
        //   - 3 orphan generation files (must be removed)
        //   - a similar-prefix-but-not-numeric file (must be preserved)
        //   - a different-prefix file (must be preserved)
        std::fs::write(&canonical, b"current").unwrap();
        std::fs::write(dir.path().join("aether-rtdb-1.shm"), b"orphan1").unwrap();
        std::fs::write(dir.path().join("aether-rtdb-42.shm"), b"orphan42").unwrap();
        std::fs::write(dir.path().join("aether-rtdb-999999.shm"), b"orphan_big").unwrap();
        std::fs::write(dir.path().join("aether-rtdb-alpha.shm"), b"non-numeric").unwrap();
        std::fs::write(dir.path().join("aether-other.shm"), b"unrelated").unwrap();

        let removed = cleanup_orphan_generation_files(&canonical).unwrap();
        assert_eq!(removed, 3);

        assert!(canonical.exists(), "canonical must be preserved");
        assert!(
            dir.path().join("aether-rtdb-alpha.shm").exists(),
            "non-numeric suffix must not match"
        );
        assert!(
            dir.path().join("aether-other.shm").exists(),
            "different-prefix file must not match"
        );
        for n in [1u32, 42, 999999] {
            assert!(
                !dir.path().join(format!("aether-rtdb-{n}.shm")).exists(),
                "orphan {n} should be removed"
            );
        }
    }

    #[test]
    fn cleanup_orphan_returns_zero_when_dir_missing() {
        // Canonical path under a non-existent directory — read_dir errors
        // with NotFound, which the helper turns into Ok(0).
        let canonical = std::path::PathBuf::from("/tmp/does-not-exist-step3/aether-rtdb.shm");
        assert_eq!(cleanup_orphan_generation_files(&canonical).unwrap(), 0);
    }

    #[test]
    fn commit_swap_works_when_canonical_missing() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("aether-rtdb.shm");
        let staging = generation_file_path(&canonical, 1);

        std::fs::write(&staging, b"NEW").unwrap();
        // canonical does NOT exist — first-create case.
        commit_generation_swap(&staging, &canonical).unwrap();

        assert_eq!(std::fs::read(&canonical).unwrap(), b"NEW");
    }
}
