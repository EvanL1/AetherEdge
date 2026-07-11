//! Reusable conformance checks for extension authors.

use aether_domain::PointSample;
use aether_ports::{
    DurableOutbox, LiveState, LiveStateWriter, OutboxMessage, PortError, PortErrorKind, PortResult,
};

/// Verifies the required read/write and ordered batch behavior of `LiveState`.
pub async fn assert_live_state_round_trip(
    reader: &dyn LiveState,
    writer: &dyn LiveStateWriter,
    first: PointSample,
    second: PointSample,
) -> PortResult<()> {
    writer.write(first).await?;
    writer.write(second).await?;

    if reader.read(first.address()).await? != Some(first) {
        return Err(contract_error("live-state single read did not round trip"));
    }

    let actual = reader
        .read_many(&[second.address(), first.address()])
        .await?;
    if actual != vec![Some(second), Some(first)] {
        return Err(contract_error(
            "live-state batch read did not preserve input order",
        ));
    }

    Ok(())
}

/// Verifies FIFO visibility and acknowledgement behavior of `DurableOutbox`.
pub async fn assert_outbox_fifo(
    outbox: &dyn DurableOutbox,
    first: OutboxMessage,
    second: OutboxMessage,
) -> PortResult<()> {
    let first_id = outbox.enqueue(first).await?;
    let second_id = outbox.enqueue(second).await?;
    let pending = outbox.peek(2).await?;

    if pending.len() != 2 || pending[0].id() != first_id || pending[1].id() != second_id {
        return Err(contract_error(
            "outbox did not expose entries in FIFO order",
        ));
    }

    if outbox.acknowledge(&[first_id]).await? != 1 {
        return Err(contract_error("outbox did not acknowledge the first entry"));
    }

    let remaining = outbox.peek(2).await?;
    if remaining.len() != 1 || remaining[0].id() != second_id {
        return Err(contract_error(
            "outbox acknowledgement removed the wrong entry",
        ));
    }

    Ok(())
}

fn contract_error(message: &str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}
