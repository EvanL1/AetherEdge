use super::*;
use aether_domain::{ChannelCommandAddress as DomainAddress, ChannelId, CommandId, PointId};
use std::future::Future;
use std::io::Write;
use std::task::Poll;
use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;

fn test_command(id: u128, value: f64) -> PhysicalDeviceCommand {
    let now = system_time_ms();
    PhysicalDeviceCommand::new(
        CommandId::new(id),
        DomainAddress::new(ChannelId::new(7), PointKind::Action, PointId::new(9))
            .expect("action target"),
        value,
        TimestampMs::new(now),
        TimestampMs::new(now + 10_000),
    )
    .expect("valid test command")
}

#[tokio::test]
async fn cancelling_blocked_notify_reconnects_before_next_complete_frame() {
    let dir = tempfile::tempdir().expect("temporary UDS directory");
    let path = dir.path().join("command.sock");
    let listener = UnixListener::bind(&path).expect("bind command listener");
    let mut notifier = CommandNotifier::connect(&path).await;
    let (mut original_peer, _) = listener.accept().await.expect("accept original stream");

    // Fill the real socket's send buffer while its peer does not read, so
    // notify's write_all is deterministically Pending when cancelled.
    let mut stream = notifier
        .stream
        .take()
        .expect("connected notifier")
        .into_std()
        .expect("standard nonblocking socket");
    let filler = [0xA5; 8192];
    let mut filled = 0;
    // Finish with single-byte writes so platforms with size-dependent socket
    // allocation cannot leave enough room for the smaller command frame.
    for chunk in [&filler[..], &filler[..1]] {
        loop {
            match stream.write(chunk) {
                Ok(count) => {
                    assert!(count > 0);
                    filled += count;
                },
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("fill send buffer: {error}"),
            }
        }
    }
    assert!(filled > 0);
    notifier.stream = Some(UnixStream::from_std(stream).expect("restore async socket"));
    let mut pending = Box::pin(notifier.notify(test_command(1, 11.0)));
    std::future::poll_fn(|context| {
        assert!(
            pending.as_mut().poll(context).is_pending(),
            "saturated write must block"
        );
        Poll::Ready(())
    })
    .await;
    drop(pending);

    let mut old_bytes = Vec::new();
    let closed = tokio::time::timeout(
        Duration::from_secs(1),
        original_peer.read_to_end(&mut old_bytes),
    )
    .await;
    assert!(
        closed.is_ok(),
        "cancellation must close the stream rather than retain an incomplete frame"
    );
    closed
        .expect("old stream closed")
        .expect("drain old stream");
    assert_eq!(old_bytes.len(), filled);

    let command = test_command(2, 22.0);
    assert!(
        notifier.notify(command).await.is_ok(),
        "next command reconnects"
    );
    let (mut fresh_peer, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
        .await
        .expect("fresh connection deadline")
        .expect("accept fresh stream");
    let mut bytes = [0; DeviceCommandFrame::SIZE];
    fresh_peer
        .read_exact(&mut bytes)
        .await
        .expect("complete fresh frame");
    let frame = DeviceCommandFrame::from_bytes(&bytes);
    assert_eq!(frame.channel_id(), 7);
    assert_eq!(frame.point_id(), 9);
    assert_eq!(frame.point_kind(), Some(PointKind::Action));
    assert_eq!(frame.value(), 22.0);
    assert_eq!(frame.timestamp_ms(), command.issued_at().get());
    assert_eq!(frame.expires_at_ms(), command.expires_at().get());
    assert_eq!(frame.sequence(), 2, "cancelled sequence must not be reused");

    // A successful frame restores the connection for the next normal write.
    assert!(notifier.notify(test_command(3, 33.0)).await.is_ok());
    tokio::time::timeout(Duration::from_secs(1), fresh_peer.read_exact(&mut bytes))
        .await
        .expect("reused stream deadline")
        .expect("complete reused frame");
    let frame = DeviceCommandFrame::from_bytes(&bytes);
    assert_eq!(frame.value(), 33.0);
    assert_eq!(frame.sequence(), 3);
}
