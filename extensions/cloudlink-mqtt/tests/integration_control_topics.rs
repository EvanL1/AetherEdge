#![cfg(feature = "integration-control")]

use aether_cloudlink_mqtt::{IntegrationControlTopicNamespace, TopicNamespace};

const GATEWAY_ID: &str = "33333333-3333-4333-8333-333333333333";

#[test]
fn control_topics_are_exact_and_absent_from_the_read_only_baseline() {
    let control =
        IntegrationControlTopicNamespace::new("aether", GATEWAY_ID).expect("control namespace");
    assert_eq!(
        control.offer_topic(),
        "aether/v1/gateways/33333333-3333-4333-8333-333333333333/down/integration-control"
    );
    assert_eq!(
        control.receipt_topic(),
        "aether/v1/gateways/33333333-3333-4333-8333-333333333333/up/integration-control/receipts"
    );

    let baseline = TopicNamespace::new("aether", GATEWAY_ID).expect("baseline");
    assert!(!baseline.subscribe_topics().contains(&control.offer_topic()));
    assert!(!baseline.publish_topics().contains(&control.receipt_topic()));
}

#[test]
fn control_namespace_rejects_wildcards_and_noncanonical_gateway_ids() {
    for (prefix, gateway_id) in [
        ("aether/#", GATEWAY_ID),
        ("aether", "33333333-3333-4333-C333-333333333333"),
        ("", GATEWAY_ID),
    ] {
        assert!(IntegrationControlTopicNamespace::new(prefix, gateway_id).is_err());
    }
}
