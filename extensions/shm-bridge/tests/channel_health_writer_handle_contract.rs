use std::sync::Arc;
use std::time::Duration;

use aether_shm_bridge::{
    ChannelHealthManifest, ShmChannelHealthReader, ShmChannelHealthWriterHandle, ShmClientConfig,
};

fn reader(path: &std::path::Path, manifest: Arc<ChannelHealthManifest>) -> ShmChannelHealthReader {
    ShmChannelHealthReader::new(
        ShmClientConfig::new(path, manifest.layout_hash())
            .with_identity_check_interval(Duration::ZERO)
            .with_writer_stale_after(Duration::from_secs(60)),
        manifest,
    )
}

#[test]
fn empty_handle_is_introspectable_and_fails_closed() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("channel-health.shm");
    let handle = ShmChannelHealthWriterHandle::empty(&path);

    assert_eq!(handle.path(), path);
    assert!(!handle.is_available());
    assert!(handle.manifest().is_none());
    assert!(handle.generation().is_none());
    assert!(handle.slot_count().is_none());
    assert!(handle.writer_heartbeat().is_none());
    assert!(
        handle
            .set_online(7, true, 1_000)
            .expect_err("unpublished health writer must reject writes")
            .is_retryable()
    );
    assert!(
        handle
            .update_heartbeat(1_001)
            .expect_err("unpublished health writer must reject heartbeat updates")
            .is_retryable()
    );
}

#[test]
fn create_publishes_a_canonical_writer_and_roundtrips_health() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("channel-health.shm");
    let manifest = Arc::new(ChannelHealthManifest::from_channel_ids([3, 9]));
    let handle = ShmChannelHealthWriterHandle::create(&path, Arc::clone(&manifest))
        .expect("publish channel-health writer");
    let now_ms = aether_shm_bridge::timestamp_ms();

    assert!(handle.is_available());
    assert_eq!(handle.path(), path);
    assert_eq!(
        handle.manifest().expect("active manifest").layout_hash(),
        manifest.layout_hash()
    );
    assert_eq!(handle.slot_count(), Some(10));
    assert!(
        handle
            .generation()
            .is_some_and(|generation| generation > 0 && generation & 1 == 0)
    );

    handle
        .set_online(9, true, now_ms)
        .expect("publish channel health");
    let sample = reader(&path, manifest)
        .read_channel(9)
        .expect("read channel health")
        .expect("observed channel health");
    assert!(sample.online());
    assert_eq!(sample.timestamp_ms(), now_ms);

    let error = handle
        .set_online(4, true, now_ms + 1)
        .expect_err("channels outside the manifest must be rejected");
    assert!(!error.is_retryable());
}

#[test]
fn rebuild_migrates_only_intersection_state_and_timestamp() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("channel-health.shm");
    let first_manifest = Arc::new(ChannelHealthManifest::from_channel_ids([3, 9, 11]));
    let handle = ShmChannelHealthWriterHandle::create(&path, Arc::clone(&first_manifest))
        .expect("publish initial channel-health writer");
    let now_ms = aether_shm_bridge::timestamp_ms();
    handle
        .set_online(3, true, now_ms)
        .expect("write retained channel");
    handle
        .set_online(9, false, now_ms + 1)
        .expect("write retained offline channel");
    handle
        .set_online(11, true, now_ms + 2)
        .expect("write removed channel");
    handle
        .update_heartbeat(now_ms + 3)
        .expect("refresh initial heartbeat");
    let first_generation = handle.generation().expect("initial generation");

    let second_manifest = Arc::new(ChannelHealthManifest::from_channel_ids([3, 9, 10]));
    handle
        .rebuild(Arc::clone(&second_manifest))
        .expect("publish replacement health writer");

    assert_ne!(handle.generation(), Some(first_generation));
    assert_eq!(handle.writer_heartbeat(), Some(now_ms + 3));
    assert_eq!(
        handle
            .manifest()
            .expect("replacement manifest")
            .channel_ids()
            .collect::<Vec<_>>(),
        vec![3, 9, 10]
    );

    let replacement_reader = reader(&path, second_manifest);
    let retained = replacement_reader
        .read_channel(3)
        .expect("read retained channel")
        .expect("retained state");
    assert!(retained.online());
    assert_eq!(retained.timestamp_ms(), now_ms);
    let retained_offline = replacement_reader
        .read_channel(9)
        .expect("read retained offline channel")
        .expect("retained offline state");
    assert!(!retained_offline.online());
    assert_eq!(retained_offline.timestamp_ms(), now_ms + 1);
    assert_eq!(
        replacement_reader
            .read_channel(10)
            .expect("read newly added channel"),
        None,
        "new channels must start unknown"
    );
    assert_eq!(
        replacement_reader
            .read_channel(11)
            .expect("removed channel is outside the manifest"),
        None
    );
    assert!(
        handle
            .set_online(11, true, now_ms + 4)
            .expect_err("removed channel must no longer be writable")
            .to_string()
            .contains("absent")
    );
}

#[test]
fn topology_change_immediately_fences_a_retained_health_reader_without_inode_polling() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("channel-health.shm");
    let first_manifest = Arc::new(ChannelHealthManifest::from_channel_ids([3]));
    let handle = ShmChannelHealthWriterHandle::create(&path, Arc::clone(&first_manifest))
        .expect("publish initial health generation");
    handle
        .set_online(3, true, aether_shm_bridge::timestamp_ms())
        .expect("publish initial health state");
    let retained_reader = ShmChannelHealthReader::new(
        ShmClientConfig::new(&path, first_manifest.layout_hash())
            .with_identity_check_interval(Duration::from_secs(60))
            .with_writer_stale_after(Duration::from_secs(60)),
        first_manifest,
    );
    assert!(
        retained_reader
            .read_channel(3)
            .expect("read initial health generation")
            .expect("initial health sample")
            .online()
    );

    handle
        .rebuild(Arc::new(ChannelHealthManifest::from_channel_ids([9])))
        .expect("publish replacement health topology");

    let error = retained_reader
        .read_channel(3)
        .expect_err("retained reader must not return the unlinked channel state");
    assert!(error.to_string().contains("manifest mismatch"), "{error}");
}

#[test]
fn health_writer_restart_immediately_fences_a_retained_reader_without_inode_polling() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("channel-health.shm");
    let manifest = Arc::new(ChannelHealthManifest::from_channel_ids([3]));
    let first = ShmChannelHealthWriterHandle::create(&path, Arc::clone(&manifest))
        .expect("publish initial health generation");
    first
        .set_online(3, true, aether_shm_bridge::timestamp_ms())
        .expect("publish initial health state");
    let retained_reader = ShmChannelHealthReader::new(
        ShmClientConfig::new(&path, manifest.layout_hash())
            .with_identity_check_interval(Duration::from_secs(60))
            .with_writer_stale_after(Duration::from_secs(60)),
        Arc::clone(&manifest),
    );
    assert!(
        retained_reader
            .read_channel(3)
            .expect("read initial health generation")
            .expect("initial health sample")
            .online()
    );
    drop(first);

    let _replacement = ShmChannelHealthWriterHandle::create(&path, manifest)
        .expect("publish health generation after writer restart");

    match retained_reader.read_channel(3) {
        Ok(Some(replacement)) => panic!(
            "the retained reader returned a pre-restart health observation at {}",
            replacement.timestamp_ms()
        ),
        Ok(None) => {},
        Err(error) => assert!(error.is_retryable(), "unexpected reader error: {error}"),
    }
}

#[test]
fn identical_manifest_rebuild_is_a_true_no_op() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("channel-health.shm");
    let manifest = Arc::new(ChannelHealthManifest::from_channel_ids([3, 9]));
    let handle = ShmChannelHealthWriterHandle::create(&path, Arc::clone(&manifest))
        .expect("publish initial channel-health writer");
    let now_ms = aether_shm_bridge::timestamp_ms();
    handle
        .set_online(3, true, now_ms)
        .expect("write retained state");
    handle
        .update_heartbeat(now_ms + 1)
        .expect("write heartbeat");
    let generation = handle.generation();
    #[cfg(unix)]
    let metadata = std::fs::metadata(&path).expect("canonical metadata");

    handle
        .rebuild(Arc::new(ChannelHealthManifest::from_channel_ids([9, 3, 9])))
        .expect("identical manifest rebuild");

    assert_eq!(handle.generation(), generation);
    assert_eq!(handle.writer_heartbeat(), Some(now_ms + 1));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let after = std::fs::metadata(&path).expect("canonical metadata after no-op");
        assert_eq!(after.dev(), metadata.dev());
        assert_eq!(after.ino(), metadata.ino());
    }
    let sample = reader(&path, manifest)
        .read_channel(3)
        .expect("read state after no-op")
        .expect("retained state after no-op");
    assert!(sample.online());
    assert_eq!(sample.timestamp_ms(), now_ms);
}

#[test]
fn rebuild_can_publish_the_first_generation_for_a_delayed_start() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("channel-health.shm");
    let handle = ShmChannelHealthWriterHandle::empty(&path);
    let manifest = Arc::new(ChannelHealthManifest::from_channel_ids([7]));

    handle
        .rebuild(Arc::clone(&manifest))
        .expect("publish delayed first generation");

    assert!(handle.is_available());
    assert_eq!(handle.slot_count(), Some(8));
    let now_ms = aether_shm_bridge::timestamp_ms();
    handle
        .set_online(7, false, now_ms)
        .expect("write after delayed publication");
    assert!(
        !reader(&path, manifest)
            .read_channel(7)
            .expect("read delayed generation")
            .expect("observed delayed state")
            .online()
    );
}

#[test]
fn coordinated_health_publication_rejects_reserved_and_reused_epochs() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let zero_path = directory.path().join("zero-health.shm");
    let manifest = Arc::new(ChannelHealthManifest::from_channel_ids([7]));

    let zero_error =
        ShmChannelHealthWriterHandle::create_at_epoch(&zero_path, Arc::clone(&manifest), 0)
            .expect_err("epoch zero is reserved for diagnostic compatibility writers");
    assert!(!zero_error.is_retryable());

    let path = directory.path().join("health.shm");
    let handle = ShmChannelHealthWriterHandle::create_at_epoch(&path, Arc::clone(&manifest), 600)
        .expect("publish coordinated health generation");
    let reused = handle
        .rebuild_for_publication(manifest, 600)
        .expect_err("a coordinated publication epoch must not be reused");
    assert!(!reused.is_retryable());
}
