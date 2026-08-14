use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;

use aether_dataplane::{DataplaneError, SubscriptionBitmap, bitmap_path_for_consumer};

const TEST_CAPACITY: usize = 100_000;

#[test]
fn bitmap_create_open_and_atomic_updates_roundtrip() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("alarm-subs.shm");
    {
        let bitmap =
            SubscriptionBitmap::open_or_create(&path, TEST_CAPACITY).expect("create bitmap");
        bitmap.set_watched(42).expect("watch slot 42");
        bitmap.set_watched(99_999).expect("watch slot 99,999");
        assert_eq!(bitmap.subscription_count(), 2);
    }

    let reopened = SubscriptionBitmap::open(&path, TEST_CAPACITY).expect("open bitmap");
    assert_eq!(reopened.capacity(), TEST_CAPACITY);
    assert!(reopened.is_watched(42));
    assert!(reopened.is_watched(99_999));
    reopened.clear_watched(42).expect("clear slot 42");
    assert!(!reopened.is_watched(42));
}

#[test]
fn concurrent_open_or_create_publishes_one_shared_bitmap() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = Arc::new(directory.path().join("concurrent.bitmap"));
    let start = Arc::new(Barrier::new(3));
    let capacity = 257;
    let slots = [17, capacity - 1];
    let workers: Vec<_> = slots
        .into_iter()
        .map(|slot| {
            let path = Arc::clone(&path);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                let bitmap = SubscriptionBitmap::open_or_create(path.as_path(), capacity)
                    .expect("concurrent open_or_create");
                bitmap.set_watched(slot).expect("watch concurrent slot");
            })
        })
        .collect();

    start.wait();
    for worker in workers {
        worker.join().expect("open_or_create worker");
    }

    let reopened = SubscriptionBitmap::open(path.as_path(), capacity).expect("open shared bitmap");
    assert!(reopened.is_watched(slots[0]));
    assert!(reopened.is_watched(slots[1]));
    assert_eq!(reopened.subscription_count(), 2);
}

#[test]
fn zero_byte_canonical_orphan_is_replaced_atomically() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("orphan.bitmap");
    let orphan = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("create zero-byte orphan");
    assert_eq!(orphan.metadata().expect("stat orphan").len(), 0);

    let capacity: usize = 129;
    let repaired =
        SubscriptionBitmap::open_or_create(&path, capacity).expect("repair orphan atomically");
    repaired.set_watched(7).expect("watch repaired slot");

    assert_eq!(
        orphan.metadata().expect("stat retained orphan inode").len(),
        0,
        "repair must publish a replacement inode instead of resizing the canonical orphan in place"
    );
    assert_eq!(
        std::fs::metadata(&path)
            .expect("stat repaired canonical")
            .len(),
        (32 + capacity.div_ceil(u64::BITS as usize) * std::mem::size_of::<u64>()) as u64
    );
    assert!(repaired.is_watched(7));
    assert!(
        SubscriptionBitmap::open(&path, capacity)
            .expect("open repaired canonical")
            .is_watched(7)
    );
}

#[test]
fn open_or_create_does_not_truncate_an_existing_live_bitmap() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("subscriptions.bitmap");
    let capacity = 128;
    let consumer = SubscriptionBitmap::open_or_create(&path, capacity).expect("create bitmap");
    consumer.set_watched(73).expect("watch retained slot");
    let original_len = std::fs::metadata(&path).expect("stat live bitmap").len();

    let writer_restart =
        SubscriptionBitmap::open_or_create(&path, capacity).expect("reuse existing bitmap");
    writer_restart
        .set_watched(91)
        .expect("watch through reopened mapping");

    assert!(consumer.is_watched(73));
    assert!(consumer.is_watched(91));
    assert!(writer_restart.is_watched(73));
    assert_eq!(writer_restart.subscription_count(), 2);
    assert_eq!(
        std::fs::metadata(&path)
            .expect("stat reused live bitmap")
            .len(),
        original_len
    );
}

#[test]
fn capacity_last_slot_succeeds_and_capacity_is_an_explicit_error() {
    let capacity = 65;
    let bitmap = SubscriptionBitmap::new_in_memory(capacity).expect("anonymous bitmap");
    let last_slot = capacity - 1;

    bitmap
        .set_watched(last_slot)
        .expect("last addressable slot");
    assert!(bitmap.is_watched(last_slot));
    bitmap
        .clear_watched(last_slot)
        .expect("clear last addressable slot");
    assert!(!bitmap.is_watched(last_slot));

    for error in [
        bitmap
            .set_watched(capacity)
            .expect_err("capacity must be rejected by set"),
        bitmap
            .clear_watched(capacity)
            .expect_err("capacity must be rejected by clear"),
    ] {
        assert!(matches!(error, DataplaneError::InvalidLayout(_)));
        assert!(error.to_string().contains("exceeds capacity"));
    }
    assert!(!bitmap.is_watched(capacity));
}

#[test]
fn zero_capacity_is_rejected() {
    let error = match SubscriptionBitmap::new_in_memory(0) {
        Ok(_) => panic!("zero capacity must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, DataplaneError::InvalidLayout(_)));
    assert!(error.to_string().contains("greater than zero"));
}

#[test]
fn expected_capacity_is_part_of_the_physical_contract() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("capacity.bitmap");
    let original = SubscriptionBitmap::open_or_create(&path, 64).expect("create bitmap");
    original.set_watched(63).expect("watch old last slot");
    assert_eq!(std::fs::metadata(&path).expect("bitmap metadata").len(), 40);

    let error = match SubscriptionBitmap::open(&path, 65) {
        Ok(_) => panic!("capacity mismatch must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, DataplaneError::InvalidLayout(_)));
    assert!(error.to_string().contains("capacity"));

    let replacement =
        SubscriptionBitmap::open_or_create(&path, 65).expect("publish replacement capacity");
    assert_eq!(replacement.capacity(), 65);
    assert_eq!(replacement.subscription_count(), 0);
    assert_eq!(std::fs::metadata(&path).expect("bitmap metadata").len(), 48);
    assert_eq!(original.capacity(), 64);
    assert!(original.is_watched(63));
}

#[test]
fn obsolete_headerless_bitmap_is_replaced_without_decoding_its_bits() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("obsolete.bitmap");
    std::fs::write(&path, 1_u64.to_ne_bytes()).expect("write obsolete headerless bitmap");

    let error = match SubscriptionBitmap::open(&path, 64) {
        Ok(_) => panic!("obsolete bitmap must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, DataplaneError::InvalidLayout(_)));

    let replacement =
        SubscriptionBitmap::open_or_create(&path, 64).expect("publish current bitmap");
    assert_eq!(replacement.capacity(), 64);
    assert_eq!(replacement.subscription_count(), 0);
    assert!(!replacement.is_watched(0));
}

#[test]
fn nonzero_reserved_header_bytes_fail_closed_and_are_repaired_atomically() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("noncanonical.bitmap");
    drop(SubscriptionBitmap::open_or_create(&path, 64).expect("create bitmap"));

    let mut bytes = std::fs::read(&path).expect("read bitmap");
    bytes[8] = 1;
    std::fs::write(&path, bytes).expect("write non-zero reserved byte");

    let error = match SubscriptionBitmap::open(&path, 64) {
        Ok(_) => panic!("non-zero reserved bytes must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, DataplaneError::InvalidLayout(_)));
    assert!(error.to_string().contains("reserved"));

    drop(SubscriptionBitmap::open_or_create(&path, 64).expect("repair canonical bitmap"));
    let repaired = std::fs::read(&path).expect("read repaired bitmap");
    assert_eq!(&repaired[8..12], &[0; 4]);
    assert!(SubscriptionBitmap::open(&path, 64).is_ok());
}

#[test]
fn each_event_consumer_gets_an_independent_bitmap_path() {
    let main = Path::new("/dev/shm/aether-live-state.shm");

    assert_eq!(
        bitmap_path_for_consumer(main, "automation"),
        PathBuf::from("/dev/shm/aether-live-state-point-watch-subs-automation.shm")
    );
    assert_eq!(
        bitmap_path_for_consumer(main, "alarm"),
        PathBuf::from("/dev/shm/aether-live-state-point-watch-subs-alarm.shm")
    );
}

#[cfg(unix)]
#[test]
fn creator_publishes_shared_mode_without_rewriting_existing_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("cross-uid.bitmap");
    drop(SubscriptionBitmap::open_or_create(&path, 128).expect("create shared bitmap"));
    assert_eq!(
        std::fs::metadata(&path)
            .expect("stat shared bitmap")
            .permissions()
            .mode()
            & 0o777,
        0o666
    );

    std::fs::set_permissions(&path, PermissionsExt::from_mode(0o600))
        .expect("set owner-only fixture mode");
    drop(SubscriptionBitmap::open_or_create(&path, 128).expect("reopen existing bitmap"));
    assert_eq!(
        std::fs::metadata(&path)
            .expect("stat reopened bitmap")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "non-creator opens must not chmod an inode they may not own"
    );
}
