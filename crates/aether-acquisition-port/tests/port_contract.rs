use std::sync::Arc;

use aether_acquisition_port::AcquisitionStateWriter;

#[test]
fn acquisition_writer_remains_an_object_safe_owner_capability() {
    fn accepts_writer(_: Option<Arc<dyn AcquisitionStateWriter>>) {}

    accepts_writer(None);
}
