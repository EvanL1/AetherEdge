use aether_testkit::{
    ScriptedDataProcessor, assert_data_processor_correlation, assert_history_query_bounded,
    assert_history_query_provenance, assert_live_state_round_trip, assert_outbox_fifo,
};

#[test]
fn conformance_helpers_are_public() {
    let _live_state_check = assert_live_state_round_trip;
    let _outbox_check = assert_outbox_fifo;
    let _data_processor_check = assert_data_processor_correlation;
    let _bounded_history_check = assert_history_query_bounded;
    let _history_provenance_check = assert_history_query_provenance;
    let _scripted_processor_type = core::mem::size_of::<ScriptedDataProcessor>();
}
