//! ShmHandle — runtime-swappable shared memory layout
//!
//! Wraps `UnifiedWriter`, `ChannelToSlotIndex`, and `ReverseSlotIndex` behind a
//! single `ArcSwapOption` layout so that a routing reload can atomically replace
//! all hot-path lookup structures without mixing versions.
//!
//! # Usage
//!
//! ```text
//! Hot path (every poll cycle):
//!   handle.layout()   → Option<Guard<Arc<ShmLayout>>>   (wait-free)
//!
//! Cold path (routing reload):
//!   handle.rebuild(&channel_points) → Result<()>
//!     1. Creates new UnifiedWriter from SharedConfig + new channel point counts
//!     2. Builds new forward and reverse indexes from new writer
//!     3. ArcSwap::store() — old layout drops when last Guard releases
//! ```

use std::sync::Arc;

use crate::channel_points::ChannelPointCounts;
use crate::core::config::{commit_generation_swap, generation_file_path};
use crate::reverse_index::ReverseSlotIndex;
use crate::shared_config::{ChannelToSlotIndex, SharedConfig};
use crate::unified_shm::UnifiedWriter;
use arc_swap::{ArcSwapOption, Guard};
use tracing::info;

#[cfg(unix)]
use crate::point_watch::PointWatchSignaler;

/// Coherent SHM layout snapshot used by hot paths.
///
/// All fields are built from the same `UnifiedWriter` layout and are swapped as
/// one `Arc`, preventing writer/index/reverse_index version skew during rebuild.
pub struct ShmLayout {
    pub writer: Arc<UnifiedWriter>,
    pub index: Arc<ChannelToSlotIndex>,
    pub reverse_index: Arc<ReverseSlotIndex>,
}

impl ShmLayout {
    fn new(writer: UnifiedWriter, index: ChannelToSlotIndex) -> Self {
        let slot_count = writer.slot_count();
        let reverse_index = ReverseSlotIndex::from_forward(&index, slot_count);

        Self {
            writer: Arc::new(writer),
            index: Arc::new(index),
            reverse_index: Arc::new(reverse_index),
        }
    }
}

/// Shared handle for runtime-swappable SHM layout.
///
/// All reads are wait-free (`ArcSwapOption::load`). Writes (rebuild) are
/// extremely infrequent (only on routing reload) and non-blocking to readers.
pub struct ShmHandle {
    layout: ArcSwapOption<ShmLayout>,
    config: SharedConfig,
    /// PointWatch signaler stored so `rebuild_via_swap` can re-attach it to
    /// each new `UnifiedWriter` produced during rebuild. `None` until
    /// `set_point_watcher` is called.
    #[cfg(unix)]
    point_watcher: ArcSwapOption<PointWatchSignaler>,
}

impl ShmHandle {
    /// Create a new ShmHandle with initial writer and index.
    pub fn new(config: SharedConfig, writer: UnifiedWriter, index: ChannelToSlotIndex) -> Self {
        Self {
            layout: ArcSwapOption::new(Some(Arc::new(ShmLayout::new(writer, index)))),
            config,
            #[cfg(unix)]
            point_watcher: ArcSwapOption::empty(),
        }
    }

    /// Create an empty ShmHandle (SHM not available).
    pub fn empty(config: SharedConfig) -> Self {
        Self {
            layout: ArcSwapOption::empty(),
            config,
            #[cfg(unix)]
            point_watcher: ArcSwapOption::empty(),
        }
    }

    /// Load current coherent SHM layout (wait-free, zero-copy).
    #[inline]
    pub fn layout(&self) -> Option<Guard<Option<Arc<ShmLayout>>>> {
        let guard = self.layout.load();
        if guard.is_some() { Some(guard) } else { None }
    }

    /// Load current layout Arc (clones the Arc, slightly more expensive than guard).
    #[inline]
    pub fn layout_arc(&self) -> Option<Arc<ShmLayout>> {
        self.layout.load_full()
    }

    /// Rebuild SHM writer and indexes from updated channel point counts.
    ///
    /// Step 3 of the SHM decoupling roadmap routes this through
    /// [`rebuild_via_swap`](Self::rebuild_via_swap): a fresh SHM file is
    /// created at a staging path with all slots initialized to the
    /// NaN-sentinel, then a POSIX `rename(2)` atomically replaces the
    /// canonical path. Existing readers in other processes that mmap'd
    /// the old canonical file keep operating on the now-unlinked inode
    /// (kept live by their mmap reference) until they re-open. automation
    /// learns of the swap via its inode-watcher task in
    /// `services/automation/src/main.rs`, which fires the existing
    /// `rebuild_trigger` when it observes a canonical-path inode change.
    ///
    /// Compared to the legacy `reconfigure_existing` (now removed from
    /// this path), no live mmap is ever mutated mid-flight — there is
    /// no window in which a reader can observe torn `slot_count` or
    /// partially-zeroed slots.
    pub fn rebuild(&self, channel_points: &ChannelPointCounts) -> anyhow::Result<()> {
        self.rebuild_via_swap(channel_points)
    }

    /// Rebuild via per-generation file + atomic rename — the Step 3
    /// alternative to `rebuild()`'s in-place `reconfigure_existing` path.
    ///
    /// Flow:
    /// 1. Compute a unique staging path under the same directory using a
    ///    nanosecond-based generation seed. Wall-clock nanoseconds make
    ///    collisions effectively impossible in practice and, since
    ///    `commit_generation_swap` does an unconditional rename, even a
    ///    collision would only overwrite a stale file (never current).
    /// 2. Create a fresh `UnifiedWriter` at the staging path — this is a
    ///    brand-new file with all slots already initialized to the
    ///    unwritten-NaN sentinel; no in-place mutation of any live mmap.
    /// 3. Flush the new mmap to its backing file so the data is durable
    ///    before we publish the file.
    /// 4. `commit_generation_swap(staging → canonical)`: POSIX `rename(2)`
    ///    atomically replaces the canonical path. Any reader still
    ///    holding a mmap of the previous canonical file keeps reading
    ///    its data (kernel-managed inode lifetime).
    /// 5. ArcSwap the local layout to the new writer.
    ///
    /// **Note**: this updates the *local* layout immediately. Other
    /// processes that have already mmap'd the canonical path will not
    /// learn about the new file until they re-open. PR 3 of Step 3 will
    /// add the cross-service prepare/commit protocol that triggers
    /// automation to re-open. Until then, callers using `rebuild_via_swap`
    /// must arrange their own automation notification (e.g. via the existing
    /// `ChannelManager` rebuild_trigger, or by explicit HTTP signal).
    pub fn rebuild_via_swap(&self, channel_points: &ChannelPointCounts) -> anyhow::Result<()> {
        let canonical = self.config.path().to_path_buf();

        let staging_seq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        let staging_path = generation_file_path(&canonical, staging_seq);

        let staging_config = self.config.clone().with_path(staging_path.clone());

        // Step 1-2: build new SHM at staging path. UnifiedWriter::create
        // initializes the header, every slot to NaN sentinel, and flushes
        // before returning.
        let writer = UnifiedWriter::create(&staging_config, channel_points)
            .map_err(|e| anyhow::anyhow!("create staging SHM at {staging_path:?}: {e}"))?;

        // Step 3: ensure data is on disk before publishing.
        writer.flush().map_err(|e| {
            anyhow::anyhow!("flush staging SHM at {staging_path:?} before swap: {e}")
        })?;

        // Step 4: atomic rename. After this point, fresh opens of
        // `canonical` see the new file; existing mmaps of the old
        // canonical keep working on the previous inode.
        commit_generation_swap(&staging_path, &canonical)?;

        // Step 5: rebuild the local layout pointing at the new writer.
        // We need to re-derive the writer from the canonical path so the
        // SlotWriter's stored `path` field matches reality; the
        // `writer` we just created knows itself as `staging_path` which
        // no longer exists.
        let mut writer =
            UnifiedWriter::open_existing(&self.config, channel_points).map_err(|e| {
                anyhow::anyhow!("re-open new SHM at canonical {canonical:?} as writer: {e}")
            })?;

        // Re-attach the PointWatch signaler (if one was registered at startup)
        // so the new writer emits events on the hot path.
        #[cfg(unix)]
        if let Some(pw) = self.point_watcher.load_full() {
            writer.set_point_watcher(Some(pw));
        }

        let slot_count = writer.slot_count();
        let index = ChannelToSlotIndex::from_unified_writer(&writer);
        let index_len = index.len();
        let layout = Arc::new(ShmLayout::new(writer, index));
        let reverse_len = layout.reverse_index.mapped_count();

        self.layout.store(Some(layout));

        info!(
            "ShmHandle: rebuilt via atomic swap (staging={:?}) — {} slots, {} index, {} reverse",
            staging_path, slot_count, index_len, reverse_len
        );
        Ok(())
    }

    /// Store a `PointWatchSignaler` so `rebuild_via_swap` can re-attach it to
    /// each new `UnifiedWriter` produced during rebuild.
    ///
    /// The caller must also call `writer.set_point_watcher(Some(signaler))`
    /// on the initial writer **before** passing that writer to `ShmHandle::new`,
    /// because the writer is already wrapped in `Arc` by the time this method
    /// can be called.
    #[cfg(unix)]
    pub fn store_point_watcher(&self, signaler: Arc<PointWatchSignaler>) {
        self.point_watcher.store(Some(signaler));
    }

    /// Get the SharedConfig.
    pub fn config(&self) -> &SharedConfig {
        &self.config
    }

    /// Check if SHM is currently available.
    pub fn is_available(&self) -> bool {
        self.layout.load().is_some()
    }
}

impl std::fmt::Debug for ShmHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShmHandle")
            .field("available", &self.is_available())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use aether_model::PointType;
    use std::collections::BTreeMap;

    fn counts(channel_id: u32) -> ChannelPointCounts {
        ChannelPointCounts::from_map(BTreeMap::from([(channel_id, [1, 0, 0, 0])]))
    }

    fn test_config(path: std::path::PathBuf) -> SharedConfig {
        SharedConfig::default().with_path(path).with_max_slots(8)
    }

    #[test]
    fn rebuild_via_swap_replaces_canonical_file_and_layout() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("test.shm");
        let config = test_config(canonical.clone());

        let initial_counts = counts(1001);
        let writer = UnifiedWriter::create(&config, &initial_counts).unwrap();
        let initial_inode = std::fs::metadata(&canonical).unwrap().len(); // proxy for "file exists"
        assert!(initial_inode > 0);
        let index = ChannelToSlotIndex::from_unified_writer(&writer);
        let handle = ShmHandle::new(config, writer, index);

        // Sanity: layout starts on channel 1001.
        let layout = handle.layout_arc().unwrap();
        assert_eq!(layout.index.lookup(1001, PointType::Telemetry, 0), Some(0));

        // Swap-rebuild to a new topology with channel 2002.
        let new_counts = counts(2002);
        handle.rebuild_via_swap(&new_counts).unwrap();

        // The canonical file still exists (POSIX rename replaced it).
        assert!(canonical.exists());

        // Local layout now reflects the new topology.
        let layout = handle.layout_arc().unwrap();
        assert!(layout.index.lookup(1001, PointType::Telemetry, 0).is_none());
        assert_eq!(layout.index.lookup(2002, PointType::Telemetry, 0), Some(0));
        let origin = layout.reverse_index.get(0).unwrap();
        assert_eq!(origin.channel_id, 2002);
        assert_eq!(origin.point_type, PointType::Telemetry);

        // No staging files left behind.
        let stragglers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name().to_string_lossy().starts_with("test-")
                    && e.file_name().to_string_lossy().ends_with(".shm")
            })
            .collect();
        assert!(
            stragglers.is_empty(),
            "rebuild_via_swap left staging files behind: {stragglers:?}"
        );
    }

    #[test]
    fn rebuild_replaces_reverse_index_with_layout() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path().join("test.shm"));

        let initial_counts = counts(1001);
        let writer = UnifiedWriter::create(&config, &initial_counts).unwrap();
        let index = ChannelToSlotIndex::from_unified_writer(&writer);
        let handle = ShmHandle::new(config, writer, index);

        let layout = handle.layout_arc().unwrap();
        let origin = layout.reverse_index.get(0).unwrap();
        assert_eq!(origin.channel_id, 1001);
        assert_eq!(origin.point_type, PointType::Telemetry);

        let rebuilt_counts = counts(2002);
        handle.rebuild(&rebuilt_counts).unwrap();

        let layout = handle.layout_arc().unwrap();
        assert!(layout.index.lookup(1001, PointType::Telemetry, 0).is_none());
        assert_eq!(layout.index.lookup(2002, PointType::Telemetry, 0), Some(0));
        let origin = layout.reverse_index.get(0).unwrap();
        assert_eq!(origin.channel_id, 2002);
        assert_eq!(origin.point_type, PointType::Telemetry);
    }
}
