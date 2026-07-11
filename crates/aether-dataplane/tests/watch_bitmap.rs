use std::path::{Path, PathBuf};

use aether_dataplane::{
    SubscriptionBitmap, automation_bitmap_path_from_shm, bitmap_path_for_consumer,
};

#[test]
fn bitmap_create_open_and_atomic_updates_roundtrip() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("alarm-subs.shm");
    {
        let bitmap = SubscriptionBitmap::create(&path).expect("create bitmap");
        bitmap.set_watched(42);
        bitmap.set_watched(99_999);
        assert_eq!(bitmap.subscription_count(), 2);
    }

    let reopened = SubscriptionBitmap::open(&path).expect("open bitmap");
    assert!(reopened.is_watched(42));
    assert!(reopened.is_watched(99_999));
    reopened.clear_watched(42);
    assert!(!reopened.is_watched(42));
}

#[test]
fn open_or_create_preserves_a_live_consumers_subscriptions() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("subscriptions.bitmap");
    let consumer = SubscriptionBitmap::open_or_create(&path).expect("create bitmap");
    consumer.set_watched(73);

    let writer_restart = SubscriptionBitmap::open_or_create(&path).expect("reuse existing bitmap");

    assert!(consumer.is_watched(73));
    assert!(writer_restart.is_watched(73));
    assert_eq!(writer_restart.subscription_count(), 1);
}

#[test]
fn each_event_consumer_gets_an_independent_bitmap_path() {
    let main = Path::new("/dev/shm/aether-rtdb.shm");

    assert_eq!(
        automation_bitmap_path_from_shm(main),
        PathBuf::from("/dev/shm/aether-rtdb-point-watch-subs-automation.shm")
    );
    assert_eq!(
        bitmap_path_for_consumer(main, "alarm"),
        PathBuf::from("/dev/shm/aether-rtdb-point-watch-subs-alarm.shm")
    );
}
