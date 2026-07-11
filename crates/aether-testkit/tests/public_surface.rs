use aether_testkit::{assert_live_state_round_trip, assert_outbox_fifo};

#[test]
fn conformance_helpers_are_public() {
    let _live_state_check = assert_live_state_round_trip;
    let _outbox_check = assert_outbox_fifo;
}
